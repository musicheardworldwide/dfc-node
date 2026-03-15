use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub coordinator: CoordinatorConfig,
    pub wallet: WalletConfig,
    #[serde(default)]
    pub docker: DockerConfig,
    #[serde(default)]
    pub hardware: HardwareConfig,
}

#[derive(Debug, Deserialize)]
pub struct CoordinatorConfig {
    pub url: String,
}

#[derive(Debug, Deserialize)]
pub struct WalletConfig {
    pub keypair_path: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DockerConfig {
    #[serde(default = "default_image")]
    pub image: String,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: u32,
    #[serde(default = "default_memory_limit")]
    pub memory_limit: u64,
    #[serde(default = "default_cpu_limit")]
    pub cpu_limit: u64,
}

#[derive(Debug, Deserialize)]
pub struct HardwareConfig {
    #[serde(default)]
    pub gpu: bool,
    #[serde(default = "default_cpu_cores")]
    pub cpu_cores: u32,
    #[serde(default = "default_memory_gb")]
    pub memory_gb: u32,
}

impl Default for DockerConfig {
    fn default() -> Self {
        Self {
            image: default_image(),
            max_concurrent: default_max_concurrent(),
            memory_limit: default_memory_limit(),
            cpu_limit: default_cpu_limit(),
        }
    }
}

impl Default for HardwareConfig {
    fn default() -> Self {
        Self {
            gpu: false,
            cpu_cores: default_cpu_cores(),
            memory_gb: default_memory_gb(),
        }
    }
}

fn default_image() -> String {
    "dfc-gladiator:armed".to_string()
}
fn default_max_concurrent() -> u32 {
    2
}
fn default_memory_limit() -> u64 {
    2 * 1024 * 1024 * 1024
}
fn default_cpu_limit() -> u64 {
    2_000_000_000
}
fn default_cpu_cores() -> u32 {
    4
}
fn default_memory_gb() -> u32 {
    16
}

/// Load config from TOML file with environment variable overrides.
pub fn load_config(path: &str) -> Result<Config, Box<dyn std::error::Error + Send + Sync>> {
    let contents = std::fs::read_to_string(path)?;
    let mut config: Config = toml::from_str(&contents)?;

    // Environment variable overrides
    if let Ok(url) = std::env::var("DFC_COORDINATOR_URL") {
        config.coordinator.url = url;
    }
    if let Ok(path) = std::env::var("DFC_WALLET_KEYPAIR") {
        config.wallet.keypair_path = PathBuf::from(path);
    }
    if let Ok(image) = std::env::var("DFC_DOCKER_IMAGE") {
        config.docker.image = image;
    }

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_config() {
        let toml = r#"
            [coordinator]
            url = "https://api.dfc.gg"
            [wallet]
            keypair_path = "/home/user/.config/solana/id.json"
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.coordinator.url, "https://api.dfc.gg");
        assert_eq!(config.docker.max_concurrent, 2);
        assert_eq!(config.hardware.cpu_cores, 4);
    }

    #[test]
    fn parse_full_config() {
        let toml = r#"
            [coordinator]
            url = "http://localhost:3001"
            [wallet]
            keypair_path = "./test-key.json"
            [docker]
            image = "dfc-gladiator:test"
            max_concurrent = 4
            memory_limit = 4294967296
            cpu_limit = 4000000000
            [hardware]
            gpu = true
            cpu_cores = 8
            memory_gb = 32
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.docker.image, "dfc-gladiator:test");
        assert_eq!(config.docker.max_concurrent, 4);
        assert!(config.hardware.gpu);
        assert_eq!(config.hardware.cpu_cores, 8);
    }
}
