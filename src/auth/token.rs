use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// Manages the node's JWT token with thread-safe access.
/// The token is refreshed before expiry by the heartbeat loop.
#[derive(Clone)]
pub struct TokenStore {
    inner: Arc<RwLock<TokenState>>,
}

struct TokenState {
    token: Option<String>,
    node_id: Option<u64>,
    /// When the token was issued (for relative expiry tracking).
    issued_at: Option<Instant>,
    /// How long the token is valid.
    ttl: Option<Duration>,
}

/// Default token lifetime assumption when the server doesn't tell us.
const DEFAULT_TOKEN_TTL_SECS: u64 = 3600;
/// Refresh when this many seconds remain before expiry.
pub const REFRESH_THRESHOLD_SECS: u64 = 120;

impl TokenStore {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(TokenState {
                token: None,
                node_id: None,
                issued_at: None,
                ttl: None,
            })),
        }
    }

    /// Store a new token after registration or refresh.
    /// `ttl_seconds` is optional — if the coordinator provides it, use it.
    pub async fn set(&self, token: String, node_id: u64) {
        self.set_with_ttl(token, node_id, DEFAULT_TOKEN_TTL_SECS)
            .await;
    }

    /// Store a new token with an explicit TTL (seconds).
    pub async fn set_with_ttl(&self, token: String, node_id: u64, ttl_seconds: u64) {
        let mut state = self.inner.write().await;
        state.token = Some(token);
        state.node_id = Some(node_id);
        state.issued_at = Some(Instant::now());
        state.ttl = Some(Duration::from_secs(ttl_seconds));
    }

    /// Get the current token for API requests.
    pub async fn get(&self) -> Option<String> {
        let state = self.inner.read().await;
        state.token.clone()
    }

    /// Get the node ID assigned during registration.
    pub async fn node_id(&self) -> Option<u64> {
        let state = self.inner.read().await;
        state.node_id
    }

    /// Check if we have a valid token.
    pub async fn is_authenticated(&self) -> bool {
        let state = self.inner.read().await;
        state.token.is_some()
    }

    /// Returns true if the token is within `REFRESH_THRESHOLD_SECS` of expiry
    /// or has already expired. Returns false if there is no token.
    pub async fn needs_refresh(&self) -> bool {
        let state = self.inner.read().await;
        match (&state.token, state.issued_at, state.ttl) {
            (Some(_), Some(issued), Some(ttl)) => {
                let elapsed = issued.elapsed();
                let remaining = ttl.saturating_sub(elapsed);
                remaining <= Duration::from_secs(REFRESH_THRESHOLD_SECS)
            }
            // No token at all — not "needs refresh", just not authenticated
            _ => false,
        }
    }

    /// Seconds remaining until the token expires. Returns 0 if expired or unknown.
    pub async fn seconds_until_expiry(&self) -> u64 {
        let state = self.inner.read().await;
        match (state.issued_at, state.ttl) {
            (Some(issued), Some(ttl)) => {
                let elapsed = issued.elapsed();
                ttl.saturating_sub(elapsed).as_secs()
            }
            _ => 0,
        }
    }

    /// Clear the token (for deregistration or forced re-auth).
    pub async fn clear(&self) {
        let mut state = self.inner.write().await;
        state.token = None;
        state.node_id = None;
        state.issued_at = None;
        state.ttl = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::sleep;

    #[tokio::test]
    async fn token_lifecycle() {
        let store = TokenStore::new();

        assert!(!store.is_authenticated().await);
        assert!(store.get().await.is_none());

        store.set("jwt-token-123".to_string(), 42).await;

        assert!(store.is_authenticated().await);
        assert_eq!(store.get().await.unwrap(), "jwt-token-123");
        assert_eq!(store.node_id().await.unwrap(), 42);

        store.clear().await;
        assert!(!store.is_authenticated().await);
    }

    #[tokio::test]
    async fn needs_refresh_false_when_fresh() {
        let store = TokenStore::new();
        // Token with 1-hour TTL — should not need refresh yet
        store
            .set_with_ttl("token".to_string(), 1, DEFAULT_TOKEN_TTL_SECS)
            .await;
        assert!(!store.needs_refresh().await);
    }

    #[tokio::test]
    async fn needs_refresh_true_when_expiry_near() {
        let store = TokenStore::new();
        // TTL of 60s < REFRESH_THRESHOLD_SECS (120s) — should need refresh immediately
        store.set_with_ttl("token".to_string(), 1, 60).await;
        assert!(store.needs_refresh().await);
    }

    #[tokio::test]
    async fn needs_refresh_true_after_ttl_elapsed() {
        let store = TokenStore::new();
        // TTL of 1s — will expire quickly
        store.set_with_ttl("token".to_string(), 1, 1).await;
        sleep(Duration::from_millis(1100)).await;
        assert!(store.needs_refresh().await);
    }

    #[tokio::test]
    async fn needs_refresh_false_when_no_token() {
        let store = TokenStore::new();
        // No token — needs_refresh should be false (not authenticated)
        assert!(!store.needs_refresh().await);
    }

    #[tokio::test]
    async fn seconds_until_expiry_decreases() {
        let store = TokenStore::new();
        store
            .set_with_ttl("token".to_string(), 1, DEFAULT_TOKEN_TTL_SECS)
            .await;
        let secs = store.seconds_until_expiry().await;
        // Should be close to DEFAULT_TOKEN_TTL_SECS
        assert!(secs > 0);
        assert!(secs <= DEFAULT_TOKEN_TTL_SECS);
    }
}
