use clap::{Args, Parser};
use controllers::{GitRepo, ScheduleTrigger, TriggerGitRepoReference, TriggerSourceKind};
use kube::{Api, Client};
use tracing::debug;

#[derive(Parser)]
#[command(version)]
#[command(name = "gitrepo-cloner")]
#[command(about = "Git repo cloner, supplementary tool for git-events-runner operator")]
struct Cli {
    /// Source kind
    #[arg(long = "kind", short = 'k')]
    source_kind: TriggerSourceKind,
    /// Source namespace (for namespaced types)
    #[arg(long = "namespace", short = 'n')]
    source_namespace: Option<String>,
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
    let cli = Cli::parse();

    if cli.debug {
        std::env::set_var("RUST_LOG", "debug");
    }
    tracing_subscriber::fmt::init();

    let ns = cli.source_namespace.unwrap_or("default".to_string());
    let client = Client::try_default()
        .await
        .expect("failed to create kube Client");

    let reference_name = String::from(&cli.reference);
    let reference = TriggerGitRepoReference::from(&cli.reference);
    let repo_name = match cli.source_kind {
        TriggerSourceKind::GitRepo => format!("{}/{}", ns, cli.source_name),
        TriggerSourceKind::ClusterGitRepo => cli.source_name.clone(),
    };
    debug!(
        "Cloning source: kind={}, name={}, reference={:?}, destination={}",
        cli.source_kind, repo_name, reference, cli.destination
    );

    let repo = match cli.source_kind {
        TriggerSourceKind::GitRepo => {
            let api = Api::<GitRepo>::namespaced(client.clone(), &ns);
            let gitrepo = api.get(&cli.source_name).await?;
            gitrepo
                .fetch_repo_ref(client, &reference_name, &cli.destination)
                .await?
        }
        TriggerSourceKind::ClusterGitRepo => todo!(),
    };

    ScheduleTrigger::get_latest_commit(&repo, &reference)?;
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
