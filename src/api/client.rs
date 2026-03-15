use reqwest::Client;
use std::time::Duration;
use tracing::warn;

use crate::auth::token::TokenStore;
use crate::types::*;

const MAX_RETRIES: u32 = 3;
const INITIAL_BACKOFF_MS: u64 = 1000;

/// HTTP client for the DFC coordinator API.
pub struct CoordinatorClient {
    http: Client,
    base_url: String,
    token_store: TokenStore,
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("API error ({status}): {body}")]
    Api { status: u16, body: String },
    #[error("Not authenticated")]
    NotAuthenticated,
    #[error("Max retries exceeded")]
    MaxRetries,
}

impl CoordinatorClient {
    pub fn new(base_url: &str, token_store: TokenStore) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to build HTTP client");

        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            token_store,
        }
    }

    /// Request a nonce for wallet authentication.
    pub async fn request_nonce(&self, wallet_address: &str) -> Result<NonceResponse, ApiError> {
        let url = format!("{}/api/nodes/auth/nonce", self.base_url);
        let body = NonceRequest {
            wallet_address: wallet_address.to_string(),
        };

        let resp = self.post_with_retry(&url, &body, false).await?;
        let nonce: NonceResponse = resp.json().await?;
        Ok(nonce)
    }

    /// Register this node with the coordinator.
    pub async fn register(
        &self,
        req: &NodeRegistrationRequest,
    ) -> Result<NodeRegistrationResponse, ApiError> {
        let url = format!("{}/api/nodes/register", self.base_url);
        let resp = self.post_with_retry(&url, req, false).await?;
        let reg: NodeRegistrationResponse = resp.json().await?;
        Ok(reg)
    }

    /// Send heartbeat to coordinator.
    pub async fn heartbeat(&self, req: &NodeHeartbeatRequest) -> Result<(), ApiError> {
        let url = format!("{}/api/nodes/heartbeat", self.base_url);
        let _resp = self.post_with_retry(&url, req, true).await?;
        Ok(())
    }

    /// Long-poll for match assignments.
    pub async fn poll_assignments(
        &self,
        timeout_seconds: u32,
    ) -> Result<Vec<NodeAssignment>, ApiError> {
        let url = format!(
            "{}/api/nodes/assignments?timeout={}",
            self.base_url, timeout_seconds
        );
        let token = self
            .token_store
            .get()
            .await
            .ok_or(ApiError::NotAuthenticated)?;

        let resp = self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {}", token))
            .timeout(Duration::from_secs((timeout_seconds + 5) as u64))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ApiError::Api { status, body });
        }

        let data: AssignmentResponse = resp.json().await?;
        Ok(data.assignments)
    }

    /// Report match results to coordinator.
    pub async fn report_match(&self, report: &NodeMatchReport) -> Result<(), ApiError> {
        let url = format!("{}/api/nodes/report", self.base_url);
        let _resp = self.post_with_retry(&url, report, true).await?;
        Ok(())
    }

    /// POST with exponential backoff retry.
    async fn post_with_retry<T: serde::Serialize>(
        &self,
        url: &str,
        body: &T,
        auth: bool,
    ) -> Result<reqwest::Response, ApiError> {
        let mut backoff = INITIAL_BACKOFF_MS;

        for attempt in 0..MAX_RETRIES {
            let mut req = self.http.post(url).json(body);

            if auth {
                let token = self
                    .token_store
                    .get()
                    .await
                    .ok_or(ApiError::NotAuthenticated)?;
                req = req.header("Authorization", format!("Bearer {}", token));
            }

            match req.send().await {
                Ok(resp) if resp.status().is_success() => return Ok(resp),
                Ok(resp) if resp.status().is_server_error() => {
                    let status = resp.status().as_u16();
                    warn!(attempt, status, "Server error, retrying...");
                }
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    let body = resp.text().await.unwrap_or_default();
                    return Err(ApiError::Api { status, body });
                }
                Err(e) if attempt < MAX_RETRIES - 1 => {
                    warn!(attempt, error = %e, "Request failed, retrying...");
                }
                Err(e) => return Err(ApiError::Http(e)),
            }

            tokio::time::sleep(Duration::from_millis(backoff)).await;
            backoff *= 2;
        }

        Err(ApiError::MaxRetries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_creation() {
        let store = TokenStore::new();
        let client = CoordinatorClient::new("http://localhost:3001", store);
        assert_eq!(client.base_url, "http://localhost:3001");
    }

    #[test]
    fn url_trailing_slash_stripped() {
        let store = TokenStore::new();
        let client = CoordinatorClient::new("http://localhost:3001/", store);
        assert_eq!(client.base_url, "http://localhost:3001");
    }
}
