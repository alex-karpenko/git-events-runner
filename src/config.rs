use std::sync::{Arc, OnceLock};

use futures::{future::ready, StreamExt};
use k8s_openapi::{api::core::v1::ConfigMap, Metadata};
use kube::{
    runtime::{predicates, watcher, WatchStreamExt},
    Api, Client,
};
use serde::Deserialize;
use tokio::sync::watch::{self, Sender};
use tracing::{debug, error, info, warn};

const CONFIG_MAP_DATA_NAME: &str = "runtimeConfig";
const DEFAULT_CONTAINER_REPO: &str = "ghcr.io/alex-karpenko/git-events-runner";
const DEFAULT_WEBHOOK_TRIGGER_AUTH_HEADER: &str = "x-trigger-auth";

static CONFIG_TX_CHANNEL: OnceLock<Sender<Arc<RuntimeConfig>>> = OnceLock::new();

/// Global dynamic controller config
/// It watches ConfigMap with configuration and posts changed configs to tokio::sync::watch channel
/// This approach makes possible using of sync methods to retrieve config values,
/// i.e., in Default trait implementations
///
/// Default values reflect actual defaults, so if CM doesn't contain some value,
/// they will always be filled with default values.
///
impl RuntimeConfig {
    /// Returns shared ref to current config
    pub fn get() -> Arc<RuntimeConfig> {
        let rx = CONFIG_TX_CHANNEL.get().unwrap().subscribe();
        let config = rx.borrow();
        config.clone()
    }

    /// Init whole config infrastructure, retrieve initial config and returns Future for watching changes
    pub async fn init_and_watch(client: Client, cm_name: String) {
        let (tx, _) = watch::channel(Arc::new(RuntimeConfig::default()));
        let _ = CONFIG_TX_CHANNEL.set(tx);

        let cm_api: Api<ConfigMap> = Api::default_namespaced(client.clone());
        // Load initial config content from ConfigMap
        let initial_config = match cm_api.get(cm_name.as_str()).await {
            Ok(cm) => {
                if let Ok(config) = RuntimeConfig::try_from(cm) {
                    config
                } else {
                    warn!("Unable to load initial runtime config, use default instead.");
                    RuntimeConfig::default()
                }
            }
            Err(_err) => {
                warn!("Unable to load initial runtime config, use default instead.");
                RuntimeConfig::default()
            }
        };
        CONFIG_TX_CHANNEL.get().unwrap().send_replace(Arc::new(initial_config));

        let watcher_config = watcher::Config::default().fields(format!("metadata.name={cm_name}").as_str());
        let cm_stream = watcher(cm_api, watcher_config);

        cm_stream
            .applied_objects()
            .predicate_filter(predicates::generation) // ignore updates if content isn't unchanged
            .for_each(|cm| {
                if let Ok(cm) = cm {
                    let cm_key = format!(
                        "{}/{}",
                        cm.metadata().namespace.clone().unwrap(),
                        cm.metadata().name.clone().unwrap()
                    );
                    match RuntimeConfig::try_from(cm) {
                        Ok(config) => {
                            debug!("{config:#?}");
                            CONFIG_TX_CHANNEL.get().unwrap().send_replace(Arc::new(config));
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

/// Convenient to convert ConfigMap directly into instance of RuntimeConfig
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

//
// Everything below is runtime configuration structure definition
// with defaults and everything needed to deserialize data section of CM.
// Config watcher/reload just deserializes CM's data field into `RuntimeConfig` instance.
//
// ATTENTION: Don't forget to reflect any changes into Helm values.yaml, runtimeConfig section
// TODO: Make new subcommand to dump out default config and update it in the chart as part of CD

#[derive(Clone, Deserialize, Debug, Default)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct RuntimeConfig {
    pub action: ActionConfig,
    pub trigger: TriggerConfig,
}

#[derive(Clone, Deserialize, Debug)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct ActionConfig {
    pub workdir: ActionWorkdirConfig,
    pub default_service_account: Option<String>,
    pub ttl_seconds_after_finished: i32,
    pub containers: ActionContainersConfig,
}

impl Default for ActionConfig {
    fn default() -> Self {
        Self {
            workdir: ActionWorkdirConfig::default(),
            containers: ActionContainersConfig::default(),
            default_service_account: None,
            ttl_seconds_after_finished: 7200,
        }
    }
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
            mount_path: String::from("/action_workdir"),
            volume_name: String::from("action-workdir"),
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
            image: format!("{DEFAULT_CONTAINER_REPO}/gitrepo-cloner:{}", env!("CARGO_PKG_VERSION")),
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
            image: format!("{DEFAULT_CONTAINER_REPO}/action-worker:{}", env!("CARGO_PKG_VERSION")),
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
            default_auth_header: String::from(DEFAULT_WEBHOOK_TRIGGER_AUTH_HEADER),
        }
    }
}
