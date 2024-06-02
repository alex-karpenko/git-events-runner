use super::{
    random_string,
    trigger::{TriggerGitRepoReference, TriggerSourceKind},
    CustomApiResource,
};
use crate::{config::RuntimeConfig, Error, Result};
use chrono::{DateTime, Local};
use k8s_openapi::{
    api::{
        batch::v1::{Job, JobSpec},
        core::v1::{
            Affinity, Container, EmptyDirVolumeSource, EnvVar, PodSpec, PodTemplateSpec, Toleration, Volume,
            VolumeMount,
        },
    },
    apimachinery::pkg::apis::meta::v1::OwnerReference,
};
use kube::{
    api::{ObjectMeta, PostParams},
    Api, Client, CustomResource, Resource, ResourceExt,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, time::SystemTime};
use strum::{Display, EnumString};
use tracing::info;

pub const ACTION_JOB_IDENTITY_LABEL: &str = "git-events-runner.rs/controller-identity";
pub const ACTION_JOB_ACTION_KIND_LABEL: &str = "git-events-runner.rs/action-kind";
pub const ACTION_JOB_ACTION_NAME_LABEL: &str = "git-events-runner.rs/action-name";
pub const ACTION_JOB_SOURCE_KIND_LABEL: &str = "git-events-runner.rs/source-kind";
pub const ACTION_JOB_SOURCE_NAME_LABEL: &str = "git-events-runner.rs/source-name";

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq)]
#[cfg_attr(test, derive(Default))]
#[kube(
    kind = "Action",
    group = "git-events-runner.rs",
    version = "v1alpha1",
    doc = "Action custom resource definition",
    namespaced
)]
#[serde(rename_all = "camelCase")]
pub struct ActionSpec {
    #[serde(skip_serializing_if = "Option::is_none")]
    source_override: Option<ActionSourceOverride>,
    #[serde(default)]
    action_job: ActionJob,
}

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq)]
#[cfg_attr(test, derive(Default))]
#[kube(
    kind = "ClusterAction",
    group = "git-events-runner.rs",
    version = "v1alpha1",
    doc = "ClusterAction custom resource definition"
)]
#[serde(rename_all = "camelCase")]
pub struct ClusterActionSpec {
    #[serde(skip_serializing_if = "Option::is_none")]
    source_override: Option<ClusterActionSourceOverride>,
    #[serde(default)]
    action_job: ActionJob,
}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ActionSourceOverride {
    kind: TriggerSourceKind,
    name: String,
    reference: TriggerGitRepoReference,
}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ClusterActionSourceOverride {
    kind: TriggerClusterSourceKind,
    name: String,
    reference: TriggerGitRepoReference,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema, EnumString, Display, PartialEq)]
#[serde(rename_all = "PascalCase")]
#[strum(serialize_all = "PascalCase")]
pub enum TriggerClusterSourceKind {
    #[default]
    ClusterGitRepo,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct ActionJob {
    #[serde(skip_serializing_if = "Option::is_none")]
    workdir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cloner_image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    action_image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    args: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    service_account: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    annotations: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    labels: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tolerations: Option<Vec<Toleration>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    affinity: Option<Affinity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    node_selector: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ttl_seconds_after_finished: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    active_deadline_seconds: Option<i64>,
    enable_cloner_debug: bool,
    preserve_git_folder: bool,
}

impl CustomApiResource for Action {
    fn kind(&self) -> &str {
        "Action"
    }
}

impl CustomApiResource for ClusterAction {
    fn kind(&self) -> &str {
        "ClusterAction"
    }
}

impl ActionExecutor for Action {}
impl ActionExecutor for ClusterAction {}

#[allow(private_bounds)]
pub(crate) trait ActionExecutor: ActionInternals {
    /// Creates K8s Job spec and run actual job from it
    #[allow(clippy::too_many_arguments)]
    async fn execute(
        &self,
        source_kind: &TriggerSourceKind,
        source_name: &str,
        source_commit: &str,
        trigger_ref: &TriggerGitRepoReference,
        client: Client,
        ns: &str,
        identity: String,
    ) -> Result<Job> {
        let job = self.create_job_spec(source_kind, source_name, source_commit, trigger_ref, ns, identity)?;

        info!("Create job {}/{}", ns, job.name_any());
        let jobs_api: Api<Job> = Api::namespaced(client.clone(), ns);
        jobs_api
            .create(&PostParams::default(), &job)
            .await
            .map_err(Error::KubeError)
    }
}

trait ActionInternals: Sized + Resource + CustomApiResource {
    fn action_job_spec(&self) -> ActionJob;
    fn source_override_spec(&self) -> Option<ActionSourceOverride>;
    fn get_owner_reference(&self) -> OwnerReference;

