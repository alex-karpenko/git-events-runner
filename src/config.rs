use k8s_openapi::api::core::v1::ConfigMap;
use kube::{Api, Client};
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
                if let Some(config_data) = cm.data.unwrap_or_default().get(CONFIG_MAP_DATA_NAME) {
                    match serde_yaml::from_str::<RuntimeConfig>(config_data) {
                        Ok(config) => Some(config),
                        Err(err) => {
                            error!("Unable to deserialize runtime config: {err}");
                            None
                        }
                    }
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

    async fn update(&self) {
        if let Some(config) = self.get_cm().await {
            info!(
                "Loading runtime config from {}/{}",
                self.client.default_namespace(),
                self.cm_name
            );
            debug!("runtime config: {config:#?}");
            self.tx.send_replace(Arc::new(config));
        } else {
            warn!("Unable to update runtime config");
        }
    }
}

impl RuntimeConfig {
    pub fn get() -> Arc<RuntimeConfig> {
        let rx = CONFIG_WATCHER.get().unwrap().tx.subscribe();
        let config = rx.borrow();
        config.clone()
    }

    pub async fn init(client: Client, cm_name: &str) {
        let (tx, _) = watch::channel(Arc::new(RuntimeConfig::default()));
        let _ = CONFIG_WATCHER.set(ConfigWatcher {
            client,
            cm_name: String::from(cm_name),
            tx,
        });

        CONFIG_WATCHER.get().unwrap().update().await;
    }
}

#[derive(Deserialize, Debug, Default)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct RuntimeConfig {
    pub action: ActionConfig,
    pub trigger: TriggerConfig,
}

#[derive(Deserialize, Debug, Default)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct ActionConfig {
    pub workdir: ActionWorkdirConfig,
    pub containers: ActionContainersConfig,
}

#[derive(Deserialize, Debug)]
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

#[derive(Deserialize, Debug, Default)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct ActionContainersConfig {
    pub cloner: ActionContainersClonerConfig,
    pub worker: ActionContainersWorkerConfig,
}

#[derive(Deserialize, Debug)]
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

#[derive(Deserialize, Debug)]
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

#[derive(Deserialize, Debug, Default)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct TriggerConfig {
    pub webhook: TriggerWebhookConfig,
}

#[derive(Deserialize, Debug)]
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
