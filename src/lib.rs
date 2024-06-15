pub mod cache;
pub mod cli;
pub mod config;
pub mod controller;
pub mod jobs;
pub mod leader;
pub mod resources;
pub mod signals;
pub mod web;

use thiserror::Error;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Kube Error: {0}")]
    KubeError(
        #[source]
        #[from]
        kube::Error,
    ),

    #[error("Kube Error: {0}")]
    KubertKubeError(
        #[source]
        #[from]
        kubert_kube::Error,
    ),

    #[error("Leader Lock Error: {0}")]
    LeaderLockError(
        #[source]
        #[from]
        kubert::lease::Error,
    ),

    #[error("Finalizer Error: {0}")]
    FinalizerError(
        #[source]
        #[from]
        Box<kube::runtime::finalizer::Error<Error>>,
    ),

    #[error("GitRepo Access Error: {0}")]
    GitrepoAccessError(
        #[source]
        #[from]
        git2::Error,
    ),

    #[error("GitRepo Secret Decoding Error: {0}")]
    SecretDecodingError(String),

    #[error("RuntimeConfig ConfigMap format Error")]
    RuntimeConfigFormatError,

    #[error("Trigger file access IO error: {0}")]
    TriggerFileAccessError(
        #[source]
        #[from]
        std::io::Error,
    ),

    #[error("Trigger dir walk error: {0}")]
    TriggerDirWalkError(String),

    #[error("Trigger file pattern error: {0}")]
    TriggerFilePatternError(
        #[source]
        #[from]
        globwalk::GlobError,
    ),

    #[error("Task scheduler error: {0}")]
    SchedulerError(
        #[source]
        #[from]
        sacs::Error,
    ),

    #[error("Config deserialization error: {0}")]
    ConfigDeserializationError(
        #[source]
        #[from]
        serde_yaml::Error,
    ),

    #[error("Resource not found in cache: {0}")]
    ResourceNotFoundError(String),

    #[error("JobsQueue error: {0}")]
    JobsQueueError(String),
}

impl Error {
    pub fn metric_label(&self) -> String {
        format!("{self:?}").to_lowercase()
    }
}
