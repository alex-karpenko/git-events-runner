use clap::Parser;
use controllers::{GitRepo, Trigger, TriggerSourceKind};
use kube::{Api, Client};

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
    /// Commit hash
    #[arg(long, short)]
    commit: String,
    /// Destination folder
    #[arg(long, short)]
    destination: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    let client = Client::try_default()
        .await
        .expect("failed to create kube Client");

    let repo = match cli.source_kind {
        TriggerSourceKind::GitRepo => {
            let ns = cli.source_namespace.unwrap_or("default".to_string());
            let api = Api::<GitRepo>::namespaced(client.clone(), &ns);
            let gitrepo = api.get(&cli.source_name).await?;
            gitrepo
                .fetch_repo_ref(client, &cli.commit, &cli.destination)
                .await?
        }
        TriggerSourceKind::ClusterGitRepo => todo!(),
    };

    Trigger::get_latest_commit(
        &repo,
        &controllers::TriggerGitRepoReference::Commit(cli.commit),
    )?;
    tokio::fs::remove_dir_all(format!("{}/.git", cli.destination)).await?;

    Ok(())
}
