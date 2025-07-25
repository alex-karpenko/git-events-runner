//! Module implements two global shared singletons to access cached resources:
//! - custom API resources, using watch/store mechanism
//! - secrets' values, using trivial store with expiration
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

/// Shared singleton instance of the secret cache
pub static SECRET_CACHE: OnceLock<SecretsCache> = OnceLock::new();

static API_CACHE_STORE: ApiCacheStore = ApiCacheStore {
    git_repo: OnceLock::new(),
    cluster_git_repo: OnceLock::new(),
    action: OnceLock::new(),
    cluster_action: OnceLock::new(),
    webhook_trigger: OnceLock::new(),
    schedule_trigger: OnceLock::new(),
};

/// Cache store for all our APIs,
/// contains shared [`Store`] instances
pub struct ApiCacheStore {
    git_repo: OnceLock<Store<GitRepo>>,
    cluster_git_repo: OnceLock<Store<ClusterGitRepo>>,
    action: OnceLock<Store<Action>>,
    cluster_action: OnceLock<Store<ClusterAction>>,
    webhook_trigger: OnceLock<Store<WebhookTrigger>>,
    schedule_trigger: OnceLock<Store<ScheduleTrigger>>,
}

impl ApiCacheStore {
    /// Method to simultaneously watch all objects of our custom APIs.
    /// Uses watch method of of the each API.
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
/// Shared behavior of all cache for the each our API.
/// Implements almost all methods for default.
pub trait ApiCache {
    /// Returns object form the cache by name and optional namespace.
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

    /// API level implementation of the watch method.
    /// Initializes API cache instance and starts watching loop.
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

    /// Returns shared Store instance of particular API type.
    /// Required method, should be implemented for each API type.
    fn get_cache_store() -> Store<Self>
    where
        Self: Sized,
        Self: 'static + reflector::Lookup,
        Self::DynamicType: Hash + Eq;

    /// Initialize shared Store instance of particular API type.
    /// Required method, should be implemented for each API type.
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
        use crate::tests;
        use std::thread::spawn;
        use tokio::runtime::Runtime;

