use std::sync::Arc;
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
}

impl TokenStore {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(TokenState {
                token: None,
                node_id: None,
            })),
        }
    }

    /// Store a new token after registration or refresh.
    pub async fn set(&self, token: String, node_id: u64) {
        let mut state = self.inner.write().await;
        state.token = Some(token);
        state.node_id = Some(node_id);
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

    /// Clear the token (for deregistration or forced re-auth).
    pub async fn clear(&self) {
        let mut state = self.inner.write().await;
        state.token = None;
        state.node_id = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
