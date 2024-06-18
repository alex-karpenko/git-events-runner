use crate::resources::{
    action::{Action, ClusterAction},
    git_repo::{ClusterGitRepo, GitRepo},
    trigger::{ScheduleTrigger, WebhookTrigger},
};
use crate::{Error, Result};
use core::hash::Hash;
use futures::future::ready;
use futures::StreamExt;
use k8s_openapi::{api::core::v1::Secret, ByteString};
use kube::runtime::WatchStreamExt;
use kube::{
    runtime::reflector::{self, Store},
    Api, Client,
};
use serde::Deserialize;
use std::{
    cmp::Eq,
    collections::{BTreeMap, HashMap},
    default::Default,
    fmt::Debug,
    marker::Send,
    sync::{Arc, OnceLock},
    time::{Duration, SystemTime},
};
use tokio::sync::RwLock;
use tracing::{debug, trace};

pub static SECRET_CACHE: OnceLock<SecretsCache> = OnceLock::new();

static API_CACHE_STORE: ApiCacheStore = ApiCacheStore {
    git_repo: OnceLock::new(),
    cluster_git_repo: OnceLock::new(),
    action: OnceLock::new(),
    cluster_action: OnceLock::new(),
    webhook_trigger: OnceLock::new(),
    schedule_trigger: OnceLock::new(),
};

pub struct ApiCacheStore {
    git_repo: OnceLock<Store<GitRepo>>,
    cluster_git_repo: OnceLock<Store<ClusterGitRepo>>,
    action: OnceLock<Store<Action>>,
    cluster_action: OnceLock<Store<ClusterAction>>,
    webhook_trigger: OnceLock<Store<WebhookTrigger>>,
    schedule_trigger: OnceLock<Store<ScheduleTrigger>>,
}

impl ApiCacheStore {
    pub async fn watch(client: Client) {
        tokio::join!(
            GitRepo::watch(client.clone()),
            ClusterGitRepo::watch(client.clone()),
            Action::watch(client.clone()),
            ClusterAction::watch(client.clone()),
            WebhookTrigger::watch(client.clone()),
            ScheduleTrigger::watch(client.clone()),
        );
    }
}

#[allow(async_fn_in_trait)]
pub trait ApiCache {
    fn get(name: &str, namespace: Option<&str>) -> Result<Arc<Self>>
    where
        Self: Sized,
        Self: 'static + reflector::Lookup,
        Self::DynamicType: Hash + Eq,
        Self: Clone,
        <Self as reflector::Lookup>::DynamicType: Clone,
        <Self as reflector::Lookup>::DynamicType: Default,
    {
        let store = Self::get_cache_store();
        let key = reflector::ObjectRef::new(name).within(namespace.unwrap_or(&String::from("default")));
        if let Some(value) = store.get(&key) {
            Ok(value)
        } else {
            let name = if let Some(namespace) = namespace {
                format!("{namespace}/{name}")
            } else {
                name.to_string()
            };
            Err(Error::ResourceNotFoundError(name))
        }
    }

    async fn watch(client: Client)
    where
        Self: 'static + Sized + Clone + Debug + Send,
        Self: kube::Resource,
        <Self as kube::Resource>::DynamicType: Default + Eq + Hash + Clone,
        for<'de> Self: Deserialize<'de>,
    {
        let api: Api<Self> = Api::all(client);
        let (store, writer) = reflector::store();
        let stream = reflector::reflector(
            writer,
            kube::runtime::watcher(api, kube::runtime::watcher::Config::default()),
        );

        Self::set_cache_store(store);
        let watch = stream.applied_objects().for_each(|o| {
            trace!(object = ?o, "updating object in cache");
            ready(())
        });
        watch.await
    }

    fn get_cache_store() -> Store<Self>
    where
        Self: Sized,
        Self: 'static + reflector::Lookup,
        Self::DynamicType: Hash + Eq;