        spawn({
            let name = String::from(name);
            let namespace = namespace.map(String::from);

            move || {
                let rt = Runtime::new()?;
                rt.block_on(async move {
                    let client = tests::get_test_kube_client().await.unwrap();
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
        use crate::tests;
        use std::thread::spawn;
        use tokio::runtime::Runtime;

        spawn({
            let name = String::from(name);
            let namespace = namespace.map(String::from);

            move || {
                let rt = Runtime::new()?;
                rt.block_on(async move {
                    let client = tests::get_test_kube_client().await.unwrap();
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
        use crate::tests;
        use std::thread::spawn;
        use tokio::runtime::Runtime;

        spawn({
            let name = String::from(name);
            let namespace = namespace.map(String::from);

            move || {
                let rt = Runtime::new()?;
                rt.block_on(async move {
                    let client = tests::get_test_kube_client().await.unwrap();
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

/// Type to store shared secret cache.
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
    /// Returns secrets' key value.
    /// It secret is present in the cache and isn't expired, than returns value from the cache,
    /// or retrieve full secrets' value from the cluster if it's expired or absent and returns keys' value after that.
    pub async fn get(namespace: &str, secret_name: &str, key: &str) -> Result<String, Error> {
        #[cfg(not(test))]
        {
            Self::query_secrets_cache(namespace, secret_name, key).await
        }

        #[cfg(test)]
        {
            use crate::tests;

            let client = tests::get_test_kube_client().await.unwrap();
            let (_, secret_value) = Self::query_secrets_api(client, namespace, secret_name, key).await?;

            Ok(secret_value)
        }
    }

    /// Returns secrets' key value.
    /// It secret is present in the cache and isn't expired, than returns value from the cache,
    /// or retrieve full secrets' value from the cluster if it's expired or absent and returns keys' value after that.
    pub async fn query_secrets_cache(namespace: &str, secret_name: &str, key: &str) -> Result<String, Error> {
        SECRET_CACHE
            .get()
            .expect("unable to get shared secret cache instance, looks like a BUG!")
            .get_key_value(namespace, secret_name, key)
            .await
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
                if secret.expires_at > SystemTime::now() {
                    if let Some(value) = value {
                        let value = value.0.clone();
                        let value = String::from_utf8(value).map_err(|_e| {
                            Error::SecretDecodingError(format!(
                                "error converting string `{key}` from UTF8 in the secret `{secret_name}`"
                            ))
                        })?;
                        trace!(secret = %hash_key, %key, "object found");
                        return Ok(value);
                    }
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

    /// Retrieve secrets' content and particular key value
    /// Return whole content of the secret (BTreeMap) for caching and
    /// extract single value by key (String)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests;
    use insta::assert_debug_snapshot;
    use k8s_openapi::api::core::v1::Namespace;
    use kube::{
        api::{DeleteParams, PostParams},
        Resource,
    };
    use tokio::sync::OnceCell;

    const TEST_SECRET_NAME: &str = "test-secrets-cache-data";
    const TEST_SECRET_KEY: &str = "token";
    const TEST_SECRET_TOKEN: &str = "1234567890";
    const NAMESPACE: &str = "secret-cache-test";

    static INITIALIZED: OnceCell<()> = OnceCell::const_new();

    async fn init() -> anyhow::Result<Client> {
        INITIALIZED
            .get_or_init(|| async {
                tests::init_crypto_provider().await;
                let client = tests::get_test_kube_client().await.unwrap();
                create_namespace(client).await;
            })
            .await;

        tests::get_test_kube_client().await
    }

    /// Unattended namespace creation
    async fn create_namespace(client: Client) {
        let api = Api::<Namespace>::all(client);
        let pp = PostParams::default();

        let mut data = Namespace::default();
        data.meta_mut().name = Some(String::from(NAMESPACE));
        api.create(&pp, &data).await.unwrap_or_default();
    }

    #[tokio::test]
    #[ignore = "needs docker"]
    async fn secrets_cache() {
        let client = init().await.unwrap();
        let pp = PostParams::default();
        let dp = DeleteParams::default();
        let expire_time = Duration::from_secs(1);
        let api = Api::<Secret>::namespaced(client.clone(), NAMESPACE);

        // Ensure Secret isn't present
        let _ = api.delete(TEST_SECRET_NAME, &dp).await;

        // Init secrets cache
        SecretsCache::init_cache(expire_time, client.clone());

        // Try to retrieve non-existent secret
        let res = SecretsCache::query_secrets_cache(NAMESPACE, TEST_SECRET_NAME, TEST_SECRET_KEY).await;
        assert!(res.is_err());
        assert_debug_snapshot!(res.err().unwrap());

        // Create secret
        let mut secrets = BTreeMap::new();
        let value = k8s_openapi::ByteString(Vec::<u8>::from(TEST_SECRET_TOKEN));
        secrets.insert("token".into(), value);
        let mut data = Secret::default();
        data.meta_mut().name = Some(String::from(TEST_SECRET_NAME));
        let _ = data.data.insert(secrets);
        api.create(&pp, &data).await.unwrap_or_default();

        // Try to retrieve non-existent key
        let res = SecretsCache::query_secrets_cache(NAMESPACE, TEST_SECRET_NAME, "some-non-existent-key").await;
        assert!(res.is_err());
        assert_debug_snapshot!(res.err().unwrap());

        // Try to retrieve existent key
        let res = SecretsCache::query_secrets_cache(NAMESPACE, TEST_SECRET_NAME, TEST_SECRET_KEY)
            .await
            .unwrap();
        assert_eq!(res, TEST_SECRET_TOKEN);

        // Delete secret while cache is valid, and try to retrieve secret from cache
        let _ = api.delete(TEST_SECRET_NAME, &dp).await;
        let res = SecretsCache::query_secrets_cache(NAMESPACE, TEST_SECRET_NAME, TEST_SECRET_KEY)
            .await
            .unwrap();
        assert_eq!(res, TEST_SECRET_TOKEN);

        // Wait for expiration, try to retrieve and get error
        tokio::time::sleep(expire_time).await;
        let res = SecretsCache::query_secrets_cache(NAMESPACE, TEST_SECRET_NAME, "some-non-existent-key").await;
        assert!(res.is_err());
    }
}
