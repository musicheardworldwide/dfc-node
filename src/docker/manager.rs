use bollard::container::{
    Config as ContainerConfig, CreateContainerOptions, RemoveContainerOptions,
    StartContainerOptions, StopContainerOptions,
};
use bollard::models::HostConfig;
use bollard::Docker;
use std::collections::HashMap;
use tracing::{info, warn};

use crate::config::DockerConfig;

/// Manages gladiator container lifecycle for match execution.
pub struct DockerManager {
    docker: Docker,
    config: DockerConfig,
}

pub struct SpawnedContainer {
    pub container_id: String,
    pub ssh_port: u16,
}

impl DockerManager {
    pub fn new(config: DockerConfig) -> Result<Self, bollard::errors::Error> {
        let docker = Docker::connect_with_defaults()?;
        Ok(Self { docker, config })
    }

    /// Check if Docker daemon is reachable.
    pub async fn ping(&self) -> Result<(), bollard::errors::Error> {
        self.docker.ping().await?;
        Ok(())
    }

    /// Pull the gladiator image if not present.
    pub async fn ensure_image(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        match self.docker.inspect_image(&self.config.image).await {
            Ok(_) => {
                info!(image = %self.config.image, "Image found");
                Ok(())
            }
            Err(_) => {
                Err(format!(
                    "Image '{}' not found. Pull or build it first: docker pull {}",
                    self.config.image, self.config.image
                )
                .into())
            }
        }
    }

    /// Spawn a gladiator container for one side of a match.
    pub async fn spawn_gladiator(
        &self,
        match_id: u64,
        slot: u8,
        env: HashMap<String, String>,
    ) -> Result<SpawnedContainer, Box<dyn std::error::Error + Send + Sync>> {
        let name = format!("dfc-{}-gladiator-{}", match_id, slot);
        let env_vec: Vec<String> = env.into_iter().map(|(k, v)| format!("{}={}", k, v)).collect();

        let mut labels = HashMap::new();
        labels.insert("dfc.matchId".to_string(), match_id.to_string());
        labels.insert("dfc.slot".to_string(), slot.to_string());
        labels.insert("dfc.managed".to_string(), "dfc-node".to_string());

        let host_config = HostConfig {
            port_bindings: Some({
                let mut bindings = HashMap::new();
                bindings.insert(
                    "22/tcp".to_string(),
                    Some(vec![bollard::models::PortBinding {
                        host_ip: Some("127.0.0.1".to_string()),
                        host_port: Some("0".to_string()), // dynamic port
                    }]),
                );
                bindings
            }),
            memory: Some(self.config.memory_limit as i64),
            nano_cpus: Some(self.config.cpu_limit as i64),
            pids_limit: Some(512),
            security_opt: Some(vec!["no-new-privileges".to_string()]),
            cap_drop: Some(vec!["ALL".to_string()]),
            cap_add: Some(vec![
                "NET_BIND_SERVICE".to_string(),
                "NET_ADMIN".to_string(),
                "SETUID".to_string(),
                "SETGID".to_string(),
                "CHOWN".to_string(),
                "DAC_OVERRIDE".to_string(),
                "SYS_CHROOT".to_string(),
                "KILL".to_string(),
                "FOWNER".to_string(),
                "AUDIT_WRITE".to_string(),
            ]),
            ..Default::default()
        };

        let container_config = ContainerConfig {
            image: Some(self.config.image.clone()),
            env: Some(env_vec),
            labels: Some(labels),
            host_config: Some(host_config),
            ..Default::default()
        };

        let container = self
            .docker
            .create_container(
                Some(CreateContainerOptions {
                    name: &name,
                    platform: None,
                }),
                container_config,
            )
            .await?;

        self.docker
            .start_container(&container.id, None::<StartContainerOptions<String>>)
            .await?;

        let info = self.docker.inspect_container(&container.id, None).await?;
        let ssh_port = info
            .network_settings
            .as_ref()
            .and_then(|ns| ns.ports.as_ref())
            .and_then(|ports| ports.get("22/tcp"))
            .and_then(|bindings| bindings.as_ref())
            .and_then(|bindings| bindings.first())
            .and_then(|b| b.host_port.as_ref())
            .and_then(|p| p.parse::<u16>().ok())
            .ok_or("Failed to get SSH port binding")?;

        info!(container_id = %container.id, ssh_port, match_id, slot, "Spawned gladiator");

        Ok(SpawnedContainer {
            container_id: container.id,
            ssh_port,
        })
    }

    /// Stop and remove a container.
    pub async fn kill_container(&self, container_id: &str) {
        if let Err(e) = self
            .docker
            .stop_container(container_id, Some(StopContainerOptions { t: 5 }))
            .await
        {
            warn!(container_id, error = %e, "Failed to stop container");
        }
        if let Err(e) = self
            .docker
            .remove_container(
                container_id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
        {
            warn!(container_id, error = %e, "Failed to remove container");
        }
    }

    /// Check if a container is running.
    pub async fn is_running(&self, container_id: &str) -> bool {
        match self.docker.inspect_container(container_id, None).await {
            Ok(info) => info.state.map(|s| s.running.unwrap_or(false)).unwrap_or(false),
            Err(_) => false,
        }
    }
}
