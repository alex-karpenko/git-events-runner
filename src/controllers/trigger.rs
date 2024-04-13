use super::{Context, ControllerType, Reconcilable, SecretRef, TriggersState};
use crate::{
    controllers::{random_string, DateTime, Utc, API_GROUP, CURRENT_API_VERSION},
    Action, Error, GitRepo, Result,
};
use git2::{Oid, Repository};
use k8s_openapi::chrono::SecondsFormat;
use kube::{
    api::{Patch, PatchParams},
    core::object::HasStatus,
    runtime::{
        controller::Action as ReconcileAction,
        events::{Event, EventType, Recorder},
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
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    io,
    sync::Arc,
    time::{Duration, SystemTime},
};
use strum_macros::{Display, EnumString};
use tokio::{
    fs::{create_dir_all, remove_dir_all, try_exists, File},
    io::AsyncReadExt,
    sync::RwLock,
};
use tracing::{debug, error, info, warn};

const DEFAULT_TEMP_DIR: &str = "/tmp/git-event-runner";

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq)]
#[kube(
    kind = "Trigger",
    group = "git-events-runner.rs",
    version = "v1alpha1",
    namespaced,
    printcolumn = r#"{"name":"State", "type":"string", "description":"current trigger state", "jsonPath":".status.state"}"#,
    printcolumn = r#"{"name":"Last Run", "type":"date", "format":"date-time", "description":"time of last trigger run", "jsonPath":".status.lastRun"}"#
)]
#[kube(status = "TriggerStatus")]
#[serde(rename_all = "camelCase")]
pub struct TriggerSpec {
    sources: TriggerSources,
    #[schemars(flatten)]
    #[serde(flatten)]
    pub(crate) trigger: TriggerType,
    action: TriggerAction,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum TriggerType {
    Schedule(TriggerSchedule),
    Webhook(TriggerWebhook),
}

impl TriggerType {
    fn is_processable(&self, cntrl_type: ControllerType) -> bool {
        match self {
            TriggerType::Schedule(_) => cntrl_type == ControllerType::Leader,
            TriggerType::Webhook(_) => cntrl_type == ControllerType::Web,
        }
    }
}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TriggerStatus {
    state: TriggerState,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_run: Option<String>,
    checked_sources: HashMap<String, CheckedSourceState>,
}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CheckedSourceState {
    #[serde(skip_serializing_if = "Option::is_none")]
    commit_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    changed: Option<String>,
}

impl CheckedSourceState {
    fn is_equal(&self, other: Option<&CheckedSourceState>, with_file: bool) -> bool {
        if let Some(other) = other {
            if !with_file {
                self.commit_hash == other.commit_hash
            } else {
                self.file_hash == other.file_hash
            }
        } else {
            self.commit_hash.is_none() && self.file_hash.is_none()
        }
    }
}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema, PartialEq)]
pub enum TriggerState {
    #[default]
    Idle,
    Running,
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
    reference: TriggerGitRepoReference,
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

impl TriggerWatchOn {
    fn reference_name(&self) -> String {
        match &self.reference {
            TriggerGitRepoReference::Branch(r) => r,
            TriggerGitRepoReference::Tag(r) => r,
            TriggerGitRepoReference::Commit(r) => r,
        }
        .to_string()
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

impl TriggerGitRepoReference {
    pub fn to_refname(&self, prefix: Option<&str>) -> (String, &String) {
        let prefix = prefix.unwrap_or("");
        match self {
            TriggerGitRepoReference::Branch(branch) => (format!("{prefix}branch"), branch),
            TriggerGitRepoReference::Tag(tag) => (format!("{prefix}tag"), tag),
            TriggerGitRepoReference::Commit(commit) => (format!("{prefix}commit"), commit),
        }
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
    pub(crate) multi_source: bool,
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

impl Reconcilable for Trigger {
    async fn reconcile(&self, ctx: Arc<Context>) -> Result<ReconcileAction> {
        let client = ctx.client.clone();
        let recorder = &ctx.diagnostics.read().await.recorder(client.clone(), self);
        let ns = self.namespace().unwrap();
        let triggers_api: Api<Trigger> = Api::namespaced(client.clone(), &ns);
        let trigger_key = self.trigger_hash_key();

        if self.spec.trigger.is_processable(ctx.cntrl_type.clone()) {
            // Create trigger status if it's not present yet
            if !ctx
                .triggers
                .read()
                .await
                .statuses
                .contains_key(&self.trigger_hash_key())
            {
                if let Some(status) = self.status() {
                    ctx.triggers
                        .write()
                        .await
                        .statuses
                        .insert(self.trigger_hash_key(), status.clone());
                } else {
                    self.update_trigger_status(
                        None,
                        None,
                        None,
                        ctx.triggers.clone(),
                        &triggers_api,
                    )
                    .await?;
                }
            }

            // Reconcile if something has been changed
            if Some(&self.spec) != ctx.triggers.read().await.specs.get(&trigger_key) {
                // What is changed?
                let is_changed = {
                    let old_trigger = ctx.triggers.read().await;
                    let old_trigger = old_trigger.specs.get(&trigger_key);
                    old_trigger.is_none()
                        || old_trigger.unwrap().sources != self.spec.sources
                        || old_trigger.unwrap().action != self.spec.action
                        || old_trigger.unwrap().trigger != self.spec.trigger
                };

                // Trigger has been changed
                if is_changed {
                    debug!("Update Trigger `{trigger_key}`");
                    let mut triggers = ctx.triggers.write().await;

                    // drop existing task if it was
                    if triggers.tasks.contains_key(&trigger_key) {
                        let task_id = triggers.tasks.remove(&trigger_key).unwrap();
                        let scheduler = ctx.scheduler.write().await;
                        let res = scheduler.cancel(task_id, CancelOpts::Ignore).await;
                        if res.is_err() {
                            error!("Can't cancel task: {res:?}");
                        }
                    }

                    match &self.spec.trigger {
                        TriggerType::Schedule(_) => {
                            // Get time of trigger last run, if it was run
                            let last_run: Option<SystemTime> = if let Some(status) = self.status() {
                                status.last_run.as_ref().map(|last_run| {
                                    DateTime::parse_from_rfc3339(last_run)
                                .expect("Incorrect time format in LastRun field, looks like a BUG.")
                                .into()
                                })
                            } else {
                                None
                            };

                            // Add new task
                            if let Some(schedule) = self.get_task_schedule(last_run) {
                                let scheduler = ctx.scheduler.write().await;
                                let task = self.create_task(
                                    client.clone(),
                                    schedule,
                                    ctx.triggers.clone(),
                                );
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
                        TriggerType::Webhook(_) => {
                            debug!("nothing to reconcile because this's Webhook trigger")
                        }
                    }
                    // Update trigger spec in the map
                    triggers
                        .specs
                        .insert(self.trigger_hash_key(), self.spec.clone());
                }
            }

            Ok(ReconcileAction::await_change())
        } else {
            // If it's not our controller type but we have this trigger in the state -
            // remove it because it means we changes type between scheduled and webhook

            // Remove from triggers map
            if ctx.triggers.read().await.specs.contains_key(&trigger_key) {
                let mut triggers = ctx.triggers.write().await;
                triggers.specs.remove(&trigger_key);
                triggers.statuses.remove(&trigger_key);
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

            Ok(ReconcileAction::await_change())
        }
    }

    async fn cleanup(&self, ctx: Arc<Context>) -> Result<ReconcileAction> {
        if self.spec.trigger.is_processable(ctx.cntrl_type.clone()) {
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
                triggers.statuses.remove(&trigger_key);
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
        }

        Ok(ReconcileAction::await_change())
    }

    fn finalizer_name(&self) -> String {
        String::from("triggers.git-events-runner.rs")
    }

    fn kind(&self) -> &str {
        "Trigger"
    }
}

impl Trigger {
    pub fn trigger_hash_key(&self) -> String {
        format!("{}/{}", self.namespace().unwrap(), self.name_any())
    }

    async fn update_trigger_status(
        &self,
        state: Option<TriggerState>,
        checked_sources: Option<HashMap<String, CheckedSourceState>>,
        last_run: Option<DateTime<Utc>>,
        triggers: Arc<RwLock<TriggersState>>,
        api: &Api<Trigger>,
    ) -> Result<()> {
        let name = self.name_any();
        let trigger_key = self.trigger_hash_key();

        let mut triggers = triggers.write().await;
        let new_status = if let Some(status) = triggers.statuses.get(&trigger_key) {
            TriggerStatus {
                state: state.unwrap_or(status.state.clone()),
                last_run: if let Some(last_run) = last_run {
                    Some(last_run.to_rfc3339_opts(SecondsFormat::Secs, true))
                } else {
                    status.last_run.clone()
                },
                checked_sources: checked_sources.unwrap_or(status.checked_sources.clone()),
            }
        } else {
            TriggerStatus {
                state: state.unwrap_or_default(),
                last_run: last_run
                    .map(|last_run| last_run.to_rfc3339_opts(SecondsFormat::Secs, true)),
                checked_sources: checked_sources.unwrap_or_default(),
            }
        };

        let status = Patch::Apply(json!({
            "apiVersion": format!("{API_GROUP}/{CURRENT_API_VERSION}"),
            "kind": "Trigger",
            "status": {
                "state": new_status.state,
                "lastRun": new_status.last_run,
                "checkedSources": new_status.checked_sources,
            }
        }));
        let pp = PatchParams::apply("cntrlr").force();
        let _o = api
            .patch_status(&name, &pp, &status)
            .await
            .map_err(Error::KubeError)?;

        triggers.statuses.insert(trigger_key, new_status);

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

    pub fn create_task(
        &self,
        client: Client,
        schedule: TaskSchedule,
        triggers: Arc<RwLock<TriggersState>>,
    ) -> Task {
        let trigger_name = self.name_any();
        let trigger_ns = self.namespace().unwrap();
        Task::new(schedule, move |id| {
            let triggers = triggers.clone();
            let trigger_name = trigger_name.clone();
            let trigger_ns = trigger_ns.clone();
            let client = client.clone();
            Box::pin(async move {
                debug!("Start trigger job: trigger={trigger_ns}/{trigger_name}, job id={id}");
                // Get current trigger from API
                let triggers_api: Api<Trigger> = Api::namespaced(client.clone(), &trigger_ns);
                let trigger = triggers_api.get(&trigger_name).await;

                if let Ok(trigger) = trigger {
                    let base_temp_dir = format!(
                        "{DEFAULT_TEMP_DIR}/{trigger_ns}/{trigger_name}/{}",
                        random_string(8)
                    );

                    let checked_sources: &mut HashMap<String, CheckedSourceState> = &mut triggers
                        .read()
                        .await
                        .statuses
                        .get(&trigger.trigger_hash_key())
                        .expect("Looks like a BUG!")
                        .checked_sources
                        .clone()
                        .into_iter()
                        .filter(|(k, _v)| trigger.spec.sources.names.contains(k))
                        .collect();

                    if let Err(err) = trigger
                        .update_trigger_status(
                            Some(TriggerState::Running),
                            None,
                            None,
                            triggers.clone(),
                            &triggers_api,
                        )
                        .await
                    {
                        error!("Unable to update trigger `{}` state: {err:?}", trigger_name);
                    }
                    // For each source
                    for source in trigger
                        .spec
                        .sources
                        .names
                        .iter()
                        .cloned()
                        .collect::<HashSet<String>>()
                    {
                        let mut new_source_state = CheckedSourceState {
                            commit_hash: None,
                            file_hash: None,
                            changed: None,
                        };
                        debug!(
                            "Processing {} source {trigger_ns}/{trigger_name}/{source}",
                            trigger.spec.sources.kind
                        );
                        // - Create temp dest folder
                        let repo_path = format!("{base_temp_dir}/{source}");
                        let _ = remove_dir_all(&repo_path).await; // to prevent error if folder exists by mistake
                        if let Err(err) = create_dir_all(&repo_path).await {
                            error!("Unable to create temporary folder `{repo_path}`: {err:?}");
                        } else {
                            match trigger.spec.sources.kind {
                                // - Get GitRepo object
                                TriggerSourceKind::GitRepo => {
                                    let gitrepo_api: Api<GitRepo> =
                                        Api::namespaced(client.clone(), &trigger_ns);
                                    let gitrepo = gitrepo_api.get(&source).await;
                                    // - Call repo.fetch_repo_ref(...) to get repository
                                    if let Ok(gitrepo) = gitrepo {
                                        let repo = gitrepo
                                            .fetch_repo_ref(
                                                client.clone(),
                                                &trigger.spec.sources.watch_on.reference_name(),
                                                &repo_path,
                                            )
                                            .await;
                                        // - Call get_latest_commit_hash(...) to get latest commit
                                        if let Ok(repo) = repo {
                                            let latest_commit = Self::get_latest_commit(
                                                &repo,
                                                &trigger.spec.sources.watch_on.reference,
                                            );
                                            if let Ok(latest_commit) = latest_commit {
                                                debug!("Latest commit: {:?}", latest_commit);
                                                new_source_state.commit_hash =
                                                    Some(latest_commit.to_string());
                                                // - Get file hash if it's required and present
                                                if let Some(path) =
                                                    &trigger.spec.sources.watch_on.file_
                                                {
                                                    let full_path = format!("{repo_path}/{path}");
                                                    let is_exists = try_exists(&full_path).await;
                                                    if is_exists.is_ok() && is_exists.unwrap() {
                                                        let file_hash =
                                                            Self::get_file_hash(&full_path).await;
                                                        if let Ok(file_hash) = file_hash {
                                                            debug!("File hash: {file_hash}");
                                                            new_source_state.file_hash =
                                                                Some(file_hash);
                                                        } else {
                                                            error!(
                                                                    "Unable to calc file `{path}` hash: {:?}",
                                                                    file_hash.err()
                                                                );
                                                        }
                                                    } else {
                                                        warn!("File `{path}` doesn't exits");
                                                    }
                                                }
                                            } else {
                                                error!("Unable to get latest commit from GitRepo `{source}`: {:?} ", latest_commit.err());
                                                continue;
                                            }
                                        } else {
                                            error!(
                                                "Unable to fetch GitRepo `{}` from remote: {:?}",
                                                source,
                                                repo.err()
                                            );
                                            continue;
                                        }
                                    } else {
                                        error!(
                                            "Unable to get GitRepo {trigger_ns}/{source}: {:?}",
                                            gitrepo.err()
                                        );
                                        continue;
                                    }
                                }
                                TriggerSourceKind::ClusterGitRepo => todo!(),
                            }
                            // - Get self.status... latest processed hash for current source
                            // - If it differs from latest commit or onChangesOnly==false - run action
                            let current_source_state = checked_sources.get(&source);
                            if !new_source_state.is_equal(
                                current_source_state,
                                trigger.spec.sources.watch_on.file_.is_some(),
                            ) || !trigger.spec.sources.watch_on.on_change_only
                            {
                                let action_exec_result = match trigger.spec.action.kind {
                                    TriggerActionKind::Action => {
                                        let actions_api: Api<Action> =
                                            Api::namespaced(client.clone(), &trigger_ns);
                                        let action =
                                            actions_api.get(&trigger.spec.action.name).await;
                                        match action {
                                            Ok(action) => {
                                                // TODO: Should we retry in case of error?
                                                action
                                                    .execute(
                                                        &trigger.spec.sources.kind,
                                                        &source,
                                                        &new_source_state
                                                            .commit_hash
                                                            .clone()
                                                            .unwrap(),
                                                        &trigger.spec.sources.watch_on.reference,
                                                        client.clone(),
                                                    )
                                                    .await
                                            }
                                            Err(err) => Err(Error::KubeError(err)),
                                        }
                                    }
                                    TriggerActionKind::ClusterAction => todo!(),
                                };
                                // - Update self.spec.... by latest processed commit
                                match action_exec_result {
                                    Ok(_) => {
                                        new_source_state.changed = Some(
                                            Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
                                        );
                                        checked_sources.insert(source, new_source_state);
                                        if let Err(err) = trigger
                                            .update_trigger_status(
                                                Some(TriggerState::Running),
                                                Some(checked_sources.clone()),
                                                Some(Utc::now()),
                                                triggers.clone(),
                                                &triggers_api,
                                            )
                                            .await
                                        {
                                            error!(
                                                "Unable to update trigger `{}` state: {err:?}",
                                                trigger_name
                                            );
                                        }
                                    }
                                    Err(err) => error!(
                                        "Unable to run {} {}/{}: {}",
                                        trigger.spec.action.kind,
                                        trigger_ns,
                                        trigger.spec.action.name,
                                        err
                                    ),
                                }
                            } else {
                                if let Some(current_source_state) = current_source_state {
                                    new_source_state
                                        .changed
                                        .clone_from(&current_source_state.changed);
                                }
                                checked_sources.insert(source, new_source_state);
                            }
                            // - Remove temp dir content
                            if let Err(err) = remove_dir_all(&repo_path).await {
                                error!("Unable to remove temporary folder `{repo_path}`: {err:?}");
                            }
                        }
                    }
                    if let Err(err) = trigger
                        .update_trigger_status(
                            Some(TriggerState::Idle),
                            Some(checked_sources.to_owned()),
                            Some(Utc::now()),
                            triggers.clone(),
                            &triggers_api,
                        )
                        .await
                    {
                        error!("Unable to update trigger `{}` state: {err:?}", trigger_name);
                    }
                } else {
                    error!(
                        "Unable to get Trigger {trigger_ns}/{trigger_name}: {:?}",
                        trigger.err()
                    );
                }

                debug!("Finish trigger job: trigger={trigger_ns}/{trigger_name}, job id={id}");
            })
        })
    }

    fn get_task_schedule(&self, last_run: Option<SystemTime>) -> Option<TaskSchedule> {
        match &self.spec.trigger {
            TriggerType::Schedule(schedule) => {
                if schedule.is_valid() {
                    let schedule = match schedule {
                        TriggerSchedule::Interval(interval) => {
                            let interval: Duration = interval
                                .parse::<humantime::Duration>()
                                .expect("unable to parse time interval, looks like a BUG")
                                .into();
                            if let Some(last_run) = last_run {
                                let since_last_run = SystemTime::now()
                                    .duration_since(last_run)
                                    .expect("wrong time of trigger last run, looks like a BUG");
                                let delay = interval
                                    .checked_sub(since_last_run)
                                    .unwrap_or(Duration::from_secs(0));
                                TaskSchedule::IntervalDelayed(interval, delay)
                            } else {
                                TaskSchedule::Interval(interval)
                            }
                        }
                        TriggerSchedule::Cron(cron) => {
                            let cron = CronSchedule::try_from(cron).unwrap();
                            TaskSchedule::Cron(
                                cron,
                                CronOpts {
                                    at_start: last_run.is_none(),
                                    concurrent: false,
                                },
                            )
                        }
                    };
                    Some(schedule)
                } else {
                    None
                }
            }
            TriggerType::Webhook(_) => Some(TaskSchedule::Once),
        }
    }

    pub fn get_latest_commit(
        repo: &Repository,
        reference: &TriggerGitRepoReference,
    ) -> Result<Oid> {
        let oid = match reference {
            TriggerGitRepoReference::Branch(r) => repo
                .resolve_reference_from_short_name(&format!("origin/{r}"))
                .map_err(Error::GitrepoAccessError)?
                .target()
                .unwrap(),
            TriggerGitRepoReference::Tag(r) => repo
                .resolve_reference_from_short_name(r)
                .map_err(Error::GitrepoAccessError)?
                .target()
                .unwrap(),
            TriggerGitRepoReference::Commit(r) => {
                Oid::from_str(r).map_err(Error::GitrepoAccessError)?
            }
        };

        let ref_obj = repo.find_object(oid, None).unwrap();
        repo.checkout_tree(&ref_obj, None)
            .map_err(Error::GitrepoAccessError)?;

        Ok(oid)
    }

    async fn get_file_hash(path: &String) -> Result<String> {
        let mut hasher = Sha256::new();
        let mut buf: Vec<u8> = Vec::new();

        File::open(&path)
            .await
            .map_err(Error::TriggerFileAccessError)?
            .read_to_end(&mut buf)
            .await
            .map_err(Error::TriggerFileAccessError)?;

        let mut buf = buf.as_slice();
        io::copy(&mut buf, &mut hasher).map_err(Error::TriggerFileAccessError)?;
        let hash = hasher.finalize();

        Ok(format!("{hash:x}"))
    }
}
