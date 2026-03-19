// Mirror of @dfc/common node-types.ts — hand-translated to Rust.

use serde::{Deserialize, Serialize};

// ─── Hardware ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeHardwareSpecs {
    pub cpu_cores: u32,
    pub memory_gb: u32,
    pub storage_gb: u32,
    pub gpu_model: Option<String>,
}

// ─── Registration ────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NonceRequest {
    pub wallet_address: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NonceResponse {
    pub nonce: String,
    pub message: String,
    #[serde(default)]
    pub expires_at: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeRegistrationRequest {
    pub wallet_address: String,
    pub signature: String,
    pub hardware: NodeHardwareSpecs,
    pub region: String,
    pub public_ip: String,
    pub stake_amount: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeRegistrationResponse {
    pub node_id: u64,
    pub token: String,
    pub status: String,
}

// ─── Heartbeat ───────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeHeartbeatRequest {
    pub active_matches: u32,
    pub cpu_usage_percent: f64,
    pub memory_usage_percent: f64,
    pub disk_usage_percent: f64,
}

// ─── Assignments ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchAssignmentConfig {
    pub match_id: u64,
    pub player1_id: u64,
    pub player2_id: u64,
    pub game_mode: String,
    pub wager_amount: String,
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeAssignment {
    pub id: u64,
    pub match_id: u64,
    pub status: String,
    pub assigned_at: String,
    pub expires_at: String,
    pub match_config: MatchAssignmentConfig,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssignmentResponse {
    pub assignments: Vec<NodeAssignment>,
}

// ─── Match Report ────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeMatchReport {
    pub match_id: u64,
    pub winner_id: Option<u64>,
    pub end_reason: String,
    pub duration_seconds: u64,
    pub recording_ref: Option<String>,
    pub container_logs: Option<String>,
}
