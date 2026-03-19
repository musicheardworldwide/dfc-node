use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::api::client::CoordinatorClient;
use crate::auth::token::TokenStore;
use crate::auth::wallet::SigningKey;
use crate::types::{NodeHardwareSpecs, NodeHeartbeatRequest, NodeRegistrationRequest};

const HEARTBEAT_INTERVAL_SECS: u64 = 60;

/// Shared counter for active matches, updated by the assignment loop.
pub type ActiveMatchCount = Arc<AtomicU32>;

/// Configuration for re-authentication (passed to heartbeat loop).
pub struct ReauthConfig {
    pub wallet_address: String,
    pub keypair: SigningKey,
    pub hardware: NodeHardwareSpecs,
    pub region: String,
    pub public_ip: String,
    pub stake_amount: String,
}

/// Run the heartbeat loop — sends metrics to coordinator every 60 seconds.
/// Before each heartbeat, checks if the token is near expiry and refreshes it.
pub async fn heartbeat_loop(
    client: Arc<CoordinatorClient>,
    active_matches: ActiveMatchCount,
    token_store: TokenStore,
    reauth: ReauthConfig,
    cancel: CancellationToken,
) {
    info!("Heartbeat loop started (interval: {}s)", HEARTBEAT_INTERVAL_SECS);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("Heartbeat loop shutting down");
                return;
            }
            _ = tokio::time::sleep(Duration::from_secs(HEARTBEAT_INTERVAL_SECS)) => {
                // P0-1: Check token expiry before heartbeat; refresh if needed
                if token_store.needs_refresh().await {
                    let remaining = token_store.seconds_until_expiry().await;
                    warn!(
                        remaining_secs = remaining,
                        "Token near expiry — refreshing before heartbeat"
                    );
                    match refresh_token(&client, &token_store, &reauth).await {
                        Ok(()) => info!("Token refreshed successfully"),
                        Err(e) => {
                            error!(error = %e, "Token refresh failed — will retry next heartbeat");
                            // Continue anyway; let the heartbeat attempt reveal the auth state
                        }
                    }
                }

                let metrics = collect_metrics(&active_matches);
                match client.heartbeat(&metrics).await {
                    Ok(()) => info!(
                        active_matches = metrics.active_matches,
                        cpu = format!("{:.1}%", metrics.cpu_usage_percent),
                        mem = format!("{:.1}%", metrics.memory_usage_percent),
                        "Heartbeat sent"
                    ),
                    Err(e) if crate::api::client::is_unauthorized(&e) => {
                        // P1-2: 401 on heartbeat — force a full re-auth
                        warn!("Heartbeat received 401 — forcing token refresh");
                        match refresh_token(&client, &token_store, &reauth).await {
                            Ok(()) => info!("Token refreshed after 401"),
                            Err(re) => error!(error = %re, "Re-authentication after 401 failed"),
                        }
                    }
                    Err(e) => warn!(error = %e, "Heartbeat failed"),
                }
            }
        }
    }
}

/// Re-authenticate with the coordinator: nonce → sign → register → store token.
pub async fn refresh_token(
    client: &CoordinatorClient,
    token_store: &TokenStore,
    reauth: &ReauthConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use crate::auth::wallet;

    let nonce_resp = client.request_nonce(&reauth.wallet_address).await?;
    let signature = wallet::sign_message(&reauth.keypair, &nonce_resp.message);

    let reg_resp = client
        .register(&NodeRegistrationRequest {
            wallet_address: reauth.wallet_address.clone(),
            signature,
            hardware: reauth.hardware.clone(),
            region: reauth.region.clone(),
            public_ip: reauth.public_ip.clone(),
            stake_amount: reauth.stake_amount.clone(),
        })
        .await?;

    token_store.set(reg_resp.token, reg_resp.node_id).await;
    info!(node_id = reg_resp.node_id, "Token refreshed — new node registration");
    Ok(())
}

fn collect_metrics(active_matches: &ActiveMatchCount) -> NodeHeartbeatRequest {
    // Read /proc/stat for CPU, /proc/meminfo for memory, statvfs for disk
    // For now, use sensible defaults — real metrics in a future pass
    let cpu = read_cpu_usage().unwrap_or(0.0);
    let mem = read_memory_usage().unwrap_or(0.0);
    let disk = read_disk_usage().unwrap_or(0.0);

    NodeHeartbeatRequest {
        active_matches: active_matches.load(Ordering::Relaxed),
        cpu_usage_percent: cpu,
        memory_usage_percent: mem,
        disk_usage_percent: disk,
    }
}

