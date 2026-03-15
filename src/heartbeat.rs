use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::api::client::CoordinatorClient;
use crate::types::NodeHeartbeatRequest;

const HEARTBEAT_INTERVAL_SECS: u64 = 60;

/// Shared counter for active matches, updated by the assignment loop.
pub type ActiveMatchCount = Arc<AtomicU32>;

/// Run the heartbeat loop — sends metrics to coordinator every 60 seconds.
pub async fn heartbeat_loop(
    client: Arc<CoordinatorClient>,
    active_matches: ActiveMatchCount,
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
                let metrics = collect_metrics(&active_matches);
                match client.heartbeat(&metrics).await {
                    Ok(()) => info!(
                        active_matches = metrics.active_matches,
                        cpu = format!("{:.1}%", metrics.cpu_usage_percent),
                        mem = format!("{:.1}%", metrics.memory_usage_percent),
                        "Heartbeat sent"
                    ),
                    Err(e) => warn!(error = %e, "Heartbeat failed"),
                }
            }
        }
    }
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
}
