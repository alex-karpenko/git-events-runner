#![deny(unsafe_code, warnings, missing_docs)]
//!

pub mod cache;
pub mod cli;
pub mod config;
pub mod controller;
pub mod jobs;
pub mod resources;
pub mod signals;
pub mod web;

use opentelemetry::trace::TraceId;
use thiserror::Error;

/// Convenient alias for Result with our Error type
pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Error, Debug)]
/// git-events-runner specific errors
pub enum Error {
    /// All errors from the Kubernetes API of `kube` crate
    #[error("Kube Error: {0}")]
    KubeError(
        #[source]
        #[from]
        kube::Error,
    ),

    /// Errors related to leader election management
    #[error("LeaseManager Error: {0}")]
    LeaseManagerError(
        #[source]
        #[from]
        kube_lease_manager::LeaseManagerError,
    ),

    /// Controller runtime specific error related to reconciling
    #[error("Finalizer Error: {0}")]
    FinalizerError(
        #[source]
        #[from]
        Box<kube::runtime::finalizer::Error<Error>>,
    ),

    /// Errors related to retrieving git repo content,
    /// or auth-related access issues
    #[error("GitRepo Access Error: {0}")]
    GitrepoAccessError(
        #[source]
        #[from]
        git2::Error,
    ),

    /// Error during decoding of base64 content of the secrets
    #[error("GitRepo Secret Decoding Error: {0}")]
    SecretDecodingError(String),

    /// Unable to deserialize runtime config due to wrong schema
    #[error("RuntimeConfig ConfigMap format Error")]
    RuntimeConfigFormatError,

    /// Unable to access file during hash calculation
    #[error("Trigger file access IO error: {0}")]
    TriggerFileAccessError(
        #[source]
        #[from]
        std::io::Error,
    ),

    /// Unable to walk thought directories during hash calculation
    #[error("Trigger dir walk error: {0}")]
    TriggerDirWalkError(String),

    /// Incorrect file pattern for hash calculation
    #[error("Trigger file pattern error: {0}")]
    TriggerFilePatternError(
        #[source]
        #[from]
        globwalk::GlobError,
    ),

    /// Error during SACS task scheduling
    #[error("Task scheduler error: {0}")]
    SchedulerError(
        #[source]
        #[from]
        sacs::Error,
    ),

    /// Unable to deserialize runtime config due to incorrect yaml format
    #[error("Config deserialization error: {0}")]
    ConfigDeserializationError(
        #[source]
        #[from]
        serde_yaml::Error,
    ),

    /// Unable to retrieve resource from cache when it should be present in cache
    #[error("Resource not found in cache: {0}")]
    ResourceNotFoundError(String),

    /// Error related to processing of the Jobs queue
    #[error("JobsQueue error: {0}")]
    JobsQueueError(String),
}

///  Fetch an opentelemetry::trace::TraceId as hex through the full tracing stack
pub fn get_trace_id() -> TraceId {
    use opentelemetry::trace::TraceContextExt as _; // opentelemetry::Context -> opentelemetry::trace::Span
    use tracing_opentelemetry::OpenTelemetrySpanExt as _; // tracing::Span to opentelemetry::Context

    tracing::Span::current().context().span().span_context().trace_id()
}