fn read_cpu_usage() -> Option<f64> {
    // Simple /proc/loadavg based estimate
    let loadavg = std::fs::read_to_string("/proc/loadavg").ok()?;
    let load1: f64 = loadavg.split_whitespace().next()?.parse().ok()?;
    let cores = num_cpus();
    Some((load1 / cores as f64 * 100.0).min(100.0))
}

fn read_memory_usage() -> Option<f64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let mut total: u64 = 0;
    let mut available: u64 = 0;
    for line in meminfo.lines() {
        if line.starts_with("MemTotal:") {
            total = parse_meminfo_kb(line)?;
        } else if line.starts_with("MemAvailable:") {
            available = parse_meminfo_kb(line)?;
        }
    }
    if total == 0 {
        return None;
    }
    Some(((total - available) as f64 / total as f64) * 100.0)
}

fn parse_meminfo_kb(line: &str) -> Option<u64> {
    line.split_whitespace().nth(1)?.parse().ok()
}

fn read_disk_usage() -> Option<f64> {
    // Use statvfs on /var/lib/docker (or /)
    // Simplified: read from df output
    None // Will be implemented with libc::statvfs
}

fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_metrics_returns_valid_values() {
        let count = Arc::new(AtomicU32::new(2));
        let metrics = collect_metrics(&count);
        assert_eq!(metrics.active_matches, 2);
        assert!(metrics.cpu_usage_percent >= 0.0);
        assert!(metrics.memory_usage_percent >= 0.0);
    }

    /// Test that needs_refresh is properly detected before heartbeat.
    /// This is an integration-style test for the token refresh path.
    #[tokio::test]
    async fn token_refresh_triggered_when_near_expiry() {
        use crate::auth::token::{TokenStore, REFRESH_THRESHOLD_SECS};

        let store = TokenStore::new();
        // Token with TTL shorter than REFRESH_THRESHOLD — should trigger refresh
        store
            .set_with_ttl("expiring-token".to_string(), 99, REFRESH_THRESHOLD_SECS - 10)
            .await;
        assert!(store.needs_refresh().await, "Expected needs_refresh=true for near-expiry token");
    }

    #[tokio::test]
    async fn token_refresh_not_triggered_when_fresh() {
        use crate::auth::token::{TokenStore, REFRESH_THRESHOLD_SECS};

        let store = TokenStore::new();
        // Token with long TTL — should NOT trigger refresh
        store
            .set_with_ttl("fresh-token".to_string(), 99, REFRESH_THRESHOLD_SECS * 10)
            .await;
        assert!(
            !store.needs_refresh().await,
            "Expected needs_refresh=false for fresh token"
        );
    }

    #[tokio::test]
    async fn refresh_token_stores_new_token() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        // Mock nonce endpoint
        Mock::given(method("POST"))
            .and(path("/api/nodes/auth/nonce"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "nonce": "abc123",
                    "message": "Sign this: abc123",
                    "expiresAt": "2099-01-01T00:00:00Z"
                })),
            )
            .mount(&mock_server)
            .await;

        // Mock register endpoint
        Mock::given(method("POST"))
            .and(path("/api/nodes/register"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "nodeId": 99,
                    "token": "new-refreshed-token",
                    "status": "active"
                })),
            )
            .mount(&mock_server)
            .await;

        let token_store = TokenStore::new();
        token_store.set("old-token".to_string(), 1).await;

        let client = Arc::new(CoordinatorClient::new(
            &mock_server.uri(),
            token_store.clone(),
        ));

        let keypair = ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng);
        let reauth = ReauthConfig {
            wallet_address: "test-wallet".to_string(),
            keypair,
            hardware: NodeHardwareSpecs {
                cpu_cores: 4,
                memory_gb: 8,
                storage_gb: 100,
                gpu_model: None,
            },
            region: "us-west".to_string(),
            public_ip: "1.2.3.4".to_string(),
            stake_amount: "0".to_string(),
        };

        refresh_token(&client, &token_store, &reauth).await.unwrap();

        assert_eq!(
            token_store.get().await.unwrap(),
            "new-refreshed-token",
            "Token store should hold the new token after refresh"
        );
        assert_eq!(token_store.node_id().await.unwrap(), 99);
    }
}
