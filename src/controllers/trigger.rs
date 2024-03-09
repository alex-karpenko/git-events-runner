use super::{Context, SecretRef};
use crate::{
    controllers::{API_GROUP, CURRENT_API_VERSION},
    Error, Result,
};
use k8s_openapi::api::core::v1::Secret;
use kube::{
    api::{Patch, PatchParams},
    core::object::HasStatus,
    runtime::{
        controller::Action,
        events::{Event, EventType, Recorder},
        finalizer::{finalizer, Event as Finalizer},
    },
    Api, Client, CustomResource, ResourceExt,
};
use sacs::{
    scheduler::{CancelOpts, TaskScheduler},
    task::{CronOpts, CronSchedule, Task, TaskSchedule},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};
use strum_macros::{Display, EnumString};
use tracing::{debug, error, info, warn};

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq)]
#[cfg_attr(test, derive(Default))]
#[kube(
    kind = "Trigger",
    group = "git-events-runner.rs",
    version = "v1alpha1",
    namespaced
)]
#[kube(status = "TriggerStatus")]
#[serde(rename_all = "camelCase")]
pub struct TriggerSpec {
    sources: TriggerSources,
    #[serde(skip_serializing_if = "Option::is_none")]
    schedule: Option<TriggerSchedule>,
    #[serde(skip_serializing_if = "Option::is_none")]
    webhook: Option<TriggerWebhook>,
    action: TriggerAction,
}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TriggerStatus {
    state: TriggerState,
    checked_sources: HashMap<String, CheckedSourceRef>,
}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CheckedSourceRef {
    #[serde(skip_serializing_if = "Option::is_none")]
    commit_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_hash: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema, PartialEq)]
pub enum TriggerState {
    #[default]
    Pending,
    Ready,
    WrongConfig,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TriggerSources {
    #[serde(default)]
    kind: TriggerSourceKind,
    names: Vec<String>,
    #[serde(default)]
    watch_on: TriggerWatchOn,
}

#[derive(
    Deserialize, Serialize, Clone, Debug, Default, JsonSchema, EnumString, Display, PartialEq,
)]
#[serde(rename_all = "PascalCase")]
#[strum(serialize_all = "PascalCase")]
pub enum TriggerSourceKind {
    #[default]
    GitRepo,
    ClusterGitRepo,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TriggerWatchOn {
    on_change_only: bool,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    reference: Option<TriggerGitRepoReference>,
    #[serde(rename = "file")]
    #[serde(skip_serializing_if = "Option::is_none")]
    file_: Option<String>,
}

impl Default for TriggerWatchOn {
    fn default() -> Self {
        Self {
            on_change_only: true,
            reference: Default::default(),
            file_: None,
        }
    }
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum TriggerGitRepoReference {
    Branch(String),
    Tag(String),
    Commit(String),
}

impl Default for TriggerGitRepoReference {
    fn default() -> Self {
        Self::Branch(String::from("main"))
    }
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum TriggerSchedule {
    Interval(String),
    Cron(String),
}

impl TriggerSchedule {
    fn is_valid(&self) -> bool {
        match self {
            TriggerSchedule::Interval(interval) => interval.parse::<humantime::Duration>().is_ok(),
            TriggerSchedule::Cron(cron) => CronSchedule::try_from(cron).is_ok(),
        }
    }
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TriggerWebhook {
    multi_source: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    auth_config: Option<TriggerWebhookAuthConfig>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TriggerWebhookAuthConfig {
    secret_ref: SecretRef,
    key: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TriggerAction {
    kind: TriggerActionKind,
    name: String,
}

#[derive(
    Deserialize, Serialize, Clone, Debug, Default, JsonSchema, EnumString, Display, PartialEq,
)]
#[serde(rename_all = "PascalCase")]
#[strum(serialize_all = "PascalCase")]

pub enum TriggerActionKind {
    #[default]
    Action,
    ClusterAction,
}

pub(crate) async fn reconcile(trigger: Arc<Trigger>, ctx: Arc<Context>) -> Result<Action> {
    let ns = trigger.namespace().unwrap();
    let triggers: Api<Trigger> = Api::namespaced(ctx.client.clone(), &ns);

    debug!("{:#?}", trigger);
    info!("Reconciling Trigger `{}` in {}", trigger.name_any(), ns);
    finalizer(
        &triggers,
        "triggers.git-events-runner.rs",
        trigger,
        |event| async {
            match event {
                Finalizer::Apply(trigger) => trigger.reconcile(ctx.clone()).await,
                Finalizer::Cleanup(trigger) => trigger.cleanup(ctx.clone()).await,
            }
        },
    )
    .await
    .map_err(|e| Error::FinalizerError(Box::new(e)))
}

pub(crate) fn error_policy(_trigger: Arc<Trigger>, error: &Error, _ctx: Arc<Context>) -> Action {
    warn!("reconcile failed: {:?}", error);
    Action::await_change()
}

impl Trigger {
    fn trigger_hash_key(&self) -> String {
        format!("{}/{}", self.namespace().unwrap(), self.name_any())
    }

