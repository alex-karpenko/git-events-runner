use futures::{future::ready, StreamExt};
use k8s_openapi::{api::core::v1::ConfigMap, Metadata};
use kube::{
    runtime::{watcher, WatchStreamExt},
    Api, Client,
};
use serde::Deserialize;
use std::sync::{Arc, OnceLock};
use tokio::sync::watch::{self, Sender};
use tracing::{debug, error, info, warn};

const CONFIG_MAP_DATA_NAME: &str = "runtimeConfig";

static CONFIG_WATCHER: OnceLock<ConfigWatcher> = OnceLock::new();

#[derive(Clone)]
struct ConfigWatcher {
    client: Client,
    cm_name: String,
    tx: Sender<Arc<RuntimeConfig>>,
}

impl ConfigWatcher {
    async fn get_cm(&self) -> Option<RuntimeConfig> {
        let cm_api: Api<ConfigMap> = Api::default_namespaced(self.client.clone());
        match cm_api.get(self.cm_name.as_str()).await {
            Ok(cm) => {
                if let Ok(config) = RuntimeConfig::try_from(cm) {
                    Some(config)
                } else {
                    None
                }
            }
            Err(err) => {
                error!("Unable to get runtime config: {err}");
                None
            }
        }
    }

    async fn init(&self) {
        if let Some(config) = self.get_cm().await {
            debug!("runtime config: {config:#?}");
            self.tx.send_replace(Arc::new(config));
        } else {
            warn!(
                "Unable to init runtime config from ConfigMap {}/{}",
                self.client.default_namespace(),
                self.cm_name
            );
        }
    }
}

impl TryFrom<ConfigMap> for RuntimeConfig {
    type Error = crate::Error;

    fn try_from(value: ConfigMap) -> Result<Self, Self::Error> {
        if let Some(config_data) = value.data.unwrap_or_default().get(CONFIG_MAP_DATA_NAME) {
            match serde_yaml::from_str::<RuntimeConfig>(config_data) {
                Ok(config) => Ok(config),
                Err(err) => {
                    error!("Unable to deserialize runtime config: {err}");
                    Err(err.into())
                }
            }
        } else {
            error!("Unable to deserialize runtime config");
            Err(crate::Error::RuntimeConfigFormatError)
        }
    }
}

impl RuntimeConfig {
    pub fn get() -> Arc<RuntimeConfig> {
        let rx = CONFIG_WATCHER.get().unwrap().tx.subscribe();
        let config = rx.borrow();
        config.clone()
    }

    pub async fn init_and_watch(client: Client, cm_name: String) {
        let (tx, _) = watch::channel(Arc::new(RuntimeConfig::default()));
        let _ = CONFIG_WATCHER.set(ConfigWatcher {
            client: client.clone(),
            cm_name: cm_name.clone(),
            tx,
        });
        CONFIG_WATCHER.get().unwrap().init().await;

        let cm_api: Api<ConfigMap> = Api::default_namespaced(client.clone());
        let watcher_config =
            watcher::Config::default().fields(format!("metadata.name={cm_name}").as_str());
        let cm_stream = watcher(cm_api, watcher_config);
        cm_stream
            .touched_objects()
            .for_each(|cm| {
                if let Ok(cm) = cm {
                    let cm_key = format!(
                        "{}/{}",
                        cm.metadata().namespace.clone().unwrap(),
                        cm.metadata().name.clone().unwrap()
                    );
                    match RuntimeConfig::try_from(cm) {
                        Ok(config) => {
                            CONFIG_WATCHER
                                .get()
                                .unwrap()
                                .tx
                                .send_replace(Arc::new(config));
                            info!("Loading runtime config from {cm_key}");
                        }
                        Err(err) => {
                            warn!("Ignore runtime config update due to error: {err}")
                        }
                    }
                }
                ready(())
            })
            .await
    }
}

#[derive(Clone, Deserialize, Debug, Default)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct RuntimeConfig {
    pub action: ActionConfig,
    pub trigger: TriggerConfig,
}

#[derive(Clone, Deserialize, Debug, Default)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct ActionConfig {
    pub workdir: ActionWorkdirConfig,
    pub containers: ActionContainersConfig,
}

#[derive(Clone, Deserialize, Debug)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct ActionWorkdirConfig {
    pub mount_path: String,
    pub volume_name: String,
}

impl Default for ActionWorkdirConfig {
    fn default() -> Self {
        Self {
            mount_path: String::from("/tmp/git-events-runner"),
            volume_name: String::from("temp-repo-data"),
        }
    }
}

#[derive(Clone, Deserialize, Debug, Default)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct ActionContainersConfig {
    pub cloner: ActionContainersClonerConfig,
    pub worker: ActionContainersWorkerConfig,
}

#[derive(Clone, Deserialize, Debug)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct ActionContainersClonerConfig {
    pub name: String,
    pub image: String,
}

impl Default for ActionContainersClonerConfig {
    fn default() -> Self {
        Self {
            name: String::from("action-cloner"),
            image: String::from("ghcr.io/alex-karpenko/git-events-runner/gitrepo-cloner:latest"), // TODO: use crate version instead
        }
    }
}

#[derive(Clone, Deserialize, Debug)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct ActionContainersWorkerConfig {
    pub name: String,
    pub image: String,
    pub variables_prefix: String,
}

impl Default for ActionContainersWorkerConfig {
    fn default() -> Self {
        Self {
            name: String::from("action-worker"),
            image: String::from("docker.io/bash:latest"),
            variables_prefix: String::from("ACTION_JOB_"),
        }
    }
}

#[derive(Clone, Deserialize, Debug, Default)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct TriggerConfig {
    pub webhook: TriggerWebhookConfig,
}

#[derive(Clone, Deserialize, Debug)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct TriggerWebhookConfig {
    pub default_auth_header: String,
}

impl Default for TriggerWebhookConfig {
    fn default() -> Self {
        Self {
            default_auth_header: String::from("x-trigger-auth"),
        }
    }
}
