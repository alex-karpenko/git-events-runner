use super::{Context, Reconcilable};
use crate::{Error, Result, TriggerGitRepoReference, TriggerSourceKind};
use k8s_openapi::{
    api::{
        batch::v1::{Job, JobSpec},
        core::v1::{
            Container, EmptyDirVolumeSource, EnvVar, PodSpec, PodTemplateSpec, Volume, VolumeMount,
        },
    },
    chrono::{DateTime, Local},
};
use kube::{
    api::{ObjectMeta, PostParams},
    runtime::controller::Action as ReconcileAction,
    Api, Client, CustomResource, Resource, ResourceExt,
};
use rand::{distributions::Alphanumeric, thread_rng, Rng};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc, time::SystemTime};
use tracing::info;

// TODO: this should be config
const DEFAULT_ACTION_WORKDIR: &str = "/action_workdir";
const DEFAULT_CLONER_IMAGE: &str = "ghcr.io/alex-karpenko/git-events-runner/gitrepo-cloner:latest";
const DEFAULT_ACTION_IMAGE: &str = "docker.io/bash:latest";
const DEFAULT_ACTION_WORKDIR_VOLUME_NAME: &str = "action-workdir";
const DEFAULT_ACTION_INIT_CONTAINER_NAME: &str = "action-init";
const DEFAULT_ACTION_WORKER_CONTAINER_NAME: &str = "action-worker";
const DEFAULT_ACTION_JOB_ENV_VARS_PREFIX: &str = "ACTION_JOB_";

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq)]
#[cfg_attr(test, derive(Default))]
#[kube(
    kind = "Action",
    group = "git-events-runner.rs",
    version = "v1alpha1",
    namespaced
)]
#[kube(status = "ActionStatus")]
#[serde(rename_all = "camelCase")]
pub struct ActionSpec {
    #[serde(skip_serializing_if = "Option::is_none")]
    source_override: Option<ActionSourceOverride>,
    #[serde(default)]
    action_job: ActionJob,
}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ActionStatus {}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ActionSourceOverride {
    kind: TriggerSourceKind,
    name: String,
    reference: TriggerGitRepoReference,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ActionJob {
    #[serde(default = "ActionJob::default_workdir")]
    workdir: String,
    #[serde(default = "ActionJob::default_cloner_image")]
    cloner_image: String,
    #[serde(default = "ActionJob::default_action_image")]
    action_image: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    args: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    service_account: Option<String>,
    #[serde(default)]
    enable_cloner_debug: bool,
    #[serde(default)]
    preserve_git_folder: bool,
}

impl Default for ActionJob {
    fn default() -> Self {
        Self {
            workdir: String::from(DEFAULT_ACTION_WORKDIR),
            cloner_image: String::from(DEFAULT_CLONER_IMAGE),
            action_image: String::from(DEFAULT_ACTION_IMAGE),
            args: None,
            command: None,
            service_account: None,
            enable_cloner_debug: false,
            preserve_git_folder: false,
        }
    }
}

impl ActionJob {
    fn default_workdir() -> String {
        String::from(DEFAULT_ACTION_WORKDIR)
    }
    fn default_action_image() -> String {
        String::from(DEFAULT_ACTION_IMAGE)
    }
    fn default_cloner_image() -> String {
        String::from(DEFAULT_CLONER_IMAGE)
    }
}

impl Reconcilable for Action {
    async fn reconcile(&self, _ctx: Arc<Context>) -> Result<ReconcileAction> {
        // If no events were received, check back 30 minutes
        Ok(ReconcileAction::await_change())
    }

    async fn cleanup(&self, _ctx: Arc<Context>) -> Result<ReconcileAction> {
        info!(
            "Cleanup Action `{}` in {}",
            self.name_any(),
            self.namespace().unwrap()
        );
        Ok(ReconcileAction::await_change())
    }

    fn finalizer_name(&self) -> String {
        String::from("actions.git-events-runner.rs")
    }

    fn kind(&self) -> &str {
        "Action"
    }
}

impl Action {
    pub(crate) async fn execute(
        &self,
        source_kind: &TriggerSourceKind,
        source_name: &str,
        source_commit: &str,
        trigger_ref: &TriggerGitRepoReference,
        client: Client,
    ) -> Result<Job> {
        let ns = self.namespace().unwrap();
        let job = self.create_job_spec(source_kind, source_name, source_commit, trigger_ref)?;
        let jobs_api: Api<Job> = Api::namespaced(client.clone(), &ns);

        info!("Create job {}/{}", job.namespace().unwrap(), job.name_any());
        jobs_api
            .create(&PostParams::default(), &job)
            .await
            .map_err(Error::KubeError)
    }

    fn get_gitrepo_cloner_args(
        &self,
        source_kind: &TriggerSourceKind,
        source_name: &str,
        source_ref: &TriggerGitRepoReference,
    ) -> Vec<String> {
        let mut args = vec![
            "--kind".into(),
            source_kind.to_string(),
            "--source".into(),
            source_name.into(),
            "--destination".into(),
            self.spec.action_job.workdir.clone(),
        ];

        let (ref_type, ref_name) = source_ref.to_refname(Some("--"));
        args.push(ref_type);
        args.push(ref_name.clone());

        if source_kind == &TriggerSourceKind::GitRepo {
            args.push("--namespace".into());
            args.push(self.namespace().unwrap());
        }

        if self.spec.action_job.preserve_git_folder {
            args.push("--preserve-git-folder".into());
        }

        if self.spec.action_job.enable_cloner_debug {
            args.push("--debug".into());
        }

        args
    }

    fn get_action_job_envs(
        &self,
        source_kind: &TriggerSourceKind,
        source_name: &str,
        source_commit: &str,
        trigger_ref: &TriggerGitRepoReference,
    ) -> Vec<EnvVar> {
        let s = &self.spec;
        let (trigger_ref_type, trigger_ref_name) = trigger_ref.to_refname(None);
        let mut envs = vec![
            Self::create_action_job_env_var("WORKDIR", &s.action_job.workdir),
            Self::create_action_job_env_var("TRIGGER_SOURCE_KIND", &source_kind.to_string()),
            Self::create_action_job_env_var("TRIGGER_SOURCE_NAME", source_name),
            Self::create_action_job_env_var("TRIGGER_SOURCE_COMMIT", source_commit),
            Self::create_action_job_env_var("TRIGGER_SOURCE_REF_TYPE", &trigger_ref_type),
            Self::create_action_job_env_var("TRIGGER_SOURCE_REF_NAME", trigger_ref_name),
        ];

        if source_kind == &TriggerSourceKind::GitRepo {
            envs.push(Self::create_action_job_env_var(
                "TRIGGER_SOURCE_NAMESPACE",
                &self.namespace().unwrap(),
            ));
        }

        if let Some(source_override) = &s.source_override {
            let (ref_type, ref_name) = source_override.reference.to_refname(None);

            envs.push(Self::create_action_job_env_var(
                "ACTION_SOURCE_KIND",
                &source_override.kind.to_string(),
            ));
            envs.push(Self::create_action_job_env_var(
                "ACTION_SOURCE_NAME",
                &source_override.name,
            ));
            envs.push(Self::create_action_job_env_var(
                "ACTION_SOURCE_REF_TYPE",
                ref_type.as_str(),
            ));
            envs.push(Self::create_action_job_env_var(
                "ACTION_SOURCE_REF_NAME",
                ref_name,
            ));
            if source_override.kind == TriggerSourceKind::GitRepo {
                envs.push(Self::create_action_job_env_var(
                    "ACTION_SOURCE_NAMESPACE",
                    &self.namespace().unwrap(),
                ));
            }
        }

        envs
    }

    fn create_action_job_env_var(name: &str, value: &str) -> EnvVar {
        EnvVar {
            name: format!("{DEFAULT_ACTION_JOB_ENV_VARS_PREFIX}{name}"),
            value: Some(value.to_owned()),
            value_from: None,
        }
    }

    fn create_job_spec(
        &self,
        source_kind: &TriggerSourceKind,
        source_name: &str,
        source_commit: &str,
        trigger_ref: &TriggerGitRepoReference,
    ) -> Result<Job> {
        let mut labels: BTreeMap<String, String> = BTreeMap::new();
        labels.insert("git-events-runner.rs/job".into(), "true".into());
        labels.insert("git-events-runner.rs/action-kind".into(), "Action".into());
        labels.insert("git-events-runner.rs/action-name".into(), self.name_any());
        labels.insert(
            "git-events-runner.rs/source-kind".into(),
            source_kind.to_string(),
        );
        labels.insert(
            "git-events-runner.rs/source-name".into(),
            source_name.into(),
        );

        let args = if let Some(source_override) = &self.spec.source_override {
            self.get_gitrepo_cloner_args(
                &source_override.kind,
                &source_override.name,
                &source_override.reference,
            )
        } else {
            self.get_gitrepo_cloner_args(
                source_kind,
                source_name,
                &TriggerGitRepoReference::Commit(source_commit.into()),
            )
        };

        let job = Job {
            metadata: ObjectMeta {
                // TODO: make func
                name: Some(self.job_name()),
                namespace: Some(self.namespace().unwrap()),
                labels: Some(labels),
                owner_references: Some(vec![self.controller_owner_ref(&()).unwrap()]),
                ..Default::default()
            },
            spec: Some(JobSpec {
                backoff_limit: Some(0),
                parallelism: Some(1),
                ttl_seconds_after_finished: Some(300),
                template: PodTemplateSpec {
                    metadata: Default::default(),
                    spec: Some(PodSpec {
                        service_account_name: self.spec.action_job.service_account.clone(),
                        init_containers: Some(vec![Container {
                            name: DEFAULT_ACTION_INIT_CONTAINER_NAME.into(),
                            image: Some(self.spec.action_job.cloner_image.clone()),
                            args: Some(args),
                            volume_mounts: Some(vec![VolumeMount {
                                name: DEFAULT_ACTION_WORKDIR_VOLUME_NAME.into(),
                                mount_path: self.spec.action_job.workdir.clone(),
                                ..Default::default()
                            }]),
                            ..Default::default()
                        }]),
                        containers: vec![Container {
                            name: DEFAULT_ACTION_WORKER_CONTAINER_NAME.into(),
                            image: Some(self.spec.action_job.action_image.clone()),
                            working_dir: Some(self.spec.action_job.workdir.clone()),
                            command: self.spec.action_job.command.clone(),
                            args: self.spec.action_job.args.clone(),
                            env: Some(self.get_action_job_envs(
                                source_kind,
                                source_name,
                                source_commit,
                                trigger_ref,
                            )),
                            volume_mounts: Some(vec![VolumeMount {
                                name: DEFAULT_ACTION_WORKDIR_VOLUME_NAME.into(),
                                mount_path: self.spec.action_job.workdir.clone(),
                                ..Default::default()
                            }]),
                            ..Default::default()
                        }],
                        restart_policy: Some("Never".into()),
                        volumes: Some(vec![Volume {
                            name: DEFAULT_ACTION_WORKDIR_VOLUME_NAME.into(),
                            empty_dir: Some(EmptyDirVolumeSource::default()),
                            ..Default::default()
                        }]),
                        ..Default::default()
                    }),
                },
                ..Default::default()
            }),
            ..Default::default()
        };

        Ok(job)
    }

    fn job_name(&self) -> String {
        let timestamp = DateTime::<Local>::from(SystemTime::now())
            .format("%Y%m%d-%H%M%S")
            .to_string();

        format!(
            "{}-{}-{}",
            self.name_any(),
            timestamp,
            Self::random_string(2)
        )
        .to_lowercase()
    }

    fn random_string(len: usize) -> String {
        let rand: String = thread_rng()
            .sample_iter(&Alphanumeric)
            .take(len)
            .map(char::from)
            .collect();
        rand
    }
}
