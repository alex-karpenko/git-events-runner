//! Utility app to clone git repo during Action run.
#![deny(unsafe_code, warnings, missing_docs)]
use clap::{Args, Parser};
use git_events_runner::resources::{
    git_repo::{ClusterGitRepo, GitRepo, GitRepoGetter},
    trigger::{get_latest_commit, TriggerGitRepoReference, TriggerSourceKind},
};
use kube::{Api, Client};
use rustls::crypto::aws_lc_rs;
use tracing::debug;

#[derive(Parser)]
#[command(version)]
#[command(name = "gitrepo-cloner")]
#[command(about = "Git repo cloner, supplementary tool for git-events-runner operator")]
struct Cli {
    /// Source kind
    #[arg(long = "kind", short = 'k')]
    source_kind: TriggerSourceKind,
    /// Source name
    #[arg(long = "source", short = 's')]
    source_name: String,
    /// Reference to clone
    #[command(flatten)]
    reference: ReferenceValue,
    /// Destination folder
    #[arg(long, short)]
    destination: String,
    /// Don't remove .git folder after clone
    #[arg(long, short = 'p')]
    preserve_git_folder: bool,
    /// Set log level to debug
    #[arg(long)]
    debug: bool,
}

#[derive(Args, Debug)]
#[group(required = true, multiple = false)]
struct ReferenceValue {
    /// Branch name
    #[arg(long, short = 'b')]
    branch: Option<String>,
    /// Tag name
    #[arg(long, short = 't')]
    tag: Option<String>,
    /// Commit hash
    #[arg(long, short = 'c')]
    commit: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");
    let cli = Cli::parse();

    if cli.debug {
        std::env::set_var("RUST_LOG", "debug");
    }
    tracing_subscriber::fmt::init();

    let client = Client::try_default().await?;
    git_repo_cloner(cli, client).await
}

async fn git_repo_cloner(cli: Cli, client: Client) -> anyhow::Result<()> {
    let ns = client.default_namespace().to_owned();

    let reference_name = String::from(&cli.reference);
    let reference = TriggerGitRepoReference::from(&cli.reference);
    let repo_name = match cli.source_kind {
        TriggerSourceKind::GitRepo => format!("{}/{}", ns, cli.source_name),
        TriggerSourceKind::ClusterGitRepo => cli.source_name.clone(),
    };
    debug!(kind = %cli.source_kind, repo = %repo_name, %reference, destination= %cli.destination, "cloning source");

    let repo = match cli.source_kind {
        TriggerSourceKind::GitRepo => {
            let api = Api::<GitRepo>::namespaced(client.clone(), &ns);
            let gitrepo = api.get(&cli.source_name).await?;
            gitrepo
                .fetch_repo_ref(client, &reference_name, &cli.destination, &ns)
                .await?
        }
        TriggerSourceKind::ClusterGitRepo => {
            let api = Api::<ClusterGitRepo>::all(client.clone());
            let gitrepo = api.get(&cli.source_name).await?;
            gitrepo
                .fetch_repo_ref(client, &reference_name, &cli.destination, &ns)
                .await?
        }
    };

    get_latest_commit(&repo, &reference)?;
    if !cli.preserve_git_folder {
        debug!("remove .git folder form cloned content");
        tokio::fs::remove_dir_all(format!("{}/.git", cli.destination)).await?;
    }

    Ok(())
}

impl From<&ReferenceValue> for TriggerGitRepoReference {
    fn from(value: &ReferenceValue) -> Self {
        if let Some(branch) = &value.branch {
            TriggerGitRepoReference::Branch(branch.clone())
        } else if let Some(tag) = &value.tag {
            TriggerGitRepoReference::Tag(tag.clone())
        } else if let Some(commit) = &value.commit {
            TriggerGitRepoReference::Commit(commit.clone())
        } else {
            panic!("Looks like a BUG")
        }
    }
}

impl From<&ReferenceValue> for String {
    fn from(value: &ReferenceValue) -> Self {
        if let Some(branch) = &value.branch {
            branch.clone()
        } else if let Some(tag) = &value.tag {
            tag.clone()
        } else if let Some(commit) = &value.commit {
            commit.clone()
        } else {
            panic!("Looks like a BUG")
        }
    }
}
