use reqwest::Client;
use std::time::Duration;
use tracing::warn;

use crate::auth::token::TokenStore;
use crate::types::*;

const MAX_RETRIES: u32 = 3;
const INITIAL_BACKOFF_MS: u64 = 1000;

/// HTTP client for the DFC coordinator API.
pub struct CoordinatorClient {
    pub(crate) http: Client,
    pub(crate) base_url: String,
    pub(crate) token_store: TokenStore,
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
    #[error("Unauthorized — token refresh failed")]
    Unauthorized,
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
    pub async fn heartbeat(&self, node_id: u64, req: &NodeHeartbeatRequest) -> Result<(), ApiError> {
        let url = format!("{}/api/nodes/{}/heartbeat", self.base_url, node_id);
        let _resp = self.post_with_retry_with_auth_handler(&url, req).await?;
        Ok(())
    }

    /// Long-poll for match assignments.
    pub async fn poll_assignments(
        &self,
        node_id: u64,
        timeout_seconds: u32,
    ) -> Result<Vec<NodeAssignment>, ApiError> {
        let url = format!(
            "{}/api/nodes/{}/assignments?timeout={}",
            self.base_url, node_id, timeout_seconds
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

        if resp.status().as_u16() == 401 || resp.status().as_u16() == 403 {
            // Token expired or unauthorized — signal caller to re-auth
            let body = resp.text().await.unwrap_or_default();
            return Err(ApiError::Api {
                status: 401,
                body,
            });
        }

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ApiError::Api { status, body });
        }

        let data: AssignmentResponse = resp.json().await?;
        Ok(data.assignments)
    }

    /// Report match results to coordinator.
    pub async fn report_match(&self, node_id: u64, report: &NodeMatchReport) -> Result<(), ApiError> {
        let url = format!("{}/api/nodes/{}/report", self.base_url, node_id);
        let _resp = self.post_with_retry_with_auth_handler(&url, report).await?;
        Ok(())
    }

    /// POST with exponential backoff retry, plus 401/403 detection.
    /// On 401/403, returns `ApiError::Unauthorized` immediately (no retry at this layer).
    async fn post_with_retry_with_auth_handler<T: serde::Serialize>(
        &self,
        url: &str,
        body: &T,
    ) -> Result<reqwest::Response, ApiError> {
        let mut backoff = INITIAL_BACKOFF_MS;

        for attempt in 0..MAX_RETRIES {
            let token = self
                .token_store
                .get()
                .await
                .ok_or(ApiError::NotAuthenticated)?;

            let req = self
                .http
                .post(url)
                .json(body)
                .header("Authorization", format!("Bearer {}", token));

            match req.send().await {
                Ok(resp) if resp.status().is_success() => return Ok(resp),
                Ok(resp) if resp.status().as_u16() == 401 || resp.status().as_u16() == 403 => {
                    // 401/403 — don't retry at the HTTP layer, propagate up
                    let body_text = resp.text().await.unwrap_or_default();
                    return Err(ApiError::Api {
                        status: 401,
                        body: body_text,
                    });
                }
                Ok(resp) if resp.status().is_server_error() => {
                    let status = resp.status().as_u16();
                    warn!(attempt, status, "Server error, retrying...");
                }
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    let body_text = resp.text().await.unwrap_or_default();
                    return Err(ApiError::Api {
                        status,
                        body: body_text,
                    });
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

    /// POST with exponential backoff retry (original — for unauthenticated endpoints).
    pub(crate) async fn post_with_retry<T: serde::Serialize>(
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

/// Returns true if this error represents a 401/403 Unauthorized response.
pub fn is_unauthorized(err: &ApiError) -> bool {
    matches!(err, ApiError::Api { status: 401, .. } | ApiError::Api { status: 403, .. } | ApiError::Unauthorized)
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

    #[test]
    fn is_unauthorized_detects_401() {
        let err = ApiError::Api {
            status: 401,
            body: "Unauthorized".to_string(),
        };
        assert!(is_unauthorized(&err));
    }

    #[test]
    fn is_unauthorized_detects_403() {
        let err = ApiError::Api {
            status: 403,
            body: "Forbidden".to_string(),
        };
        assert!(is_unauthorized(&err));
    }

    #[test]
    fn is_unauthorized_false_for_other_errors() {
        let err = ApiError::Api {
            status: 500,
            body: "Server error".to_string(),
        };
        assert!(!is_unauthorized(&err));

        let err2 = ApiError::MaxRetries;
        assert!(!is_unauthorized(&err2));
    }

    #[tokio::test]
    async fn poll_assignments_returns_unauthorized_on_401() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/nodes/1/assignments"))
            .respond_with(ResponseTemplate::new(401).set_body_string("Unauthorized"))
            .mount(&mock_server)
            .await;

        let store = TokenStore::new();
        store.set("expired-token".to_string(), 1).await;
        let client = CoordinatorClient::new(&mock_server.uri(), store);

        let result = client.poll_assignments(1, 5).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(is_unauthorized(&err), "Expected unauthorized error, got: {err}");
    }
}