    /// format job name
    fn job_name(&self) -> String {
        let timestamp = DateTime::<Local>::from(SystemTime::now())
            .format("%Y%m%d-%H%M%S")
            .to_string();

        format!("{}-{}-{}", self.name_any(), timestamp, random_string(2)).to_lowercase()
    }

    /// just create K8s Job specification
    fn create_job_spec(
        &self,
        source_kind: &TriggerSourceKind,
        source_name: &str,
        source_commit: &str,
        trigger_ref: &TriggerGitRepoReference,
        ns: &str,
        identity: String,
    ) -> Result<Job> {
        let config = RuntimeConfig::get();
        let action_job = self.action_job_spec();
        let source_override = self.source_override_spec();

        // Create pod metadata if additional labels/annotations is defined
        let pod_template_metadata = if action_job.labels.is_some() || action_job.annotations.is_some() {
            let metadata = ObjectMeta {
                labels: action_job.labels.clone(),
                annotations: action_job.annotations.clone(),
                ..Default::default()
            };

            Some(metadata)
        } else {
            None
        };

        // Fill out custom jobs' labels
        let mut labels: BTreeMap<String, String> = BTreeMap::new();
        labels.insert(ACTION_JOB_IDENTITY_LABEL.into(), identity);
        labels.insert(ACTION_JOB_ACTION_KIND_LABEL.into(), self.kind().to_string());
        labels.insert(ACTION_JOB_ACTION_NAME_LABEL.into(), self.name_any());
        labels.insert(ACTION_JOB_SOURCE_KIND_LABEL.into(), source_kind.to_string());
        labels.insert(ACTION_JOB_SOURCE_NAME_LABEL.into(), source_name.into());

        // And add additional labels if defined
        if let Some(additional_labels) = action_job.labels {
            labels.extend(additional_labels);
        }

        let args = if let Some(source_override) = source_override {
            self.get_gitrepo_cloner_args(&source_override.kind, &source_override.name, &source_override.reference)
        } else {
            self.get_gitrepo_cloner_args(
                source_kind,
                source_name,
                &TriggerGitRepoReference::Commit(source_commit.into()),
            )
        };

        let service_account = if let Some(sa) = action_job.service_account {
            Some(sa)
        } else {
            RuntimeConfig::get().action.default_service_account.clone()
        };

        let ttl_seconds_after_finished = action_job
            .ttl_seconds_after_finished
            .unwrap_or(RuntimeConfig::get().action.ttl_seconds_after_finished);
        let active_deadline_seconds = action_job
            .active_deadline_seconds
            .unwrap_or(RuntimeConfig::get().action.active_deadline_seconds);

        let job = Job {
            metadata: ObjectMeta {
                name: Some(self.job_name()),
                namespace: Some(ns.to_string()),
                labels: Some(labels),
                annotations: action_job.annotations,
                owner_references: Some(vec![self.get_owner_reference()]),
                ..Default::default()
            },
            spec: Some(JobSpec {
                backoff_limit: Some(0),
                parallelism: Some(1),
                ttl_seconds_after_finished: Some(ttl_seconds_after_finished),
                active_deadline_seconds: Some(active_deadline_seconds),
                template: PodTemplateSpec {
                    metadata: pod_template_metadata,
                    spec: Some(PodSpec {
                        service_account_name: service_account,
                        node_selector: action_job.node_selector,
                        affinity: action_job.affinity,
                        tolerations: action_job.tolerations,
                        init_containers: Some(vec![Container {
                            name: config.action.containers.cloner.name.clone(),
                            image: Some(
                                action_job
                                    .cloner_image
                                    .unwrap_or(config.action.containers.cloner.image.clone()),
                            ),
                            args: Some(args),
                            volume_mounts: Some(vec![VolumeMount {
                                name: config.action.workdir.volume_name.clone(),
                                mount_path: action_job
                                    .workdir
                                    .clone()
                                    .unwrap_or(config.action.workdir.mount_path.clone()),
                                ..Default::default()
                            }]),
                            ..Default::default()
                        }]),
                        containers: vec![Container {
                            name: config.action.containers.worker.name.clone(),
                            image: Some(
                                action_job
                                    .action_image
                                    .unwrap_or(config.action.containers.worker.image.clone()),
                            ),
                            working_dir: Some(
                                action_job
                                    .workdir
                                    .clone()
                                    .unwrap_or(config.action.workdir.mount_path.clone()),
                            ),
                            command: action_job.command.clone(),
                            args: action_job.args,
                            env: Some(self.get_action_job_envs(
                                source_kind,
                                source_name,
                                source_commit,
                                trigger_ref,
                                ns,
                            )),
                            volume_mounts: Some(vec![VolumeMount {
                                name: config.action.workdir.volume_name.clone(),
                                mount_path: action_job.workdir.unwrap_or(config.action.workdir.mount_path.clone()),
                                ..Default::default()
                            }]),
                            ..Default::default()
                        }],
                        restart_policy: Some("Never".into()),
                        volumes: Some(vec![Volume {
                            name: config.action.workdir.volume_name.clone(),
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

    /// Prepare list of special environment variables to pass to worker container
    /// use `format_action_job_env_var` helper to unify approach
    fn get_action_job_envs(
        &self,
        source_kind: &TriggerSourceKind,
        source_name: &str,
        source_commit: &str,
        trigger_ref: &TriggerGitRepoReference,
        ns: &str,
    ) -> Vec<EnvVar> {
        let action_job = self.action_job_spec();
        let source_override = self.source_override_spec();

        let (trigger_ref_type, trigger_ref_name) = trigger_ref.to_refname(None);
        let mut envs = vec![
            format_action_job_env_var(
                "WORKDIR",
                &action_job
                    .workdir
                    .unwrap_or(RuntimeConfig::get().action.workdir.mount_path.clone()),
            ),
            format_action_job_env_var("TRIGGER_SOURCE_KIND", &source_kind.to_string()),
            format_action_job_env_var("TRIGGER_SOURCE_NAME", source_name),
            format_action_job_env_var("TRIGGER_SOURCE_COMMIT", source_commit),
            format_action_job_env_var("TRIGGER_SOURCE_REF_TYPE", &trigger_ref_type),
            format_action_job_env_var("TRIGGER_SOURCE_REF_NAME", trigger_ref_name),
        ];

        if source_kind == &TriggerSourceKind::GitRepo {
            envs.push(format_action_job_env_var("TRIGGER_SOURCE_NAMESPACE", ns));
        }

        if let Some(source_override) = source_override {
            let (ref_type, ref_name) = source_override.reference.to_refname(None);

            envs.push(format_action_job_env_var(
                "ACTION_SOURCE_KIND",
                &source_override.kind.to_string(),
            ));
            envs.push(format_action_job_env_var("ACTION_SOURCE_NAME", &source_override.name));
            envs.push(format_action_job_env_var("ACTION_SOURCE_REF_TYPE", ref_type.as_str()));
            envs.push(format_action_job_env_var("ACTION_SOURCE_REF_NAME", ref_name));
            if source_override.kind == TriggerSourceKind::GitRepo {
                envs.push(format_action_job_env_var("ACTION_SOURCE_NAMESPACE", ns));
            }
        }

        envs
    }

    /// prepares all needed arguments for cloner container entrypoint
    fn get_gitrepo_cloner_args(
        &self,
        source_kind: &TriggerSourceKind,
        source_name: &str,
        source_ref: &TriggerGitRepoReference,
    ) -> Vec<String> {
        let action_job = self.action_job_spec();

        let mut args = vec![
            "--kind".into(),
            source_kind.to_string(),
            "--source".into(),
            source_name.into(),
            "--destination".into(),
            action_job
                .workdir
                .clone()
                .unwrap_or(RuntimeConfig::get().action.workdir.mount_path.clone()),
        ];

        let (ref_type, ref_name) = source_ref.to_refname(Some("--"));
        args.push(ref_type);
        args.push(ref_name.clone());

        if self.action_job_spec().preserve_git_folder {
            args.push("--preserve-git-folder".into());
        }

        if action_job.enable_cloner_debug {
            args.push("--debug".into());
        }

        args
    }
}

impl ActionInternals for ClusterAction {
    fn action_job_spec(&self) -> ActionJob {
        self.spec.action_job.clone()
    }

    fn source_override_spec(&self) -> Option<ActionSourceOverride> {
        // Since ClusterAction restricts repo type to ClusterGitRepo only,
        // we create ActionSourceOverride from ClusterActionSourceOverride
        self.spec.source_override.as_ref().map(|value| ActionSourceOverride {
            kind: TriggerSourceKind::ClusterGitRepo,
            name: value.name.clone(),
            reference: value.reference.clone(),
        })
    }

    /// returns reference to action to set as owner to Job
    fn get_owner_reference(&self) -> OwnerReference {
        self.controller_owner_ref(&())
            .expect("unable to get owner reference, looks like a BUG!")
    }
}

impl ActionInternals for Action {
    fn action_job_spec(&self) -> ActionJob {
        self.spec.action_job.clone()
    }

    fn source_override_spec(&self) -> Option<ActionSourceOverride> {
        self.spec.source_override.clone()
    }

    fn get_owner_reference(&self) -> OwnerReference {
        self.controller_owner_ref(&())
            .expect("unable to get owner reference, looks like a BUG!")
    }
}

/// helper to create unified env var.
/// uses configured prefix to attach to each var.
fn format_action_job_env_var(name: &str, value: &str) -> EnvVar {
    EnvVar {
        name: format!(
            "{}{name}",
            RuntimeConfig::get().action.containers.worker.variables_prefix.clone()
        ),
        value: Some(value.to_owned()),
        value_from: None,
    }
}
