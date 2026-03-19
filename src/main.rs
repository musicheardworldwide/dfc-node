mod api;
mod assignments;
#[allow(dead_code)]
mod auth;
mod config;
mod docker;
mod heartbeat;
mod shutdown;
#[allow(dead_code)]
mod types;

use std::sync::atomic::AtomicU32;
use std::sync::Arc;

use clap::Parser;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::api::client::CoordinatorClient;
use crate::auth::token::TokenStore;
use crate::auth::wallet;
use crate::config::load_config;
use crate::docker::manager::DockerManager;
use crate::heartbeat::ReauthConfig;
use crate::types::{NodeHardwareSpecs, NodeRegistrationRequest};

#[derive(Parser)]
#[command(name = "dfc-node", version, about = "DFC node operator — run gladiator matches on your hardware")]
struct Cli {
    /// Path to config file
    #[arg(short, long, default_value = "config.toml")]
    config: String,

    /// Beta mode — generate ephemeral wallet, skip real staking
    #[arg(long)]
    beta: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    info!("Loading config from {}", cli.config);
    let config = load_config(&cli.config)?;

    // P1-3: Warn if coordinator URL uses plain HTTP in non-beta mode
    if config.coordinator.url.starts_with("http://") && !cli.beta {
        warn!("╔══════════════════════════════════════════════════════════════╗");
        warn!("║  ⚠  COORDINATOR URL USES PLAIN HTTP — NOT SECURE!          ║");
        warn!("║  Bearer tokens and signed messages will be sent UNENCRYPTED ║");
        warn!("║  Use HTTPS in production: coordinator.url = \"https://...\"   ║");
        warn!("╚══════════════════════════════════════════════════════════════╝");
    }

    // Load wallet (or generate ephemeral one in beta mode)
    let keypair = if cli.beta {
        info!("BETA MODE — generating ephemeral wallet (no real stake required)");
        use ed25519_dalek::SigningKey;
        SigningKey::generate(&mut rand::rngs::OsRng)
    } else {
        info!("Loading wallet from {:?}", config.wallet.keypair_path);
        wallet::load_keypair(&config.wallet.keypair_path)?
    };
    let address = wallet::wallet_address(&keypair);
    info!(wallet = %address, "Wallet loaded{}", if cli.beta { " (ephemeral)" } else { "" });

    // Initialize Docker
    let docker_manager = DockerManager::new(config.docker.clone())?;
    docker_manager.ping().await?;
    docker_manager.ensure_image().await?;
    info!("Docker connection verified");

    // Initialize API client
    let token_store = TokenStore::new();
    let client = Arc::new(CoordinatorClient::new(
        &config.coordinator.url,
        token_store.clone(),
    ));

    // Register with coordinator
    info!("Registering with coordinator at {}", config.coordinator.url);
    let nonce_resp = client.request_nonce(&address).await?;
    let signature = wallet::sign_message(&keypair, &nonce_resp.message);

    let public_ip = detect_public_ip().await.unwrap_or_else(|| "unknown".to_string());
    let region = detect_region();
    let stake_amount = if cli.beta { "0.00000000".to_string() } else { "0".to_string() };
    let hardware = NodeHardwareSpecs {
        cpu_cores: config.hardware.cpu_cores,
        memory_gb: config.hardware.memory_gb,
        storage_gb: 100,
        gpu_model: if config.hardware.gpu {
            Some("unknown".to_string())
        } else {
            None
        },
    };

    let reg_resp = client
        .register(&NodeRegistrationRequest {
            wallet_address: address.clone(),
            signature,
            hardware: hardware.clone(),
            region: region.clone(),
            public_ip: public_ip.clone(),
            stake_amount: stake_amount.clone(),
        })
        .await?;

    token_store.set(reg_resp.token, reg_resp.node_id).await;
    info!(
        node_id = reg_resp.node_id,
        status = %reg_resp.status,
        "Registered with coordinator"
    );

    // Build re-auth config for token refresh (shared by heartbeat + assignment loops)
    let reauth = Arc::new(ReauthConfig {
        wallet_address: address.clone(),
        keypair,
        hardware,
        region,
        public_ip,
        stake_amount,
    });

    // Shared state
    let active_matches = Arc::new(AtomicU32::new(0));
    let cancel = CancellationToken::new();
    let docker = Arc::new(docker_manager);

    // Spawn tasks
    let heartbeat_handle = tokio::spawn(heartbeat::heartbeat_loop(
        Arc::clone(&client),
        Arc::clone(&active_matches),
        token_store.clone(),
        ReauthConfig {
            wallet_address: reauth.wallet_address.clone(),
            keypair: reauth.keypair.clone(),
            hardware: reauth.hardware.clone(),
            region: reauth.region.clone(),
            public_ip: reauth.public_ip.clone(),
            stake_amount: reauth.stake_amount.clone(),
        },
        cancel.clone(),
    ));

    let assignment_handle = tokio::spawn(assignments::assignment_loop(
        Arc::clone(&client),
        Arc::clone(&docker),
        Arc::clone(&active_matches),
        config.docker.max_concurrent,
        token_store.clone(),
        Arc::clone(&reauth),
        cancel.clone(),
    ));

    let signal_handle = tokio::spawn(shutdown::wait_for_signal(cancel.clone()));

    info!("DFC node running. Waiting for match assignments...");

    // Wait for shutdown signal
    signal_handle.await?;

    info!("Waiting for tasks to finish...");
    let _ = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let _ = heartbeat_handle.await;
        let _ = assignment_handle.await;
    })
    .await;

    info!("DFC node stopped");
    Ok(())
}

fn detect_region() -> String {
    std::env::var("DFC_REGION").unwrap_or_else(|_| "unknown".to_string())
}

async fn detect_public_ip() -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .ok()?;
    let resp = client.get("https://api.ipify.org").send().await.ok()?;
    resp.text().await.ok()
}