    // Reconcile (for non-finalizer related changes)
    async fn reconcile(&self, ctx: Arc<Context>) -> Result<Action> {
        let client = ctx.client.clone();
        let recorder = &ctx.diagnostics.read().await.recorder(client.clone(), self);
        let ns = self.namespace().unwrap();
        let triggers_api: Api<Trigger> = Api::namespaced(client.clone(), &ns);
        let trigger_key = self.trigger_hash_key();

        // Reconcile is something has been changed
        if Some(&self.spec) != ctx.triggers.read().await.specs.get(&trigger_key) {
            let mut triggers = ctx.triggers.write().await;
            let old_trigger = triggers.specs.get(&trigger_key);
            let is_changed = old_trigger.is_none()
                || old_trigger.unwrap().sources != self.spec.sources
                || old_trigger.unwrap().action != self.spec.action;

            // Schedule has been changed
            if is_changed || old_trigger.unwrap().schedule != self.spec.schedule {
                info!("Update schedule for `{trigger_key}`");
                // drop existing task if it was
                let is_new_task = if triggers.tasks.contains_key(&trigger_key) {
                    let task_id = triggers.tasks.remove(&trigger_key).unwrap();
                    let scheduler = ctx.scheduler.write().await;
                    let res = scheduler.cancel(task_id, CancelOpts::Ignore).await;
                    if res.is_err() {
                        error!("Can't cancel task: {res:?}");
                    }
                    false
                } else {
                    true
                };

                // Add new task if schedule is present
                if self.spec.schedule.is_some() {
                    if let Some(schedule) = self.get_task_schedule(is_new_task) {
                        let scheduler = ctx.scheduler.write().await;
                        let task = self.get_scheduled_task(schedule);
                        let task_id = scheduler.add(task).await.unwrap(); // TODO: get rid of unwrap
                        let tasks = &mut triggers.tasks;
                        tasks.insert(self.trigger_hash_key(), task_id);
                    } else {
                        // Invalid schedule
                        self.publish_trigger_validation_event(
                            recorder,
                            EventType::Warning,
                            "Unable to parse schedule expression, scheduler is disabled",
                            "ValidateTriggerConfig",
                        )
                        .await?;
                    }
                }
            }

            // TODO: Webhook has been changed
            if is_changed || old_trigger.unwrap().webhook != self.spec.webhook {
                info!("Update webhook for `{trigger_key}`");
                // TODO: Drop existing server if it was

                if let Some(webhook) = &self.spec.webhook {
                    // TODO: Create new web-server
                    // Verify auth config
                    if let Some(auth_config) = &webhook.auth_config {
                        let secret_name = &auth_config.secret_ref.name;
                        let keys_to_check = [auth_config.key.clone()];
                        let result = self
                            .check_secret_ref(
                                client.clone(),
                                secret_name,
                                &keys_to_check,
                                Error::WrongTriggerConfig,
                            )
                            .await;
                        if result.is_err() {
                            self.publish_trigger_validation_event(
                                recorder,
                                EventType::Warning,
                                format!(
                                    "Auth config may be wrong: secret `{secret_name}` should exist and contain `{}` key(s)",
                                    keys_to_check.join(",")).as_str(),
                                    "ValidateTriggerConfig"
                                ).await?;
                        }
                    }
                }
            }



            // Update trigger spec in the map
            triggers
                .specs
                .insert(self.trigger_hash_key(), self.spec.clone());
        }

        //  - schedule or webhook or both should be defined, error if neither is
        if (self.spec.webhook.is_none() && self.spec.schedule.is_none())
            || (self.spec.schedule.is_some() && !self.spec.schedule.clone().unwrap().is_valid())
        {
            self.update_resource_state(TriggerState::WrongConfig, &triggers_api)
                .await?;
            self.publish_trigger_validation_event(
                recorder,
                EventType::Warning,
                "At least one of `schedule` or `webhook` should be configured or valid",
                "ValidateTriggerConfig",
            )
            .await?;
            return Err(Error::WrongTriggerConfig);
        }

        // Always overwrite status object with what we saw
        self.update_resource_state(TriggerState::Ready, &triggers_api)
            .await?;
        // If no events were received, check back 30 minutes
        Ok(Action::requeue(Duration::from_secs(30 * 60)))
    }

