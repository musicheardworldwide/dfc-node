use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::api::client::CoordinatorClient;
use crate::docker::manager::DockerManager;
use crate::heartbeat::ActiveMatchCount;
use crate::types::{NodeAssignment, NodeMatchReport};

const LONG_POLL_TIMEOUT_SECS: u32 = 15;
const RETRY_DELAY_SECS: u64 = 5;

/// Run the assignment loop — long-polls for match assignments, spawns containers.
pub async fn assignment_loop(
    client: Arc<CoordinatorClient>,
    docker: Arc<DockerManager>,
    active_matches: ActiveMatchCount,
    max_concurrent: u32,
    cancel: CancellationToken,
) {
    info!("Assignment loop started (max_concurrent: {})", max_concurrent);

    loop {
        if cancel.is_cancelled() {
            info!("Assignment loop shutting down");
            return;
        }

        let current = active_matches.load(Ordering::Relaxed);
        if current >= max_concurrent {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_secs(5)) => continue,
            }
        }

        match client.poll_assignments(LONG_POLL_TIMEOUT_SECS).await {
            Ok(assignments) if assignments.is_empty() => {
                // No assignments — loop back to long-poll
                continue;
            }
            Ok(assignments) => {
                for assignment in assignments {
                    let client = Arc::clone(&client);
                    let docker = Arc::clone(&docker);
                    let active = Arc::clone(&active_matches);
                    let cancel = cancel.clone();

                    tokio::spawn(async move {
                        execute_match(client, docker, active, assignment, cancel).await;
                    });
                }
            }
            Err(e) => {
                warn!(error = %e, "Failed to poll assignments");
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = tokio::time::sleep(Duration::from_secs(RETRY_DELAY_SECS)) => {}
                }
            }
        }
    }
}

/// Execute a single match: spawn containers, monitor, report result.
async fn execute_match(
    client: Arc<CoordinatorClient>,
    docker: Arc<DockerManager>,
    active_matches: ActiveMatchCount,
    assignment: NodeAssignment,
    cancel: CancellationToken,
) {
    let match_id = assignment.match_id;
    let config = &assignment.match_config;

    info!(
        match_id,
        game_mode = %config.game_mode,
        timeout = config.timeout_seconds,
        "Executing match"
    );

    active_matches.fetch_add(1, Ordering::Relaxed);
    let start = Instant::now();

    let mut container_ids: Vec<String> = Vec::new();

    let result = async {
        // Spawn container 1
        let env1 = build_env(match_id, 1, config.player1_id);
        let c1 = docker.spawn_gladiator(match_id, 1, env1).await?;
        container_ids.push(c1.container_id.clone());

        // Spawn container 2
        let env2 = build_env(match_id, 2, config.player2_id);
        let c2 = docker.spawn_gladiator(match_id, 2, env2).await?;
        container_ids.push(c2.container_id.clone());

        info!(match_id, c1_port = c1.ssh_port, c2_port = c2.ssh_port, "Both containers spawned");

        // Monitor containers until one exits or timeout
        let timeout = Duration::from_secs(config.timeout_seconds);
        let (winner_id, end_reason) = monitor_match(
            &docker,
            &c1.container_id,
            &c2.container_id,
            config.player1_id,
            config.player2_id,
            timeout,
            &cancel,
        )
        .await;

        Ok::<_, Box<dyn std::error::Error + Send + Sync>>((winner_id, end_reason))
    }
    .await;

    let duration = start.elapsed().as_secs();

    // Cleanup containers
    for id in &container_ids {
        docker.kill_container(id).await;
    }

    active_matches.fetch_sub(1, Ordering::Relaxed);

    // Report result
    let (winner_id, end_reason) = match result {
        Ok((w, r)) => (w, r),
        Err(e) => {
            error!(match_id, error = %e, "Match execution failed");
            (None, "error".to_string())
        }
    };

    let report = NodeMatchReport {
        match_id,
        winner_id,
        end_reason,
        duration_seconds: duration,
        recording_ref: None,
        container_logs: None,
    };

    match client.report_match(&report).await {
        Ok(()) => info!(match_id, "Match result reported"),
        Err(e) => error!(match_id, error = %e, "Failed to report match result"),
    }
}

/// Monitor two containers — return when one exits or timeout is reached.
async fn monitor_match(
    docker: &DockerManager,
    c1_id: &str,
    c2_id: &str,
    player1_id: u64,
    player2_id: u64,
    timeout: Duration,
    cancel: &CancellationToken,
) -> (Option<u64>, String) {
    let poll_interval = Duration::from_secs(5);
    let deadline = Instant::now() + timeout;

    loop {
        if cancel.is_cancelled() {
            return (None, "node_shutdown".to_string());
        }

        if Instant::now() >= deadline {
            return (None, "timeout".to_string());
        }

        let c1_running = docker.is_running(c1_id).await;
        let c2_running = docker.is_running(c2_id).await;

        match (c1_running, c2_running) {
            (true, true) => {
                // Both alive, keep polling
            }
            (false, true) => {
                return (Some(player2_id), "objective_complete".to_string());
            }
            (true, false) => {
                return (Some(player1_id), "objective_complete".to_string());
            }
            (false, false) => {
                return (None, "error".to_string());
            }
        }

        tokio::select! {
            _ = cancel.cancelled() => return (None, "node_shutdown".to_string()),
            _ = tokio::time::sleep(poll_interval) => {}
        }
    }
}

fn build_env(match_id: u64, slot: u8, player_id: u64) -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert("DFC_MATCH_ID".to_string(), match_id.to_string());
    env.insert("DFC_PLAYER_SLOT".to_string(), slot.to_string());
    env.insert("DFC_PLAYER_ID".to_string(), player_id.to_string());
    env
}