    fn set_cache_store(store: Store<Self>)
    where
        Self: Sized,
        Self: 'static + reflector::Lookup,
        Self::DynamicType: Hash + Eq;
}

impl ApiCache for GitRepo {
    fn get_cache_store() -> Store<Self> {
        API_CACHE_STORE.git_repo.get().unwrap().clone()
    }

    fn set_cache_store(store: Store<Self>) {
        API_CACHE_STORE.git_repo.set(store).unwrap();
    }

    #[cfg(test)]
    fn get(name: &str, namespace: Option<&str>) -> Result<Arc<Self>>
    where
        Self: Sized + Clone,
    {
        use std::thread::spawn;
        use tokio::runtime::Runtime;

        spawn({
            let name = String::from(name);
            let namespace = namespace.map(String::from);

            move || {
                let rt = Runtime::new()?;
                rt.block_on(async move {
                    let client = Client::try_default().await.unwrap();
                    let api = Api::<Self>::namespaced(client, &namespace.unwrap());
                    match api.get(&name).await {
                        Ok(resource) => Ok(Arc::new(resource)),
                        Err(e) => Err(e.into()),
                    }
                })
            }
        })
        .join()
        .unwrap()
    }
}

impl ApiCache for ClusterGitRepo {
    fn get_cache_store() -> Store<Self> {
        API_CACHE_STORE.cluster_git_repo.get().unwrap().clone()
    }

    fn set_cache_store(store: Store<Self>) {
        API_CACHE_STORE.cluster_git_repo.set(store).unwrap();
    }
}

impl ApiCache for Action {
    fn get_cache_store() -> Store<Self> {
        API_CACHE_STORE.action.get().unwrap().clone()
    }

    fn set_cache_store(store: Store<Self>) {
        API_CACHE_STORE.action.set(store).unwrap();
    }
}

impl ApiCache for ClusterAction {
    fn get_cache_store() -> Store<Self> {
        API_CACHE_STORE.cluster_action.get().unwrap().clone()
    }

    fn set_cache_store(store: Store<Self>) {
        API_CACHE_STORE.cluster_action.set(store).unwrap();
    }
}

impl ApiCache for WebhookTrigger {
    fn get_cache_store() -> Store<Self> {
        API_CACHE_STORE.webhook_trigger.get().unwrap().clone()
    }

    fn set_cache_store(store: Store<Self>) {
        API_CACHE_STORE.webhook_trigger.set(store).unwrap();
    }

    #[cfg(test)]
    fn get(name: &str, namespace: Option<&str>) -> Result<Arc<Self>>
    where
        Self: Sized + Clone,
    {
        use std::thread::spawn;
        use tokio::runtime::Runtime;

        spawn({
            let name = String::from(name);
            let namespace = namespace.map(String::from);

            move || {
                let rt = Runtime::new()?;
                rt.block_on(async move {
                    let client = Client::try_default().await.unwrap();
                    let api = Api::<Self>::namespaced(client, &namespace.unwrap());
                    match api.get(&name).await {
                        Ok(resource) => Ok(Arc::new(resource)),
                        Err(e) => Err(e.into()),
                    }
                })
            }
        })
        .join()
        .unwrap()
    }
}

impl ApiCache for ScheduleTrigger {
    fn get_cache_store() -> Store<Self> {
        API_CACHE_STORE.schedule_trigger.get().unwrap().clone()
    }

    fn set_cache_store(store: Store<Self>) {
        API_CACHE_STORE.schedule_trigger.set(store).unwrap();
    }

    #[cfg(test)]
    fn get(name: &str, namespace: Option<&str>) -> Result<Arc<Self>>
    where
        Self: Sized + Clone,
    {
        use std::thread::spawn;
        use tokio::runtime::Runtime;

        spawn({
            let name = String::from(name);
            let namespace = namespace.map(String::from);

            move || {
                let rt = Runtime::new()?;
                rt.block_on(async move {
                    let client = Client::try_default().await.unwrap();
                    let api = Api::<Self>::namespaced(client, &namespace.unwrap());
                    match api.get(&name).await {
                        Ok(resource) => Ok(Arc::new(resource)),
                        Err(e) => Err(e.into()),
                    }
                })
            }
        })
        .join()
        .unwrap()
    }
}