    // Finalizer cleanup (the object was deleted, ensure nothing is orphaned)
    async fn cleanup(&self, ctx: Arc<Context>) -> Result<Action> {
        info!(
            "Cleanup Trigger `{}` in {}",
            self.name_any(),
            self.namespace().unwrap()
        );
        let trigger_key = self.trigger_hash_key();
        // Remove from triggers map
        if ctx.triggers.read().await.specs.contains_key(&trigger_key) {
            let mut triggers = ctx.triggers.write().await;
            triggers.specs.remove(&trigger_key);
        }
        // Drop task and remove from tasks map
        if ctx.triggers.read().await.tasks.contains_key(&trigger_key) {
            let mut triggers = ctx.triggers.write().await;
            let task_id = triggers.tasks.remove(&trigger_key).unwrap();
            let scheduler = ctx.scheduler.write().await;
            let res = scheduler.cancel(task_id, CancelOpts::Kill).await;
            if res.is_err() {
                error!("Can't cancel task: {res:?}");
            }
        }
        // TODO: Drop web-server and remove from servers map
        Ok(Action::await_change())
    }

    async fn update_resource_state(&self, state: TriggerState, api: &Api<Trigger>) -> Result<()> {
        let name = self.name_any();
        let checked_sources = if let Some(status) = self.status() {
            status.checked_sources.clone()
        } else {
            HashMap::<String, CheckedSourceRef>::new()
        };

        let new_status = Patch::Apply(json!({
            "apiVersion": format!("{API_GROUP}/{CURRENT_API_VERSION}"),
            "kind": "Trigger",
            "status": {
                "state": state,
                "checkedSources": checked_sources,
            }
        }));
        let ps = PatchParams::apply("cntrlr").force();
        let _o = api
            .patch_status(&name, &ps, &new_status)
            .await
            .map_err(Error::KubeError)?;

        Ok(())
    }

    async fn publish_trigger_validation_event(
        &self,
        recorder: &Recorder,
        type_: EventType,
        note: &str,
        action: &str,
    ) -> Result<()> {
        recorder
            .publish(Event {
                type_,
                reason: "ValidateTrigger".into(),
                note: Some(note.into()),
                action: action.into(),
                secondary: None,
            })
            .await
            .map_err(Error::KubeError)?;

        Ok(())
    }

    // TODO: remove Duplicate with GitRepo
    async fn check_secret_ref(
        &self,
        client: Client,
        secret_name: &str,
        keys: &[String],
        error: Error,
    ) -> Result<()> {
        let ns = self.namespace().unwrap();
        let secret_api: Api<Secret> = Api::namespaced(client, &ns);
        let expected_keys: HashSet<&String> = keys.iter().collect();

        let secret_ref = secret_api
            .get(secret_name)
            .await
            .map_err(Error::KubeError)?;

        debug!("secret_ref={secret_ref:#?}");
        debug!("expected_keys={expected_keys:?}");

        if let Some(data) = secret_ref.data {
            // Check data field
            let secret_keys: HashSet<&String> = data.keys().collect();
            debug!("data_keys={secret_keys:?}");
            if secret_keys.intersection(&expected_keys).count() == expected_keys.len() {
                return Ok(());
            };
        }

        if let Some(data) = secret_ref.string_data {
            // Check string_data field
            let secret_keys: HashSet<&String> = data.keys().collect();
            debug!("string_data_keys={secret_keys:?}");
            if secret_keys.intersection(&expected_keys).count() == expected_keys.len() {
                return Ok(());
            };
        }

        Err(error)
    }

    fn get_scheduled_task(&self, schedule: TaskSchedule) -> Task {
        let trigger_key = self.trigger_hash_key();
        Task::new(schedule, move |id| {
            let trigger_key = trigger_key.clone();
            Box::pin(async move {
                // TODO: Update to real task and move to method
                info!("Start trigger job: trigger={trigger_key}, job id={id}");
                tokio::time::sleep(Duration::from_secs(5)).await;
                info!("Finish trigger job: trigger={trigger_key}, job id={id}");
            })
        })
    }

    fn get_task_schedule(&self, is_new_task: bool) -> Option<TaskSchedule> {
        if let Some(schedule) = &self.spec.schedule {
            if schedule.is_valid() {
                let schedule = match schedule {
                    TriggerSchedule::Interval(interval) => {
                        if is_new_task {
                            TaskSchedule::Interval(
                                interval.parse::<humantime::Duration>().unwrap().into(),
                            )
                        } else {
                            TaskSchedule::IntervalDelayed(
                                interval.parse::<humantime::Duration>().unwrap().into(),
                            )
                        }
                    }
                    TriggerSchedule::Cron(cron) => {
                        let cron = CronSchedule::try_from(cron).unwrap();
                        TaskSchedule::Cron(
                            cron,
                            CronOpts {
                                at_start: is_new_task,
                                concurrent: false,
                            },
                        )
                    }
                };
                Some(schedule)
            } else {
                None
            }
        } else {
            None
        }
    }
}
