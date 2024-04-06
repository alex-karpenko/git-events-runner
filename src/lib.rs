pub mod controllers;
pub mod lock;
pub mod signals;

pub use crate::controllers::action::*;
pub use crate::controllers::git_repo::*;
pub use crate::controllers::trigger::*;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("SerializationError: {0}")]
    SerializationError(#[source] serde_json::Error),

    #[error("Kube Error: {0}")]
    KubeError(#[source] kube::Error),

    #[error("Finalizer Error: {0}")]
    // NB: awkward type because finalizer::Error embeds the reconciler error (which is this)
    // so boxing this error to break cycles
    FinalizerError(#[source] Box<kube::runtime::finalizer::Error<Error>>),

    #[error("GitRepo Access Error: {0}")]
    GitrepoAccessError(#[source] git2::Error),

    #[error("GitRepo Secret Decoding Error: {0}")]
    GitrepoSecretDecodingError(String),

    #[error("Illegal Source")]
    IllegalSource,

    #[error("Wrong Secret Reference")]
    WrongSecretReference,

    #[error("Wrong Repository Authentication Config")]
    WrongAuthConfig,

    #[error("Wrong Source Uri")]
    WrongSourceUri,

    #[error("Wrong TLS Config")]
    WrongTlsConfig,

    #[error("Wrong Trigger Config")]
    WrongTriggerConfig,

    #[error("Trigger file access IO error: {0}")]
    TriggerFileAccessError(#[source] std::io::Error),
}
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub fn metric_label(&self) -> String {
        format!("{self:?}").to_lowercase()
    }
}
