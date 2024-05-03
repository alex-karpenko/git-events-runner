use crate::resources::{
    action::{Action, ClusterAction},
    git_repo::{ClusterGitRepo, GitRepo},
    trigger::{ScheduleTrigger, WebhookTrigger},
};
use crate::{Error, Result};
use core::hash::Hash;
use futures::future::ready;
use futures::StreamExt;
use kube::runtime::WatchStreamExt;
use kube::{
    runtime::reflector::{self, Store},
    Api, Client,
};
use serde::Deserialize;
use std::{
    cmp::Eq,
    default::Default,
    fmt::Debug,
    marker::Send,
    sync::{Arc, OnceLock},
};
use tracing::debug;

static CACHE_STORE: CacheStore = CacheStore {
    git_repo: OnceLock::new(),
    cluster_git_repo: OnceLock::new(),
    action: OnceLock::new(),
    cluster_action: OnceLock::new(),
    webhook_trigger: OnceLock::new(),
    schedule_trigger: OnceLock::new(),
};

pub struct CacheStore {
    git_repo: OnceLock<Store<GitRepo>>,
    cluster_git_repo: OnceLock<Store<ClusterGitRepo>>,
    action: OnceLock<Store<Action>>,
    cluster_action: OnceLock<Store<ClusterAction>>,
    webhook_trigger: OnceLock<Store<WebhookTrigger>>,
    schedule_trigger: OnceLock<Store<ScheduleTrigger>>,
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
        let key =
            reflector::ObjectRef::new(name).within(namespace.unwrap_or(&String::from("default")));
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
            debug!("ApiCache {o:?}");
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
        CACHE_STORE.git_repo.get().unwrap().clone()
    }

    fn set_cache_store(store: Store<Self>) {
        CACHE_STORE.git_repo.set(store).unwrap();
    }
}

impl ApiCache for ClusterGitRepo {
    fn get_cache_store() -> Store<Self> {
        CACHE_STORE.cluster_git_repo.get().unwrap().clone()
    }

    fn set_cache_store(store: Store<Self>) {
        CACHE_STORE.cluster_git_repo.set(store).unwrap();
    }
}

impl ApiCache for Action {
    fn get_cache_store() -> Store<Self> {
        CACHE_STORE.action.get().unwrap().clone()
    }

    fn set_cache_store(store: Store<Self>) {
        CACHE_STORE.action.set(store).unwrap();
    }
}

impl ApiCache for ClusterAction {
    fn get_cache_store() -> Store<Self> {
        CACHE_STORE.cluster_action.get().unwrap().clone()
    }

    fn set_cache_store(store: Store<Self>) {
        CACHE_STORE.cluster_action.set(store).unwrap();
    }
}

impl ApiCache for WebhookTrigger {
    fn get_cache_store() -> Store<Self> {
        CACHE_STORE.webhook_trigger.get().unwrap().clone()
    }

    fn set_cache_store(store: Store<Self>) {
        CACHE_STORE.webhook_trigger.set(store).unwrap();
    }
}

impl ApiCache for ScheduleTrigger {
    fn get_cache_store() -> Store<Self> {
        CACHE_STORE.schedule_trigger.get().unwrap().clone()
    }

    fn set_cache_store(store: Store<Self>) {
        CACHE_STORE.schedule_trigger.set(store).unwrap();
    }
}

impl CacheStore {
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
