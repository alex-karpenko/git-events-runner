pub mod cli;
pub mod config;
pub mod controller;
pub mod leader_lock;
pub mod resources;
pub mod secrets_cache;
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
    // NB: awkward type because finalizer::Error embeds the reconciler error (which is this)
    // so boxing this error to break cycles
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

    #[error("Trigger file access IO error: {0}")]
    TriggerFileAccessError(
        #[source]
        #[from]
        std::io::Error,
    ),

    #[error("Task scheduler error: {0}")]
    SchedulerError(
        #[source]
        #[from]
        sacs::Error,
    ),
}

impl Error {
    pub fn metric_label(&self) -> String {
        format!("{self:?}").to_lowercase()
    }
}