#[derive(Clone)]
pub struct SecretsCache {
    expiration_timeout: Duration,
    client: Client,
    cache: Arc<RwLock<HashMap<String, SecretValue>>>,
}

#[derive(Debug)]
struct SecretValue {
    data: BTreeMap<String, ByteString>,
    expires_at: SystemTime,
}

impl Debug for SecretsCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExpiringSecretCache")
            .field("expiration_timeout", &self.expiration_timeout)
            .field("cache", &self.cache)
            .finish()
    }
}

impl SecretsCache {
    #[cfg(not(test))]
    pub async fn get(namespace: &str, secret_name: &str, key: &str) -> Result<String, Error> {
        SECRET_CACHE
            .get()
            .expect("unable to get shared secret cache instance, looks like a BUG!")
            .get_key_value(namespace, secret_name, key)
            .await
    }

    #[cfg(test)]
    pub async fn get(namespace: &str, secret_name: &str, key: &str) -> Result<String, Error> {
        let client = Client::try_default().await.unwrap();
        let (_, secret_value) = Self::query_secrets_api(client, namespace, secret_name, key).await?;

        Ok(secret_value)
    }

    /// Creates shared static instance of the cache
    pub fn init_cache(expiration_timeout: Duration, client: Client) {
        SECRET_CACHE
            .set(Self {
                expiration_timeout,
                client,
                cache: Arc::new(RwLock::new(HashMap::new())),
            })
            .expect("failed to init shared secret cache, looks like a BUG!");
    }

    /// Does actual work with secrets and hides implementation
    async fn get_key_value(&self, namespace: &str, secret_name: &str, key: &str) -> Result<String, Error> {
        let hash_key = format!("{namespace}/{secret_name}");
        debug!(secret = %hash_key, %key, "getting secret from cache");

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
                    trace!(secret = %hash_key, %key, "object found");
                    return Ok(value);
                } else {
                    trace!(secret = %hash_key, %key, "object expired or key doesn't exist");
                }
            } else {
                trace!(secret = %hash_key, %key, "object is not in the cache");
            }
        }

        // If it's not cached yet or already expired - retrieve secret from API and store to cache
        let mut cache = self.cache.write().await;
        trace!(secret = %hash_key, %key, "try to retrieve and save to the cache");
        let (secret_data_raw, secret_value) =
            Self::query_secrets_api(self.client.clone(), namespace, secret_name, key).await?;

        let cache_data = SecretValue {
            data: secret_data_raw,
            expires_at: SystemTime::now()
                .checked_add(self.expiration_timeout)
                .expect("looks like a BUG!"),
        };
        trace!(secret = %hash_key, %key, "save object to the cache");
        cache.insert(hash_key, cache_data);

        Ok(secret_value)
    }

    pub async fn query_secrets_api(
        client: Client,
        namespace: &str,
        secret_name: &str,
        key: &str,
    ) -> Result<(BTreeMap<String, ByteString>, String), Error> {
        let secrets_api: Api<Secret> = Api::namespaced(client, namespace);
        let secret = secrets_api.get(secret_name).await?;

        let secret_data_raw = secret
            .clone()
            .data
            .ok_or_else(|| Error::SecretDecodingError(format!("no `data` part in the secret `{secret_name}`")))?;
        let secret_data = secret_data_raw
            .get(key)
            .ok_or_else(|| Error::SecretDecodingError(format!("no `{key}` key in the secret `{secret_name}`")))?
            .to_owned();
        let secret_value = String::from_utf8(secret_data.0).map_err(|_e| {
            Error::SecretDecodingError(format!(
                "error converting string `{key}` from UTF8 in the secret `{secret_name}`"
            ))
        })?;

        Ok((secret_data_raw, secret_value))
    }
}
