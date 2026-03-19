use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::api::client::{is_unauthorized, CoordinatorClient};
use crate::auth::token::TokenStore;
use crate::docker::manager::DockerManager;
use crate::heartbeat::{refresh_token, ActiveMatchCount, ReauthConfig};
use crate::types::{NodeAssignment, NodeMatchReport};

const LONG_POLL_TIMEOUT_SECS: u32 = 15;
const RETRY_DELAY_SECS: u64 = 5;

/// Run the assignment loop — long-polls for match assignments, spawns containers.
pub async fn assignment_loop(
    client: Arc<CoordinatorClient>,
    docker: Arc<DockerManager>,
    active_matches: ActiveMatchCount,
    max_concurrent: u32,
    node_id: u64,
    token_store: TokenStore,
    reauth: Arc<ReauthConfig>,
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

        match client.poll_assignments(node_id, LONG_POLL_TIMEOUT_SECS).await {
            Ok(assignments) if assignments.is_empty() => {
                // No assignments — loop back to long-poll
                continue;
            }
            Ok(assignments) => {
                // P0-2: Atomically reserve capacity BEFORE spawning.
                // Calculate how many slots we can actually fill right now.
                let available = max_concurrent.saturating_sub(active_matches.load(Ordering::Relaxed));
                let to_spawn: Vec<NodeAssignment> = assignments
                    .into_iter()
                    .take(available as usize)
                    .collect();

                for assignment in to_spawn {
                    // P0-2: Atomically check-and-increment BEFORE spawning the task.
                    // fetch_add returns the previous value; if it was already at or above
                    // max_concurrent, undo the increment and stop.
                    let prev = active_matches.fetch_add(1, Ordering::AcqRel);
                    if prev >= max_concurrent {
                        // We over-committed — undo and stop
                        active_matches.fetch_sub(1, Ordering::AcqRel);
                        warn!(
                            prev,
                            max_concurrent,
                            "Concurrent limit reached during spawn; skipping remaining assignments"
                        );
                        break;
                    }

                    let client = Arc::clone(&client);
                    let docker = Arc::clone(&docker);
                    let active = Arc::clone(&active_matches);
                    let cancel = cancel.clone();

                    tokio::spawn(async move {
                        // Counter was already incremented above; execute_match decrements on exit.
                        execute_match_inner(client, docker, active, node_id, assignment, cancel).await;
                    });
                }
            }
            Err(e) if is_unauthorized(&e) => {
                // P1-2: 401/403 on poll — trigger re-auth once, then retry
                warn!("poll_assignments received 401/403 — triggering token refresh");
                match refresh_token(&client, &token_store, &reauth).await {
                    Ok(()) => info!("Token refreshed after 401 on poll_assignments"),
                    Err(re) => {
                        error!(error = %re, "Re-authentication failed after 401 on poll_assignments");
                        tokio::select! {
                            _ = cancel.cancelled() => return,
                            _ = tokio::time::sleep(Duration::from_secs(RETRY_DELAY_SECS)) => {}
                        }
                    }
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
/// NOTE: The caller has already incremented active_matches; this function is responsible
/// for decrementing it before returning.
async fn execute_match_inner(
    client: Arc<CoordinatorClient>,
    docker: Arc<DockerManager>,
    active_matches: ActiveMatchCount,
    node_id: u64,
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

    let start = Instant::now();
    let mut container_ids: Vec<String> = Vec::new();

    let result = async {
        // P1-1: Pass full match config to containers
        let env1 = build_env(match_id, 1, config.player1_id, config);
        let c1 = docker.spawn_gladiator(match_id, 1, env1).await?;
        container_ids.push(c1.container_id.clone());

        let env2 = build_env(match_id, 2, config.player2_id, config);
        let c2 = docker.spawn_gladiator(match_id, 2, env2).await?;
        container_ids.push(c2.container_id.clone());

        info!(match_id, c1_port = c1.ssh_port, c2_port = c2.ssh_port, "Both containers spawned");

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

    // Decrement counter now that match is complete
    active_matches.fetch_sub(1, Ordering::AcqRel);

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

    match client.report_match(node_id, &report).await {
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
                // Both exited simultaneously — treat as draw, not error
                return (None, "draw".to_string());
            }
        }

        tokio::select! {
            _ = cancel.cancelled() => return (None, "node_shutdown".to_string()),
            _ = tokio::time::sleep(poll_interval) => {}
        }
    }
}

/// P1-1: Build environment variables for a gladiator container.
/// Passes match_id, player slot/id, AND full match config (game_mode, wager, timeout).
pub fn build_env(
    match_id: u64,
    slot: u8,
    player_id: u64,
    config: &crate::types::MatchAssignmentConfig,
) -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert("DFC_MATCH_ID".to_string(), match_id.to_string());
    env.insert("DFC_PLAYER_SLOT".to_string(), slot.to_string());
    env.insert("DFC_PLAYER_ID".to_string(), player_id.to_string());
    // P1-1: Previously missing fields
    env.insert("DFC_GAME_MODE".to_string(), config.game_mode.clone());
    env.insert("DFC_WAGER_AMOUNT".to_string(), config.wager_amount.clone());
    env.insert("DFC_TIMEOUT".to_string(), config.timeout_seconds.to_string());
    env
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MatchAssignmentConfig;
    use std::sync::atomic::AtomicU32;

    fn test_match_config() -> MatchAssignmentConfig {
        MatchAssignmentConfig {
            match_id: 42,
            player1_id: 1,
            player2_id: 2,
            game_mode: "ranked".to_string(),
            wager_amount: "100.0".to_string(),
            timeout_seconds: 300,
        }
    }

    // ─── P1-1: build_env tests ───────────────────────────────────────────────

    #[test]
    fn build_env_includes_all_required_fields() {
        let config = test_match_config();
        let env = build_env(42, 1, 100, &config);

        assert_eq!(env.get("DFC_MATCH_ID").unwrap(), "42");
        assert_eq!(env.get("DFC_PLAYER_SLOT").unwrap(), "1");
        assert_eq!(env.get("DFC_PLAYER_ID").unwrap(), "100");
        assert_eq!(env.get("DFC_GAME_MODE").unwrap(), "ranked");
        assert_eq!(env.get("DFC_WAGER_AMOUNT").unwrap(), "100.0");
        assert_eq!(env.get("DFC_TIMEOUT").unwrap(), "300");
    }

    #[test]
    fn build_env_player2_slot() {
        let config = test_match_config();
        let env = build_env(42, 2, 999, &config);

        assert_eq!(env.get("DFC_PLAYER_SLOT").unwrap(), "2");
        assert_eq!(env.get("DFC_PLAYER_ID").unwrap(), "999");
        // All match config fields still present
        assert!(env.contains_key("DFC_GAME_MODE"));
        assert!(env.contains_key("DFC_WAGER_AMOUNT"));
        assert!(env.contains_key("DFC_TIMEOUT"));
    }

    #[test]
    fn build_env_different_game_modes() {
        for mode in &["casual", "ranked", "tournament"] {
            let mut config = test_match_config();
            config.game_mode = mode.to_string();
            let env = build_env(1, 1, 1, &config);
            assert_eq!(env.get("DFC_GAME_MODE").unwrap().as_str(), *mode);
        }
    }

    // ─── P0-2: Concurrent match counter tests ───────────────────────────────

    #[test]
    fn counter_increments_before_spawn() {
        // Verify atomic increment logic: if we're AT max_concurrent, fetch_add
        // returns max_concurrent and we immediately undo it.
        let active = Arc::new(AtomicU32::new(2));
        let max_concurrent: u32 = 2;

        let prev = active.fetch_add(1, Ordering::AcqRel);
        if prev >= max_concurrent {
            active.fetch_sub(1, Ordering::AcqRel);
        }
        // Counter should be back to 2 — no over-commitment
        assert_eq!(active.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn counter_allows_spawn_when_below_limit() {
        let active = Arc::new(AtomicU32::new(1));
        let max_concurrent: u32 = 2;

        let prev = active.fetch_add(1, Ordering::AcqRel);
        let should_spawn = prev < max_concurrent;

        assert!(should_spawn, "Should allow spawn when prev=1 < max=2");
        // Counter is now 2 (incremented, not rolled back)
        assert_eq!(active.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn counter_blocks_spawn_when_at_limit() {
        let active = Arc::new(AtomicU32::new(2));
        let max_concurrent: u32 = 2;

        let prev = active.fetch_add(1, Ordering::AcqRel);
        let should_spawn = prev < max_concurrent;
        if !should_spawn {
            active.fetch_sub(1, Ordering::AcqRel);
        }

        assert!(!should_spawn, "Should block spawn when prev=2 >= max=2");
        // Counter rolled back to 2
        assert_eq!(active.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn concurrent_counter_no_race_under_concurrency() {
        // Simulate N goroutines all trying to increment past max_concurrent.
        // After all complete, counter must be <= max_concurrent.
        let max_concurrent: u32 = 3;
        let active = Arc::new(AtomicU32::new(0));
        let mut handles = vec![];

        for _ in 0..20 {
            let active = Arc::clone(&active);
            handles.push(tokio::spawn(async move {
                let prev = active.fetch_add(1, Ordering::AcqRel);
                if prev >= max_concurrent {
                    active.fetch_sub(1, Ordering::AcqRel);
                    false // Did not acquire slot
                } else {
                    // Simulate match duration
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    active.fetch_sub(1, Ordering::AcqRel);
                    true // Acquired and released slot
                }
            }));
        }

        // During execution, counter should never exceed max_concurrent
        // (We sample it here — not a perfect proof but validates the pattern)
        let sampled = active.load(Ordering::Relaxed);
        assert!(sampled <= max_concurrent + 1, "Counter {sampled} exceeded max {max_concurrent} (sampled mid-flight)");

        for h in handles {
            h.await.unwrap();
        }

        // After all done, counter should be 0
        assert_eq!(active.load(Ordering::Relaxed), 0, "Counter not back to 0 after all tasks");
    }
}
