use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Read-only credential abstraction. Credentials never enter model context or session log.
#[async_trait]
pub trait CredentialVault: Send + Sync {
    async fn get(&self, key: &str) -> Option<String>;
}

/// Reads credentials from environment variables.
pub struct EnvCredentialVault;

#[async_trait]
impl CredentialVault for EnvCredentialVault {
    async fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

/// In-memory credential store, useful for tests and embedded deployments.
pub struct InMemoryCredentialVault {
    store: Arc<RwLock<HashMap<String, String>>>,
}

impl Default for InMemoryCredentialVault {
    fn default() -> Self {
        Self { store: Arc::new(RwLock::new(HashMap::new())) }
    }
}

impl InMemoryCredentialVault {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn set(&self, key: impl Into<String>, value: impl Into<String>) {
        self.store.write().await.insert(key.into(), value.into());
    }
}

#[async_trait]
impl CredentialVault for InMemoryCredentialVault {
    async fn get(&self, key: &str) -> Option<String> {
        self.store.read().await.get(key).cloned()
    }
}

/// Tries each vault in order, returning the first non-`None` result.
pub struct ChainedCredentialVault {
    vaults: Vec<Box<dyn CredentialVault>>,
}

impl ChainedCredentialVault {
    pub fn new(vaults: Vec<Box<dyn CredentialVault>>) -> Self {
        Self { vaults }
    }
}

#[async_trait]
impl CredentialVault for ChainedCredentialVault {
    async fn get(&self, key: &str) -> Option<String> {
        for vault in &self.vaults {
            if let Some(v) = vault.get(key).await {
                return Some(v);
            }
        }
        None
    }
}
