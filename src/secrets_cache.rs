use crate::Error;
use k8s_openapi::{api::core::v1::Secret, ByteString};
use kube::{Api, Client};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::sync::RwLock;
use tracing::debug;

#[allow(async_fn_in_trait)]
pub trait SecretCache {
    async fn get(&self, namespace: &str, secret: &str, key: &str) -> Result<String, Error>;
}

#[derive(Clone)]
pub struct ExpiringSecretCache {
    expiration_timeout: Duration,
    client: Client,
    cache: Arc<RwLock<HashMap<String, SecretValue>>>,
}

struct SecretValue {
    data: BTreeMap<String, ByteString>,
    expires_at: SystemTime,
}

impl SecretCache for ExpiringSecretCache {
    async fn get(&self, namespace: &str, secret_name: &str, key: &str) -> Result<String, Error> {
        let hash_key = format!("{namespace}/{secret_name}");
        debug!("get: hash_key={hash_key}, key={key}");

        // Let's try to find in cache
        {
            let cache = self.cache.read().await;
            if let Some(secret) = cache.get(&hash_key) {
                // and return value if it's cached and not expired and contains key
                let value = secret.data.get(key);
                if secret.expires_at > SystemTime::now() && value.is_some() {
                    let value = value.unwrap().0.clone();
                    let value = String::from_utf8(value).map_err(|_e| {
                        Error::SecretDecodingError(format!(
                            "error converting string `{key}` from UTF8 in the secret `{secret_name}`"
                        ))
                    })?;
                    debug!("get: {hash_key}/{key} exists");
                    return Ok(value);
                } else {
                    debug!("get: {hash_key}/{key} expired or key doesn't exist");
                }
            } else {
                debug!("get: {hash_key}/{key} not in cache");
            }
        }

        // If it's not cached yet or already expired - retrieve secret from API and store to cache
        let mut cache = self.cache.write().await;
        debug!("get: {hash_key}/{key} try to retrieve and save in cache");
        let secrets_api: Api<Secret> = Api::namespaced(self.client.clone(), namespace);
        let secret = secrets_api.get(secret_name).await?;

        let secret_data_raw = secret.clone().data.ok_or_else(|| {
            Error::SecretDecodingError(format!("no `data` part in the secret `{secret_name}`"))
        })?;
        let secret_data = secret_data_raw
            .get(key)
            .ok_or_else(|| {
                Error::SecretDecodingError(format!("no `{key}` key in the secret `{secret_name}`"))
            })?
            .to_owned();
        let secret_value = String::from_utf8(secret_data.0).map_err(|_e| {
            Error::SecretDecodingError(format!(
                "error converting string `{key}` from UTF8 in the secret `{secret_name}`"
            ))
        })?;

        let cache_data = SecretValue {
            data: secret_data_raw,
            expires_at: SystemTime::now()
                .checked_add(self.expiration_timeout)
                .expect("looks like a BUG!"),
        };
        debug!("get: {hash_key}/{key} save to cache");
        cache.insert(hash_key, cache_data);

        Ok(secret_value)
    }
}

impl ExpiringSecretCache {
    pub fn new(expiration_timeout: Duration, client: Client) -> Arc<Self> {
        let cache = Self {
            expiration_timeout,
            client,
            cache: Arc::new(RwLock::new(HashMap::new())),
        };

        Arc::new(cache)
    }
}
