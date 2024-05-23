# Configuration

TBW

## Controller config

TBW

### Override Defaults

TBW

### Command line parameters

```bash
docker run --rm ghcr.io/alex-karpenko/git-events-runner/git-events-runner:latest --help

Kubernetes operator to run Jobs on events from Git

Usage: git-events-runner <COMMAND>

Commands:
  crds  Print CRD definitions to stdout
  run   Run K8s controller
  help  Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
```

Controller app has two subcommands:

* `crds`: to print CRD manifests to the stdout.
  It's useful to install CRD declarations to your cluster directly.
  It has no additional options.
* `run`: to run controller, all command line parameters are optional and have defaults.
  All of them are described as a static configuration options above in this section.

```bash
docker run --rm ghcr.io/alex-karpenko/git-events-runner/git-events-runner:latest run --help

Run K8s controller

Usage: git-events-runner run [OPTIONS]

Options:
  -w, --webhooks-port <WEBHOOKS_PORT>
          Port to listen on for webhooks [default: 8080]
  -u, --utility-port <UTILITY_PORT>
          Port to listen on for utilities web [default: 3000]
      --webhooks-parallelism <WEBHOOKS_PARALLELISM>
          Maximum number of webhook triggers running in parallel [default: 16]
      --schedule-parallelism <SCHEDULE_PARALLELISM>
          Maximum number of schedule triggers running in parallel [default: 16]
      --secrets-cache-time <SECRETS_CACHE_TIME>
          Seconds to cache secrets for [default: 60]
      --source-clone-folder <SOURCE_CLONE_FOLDER>
          Path (within container) to clone repo to [default: /tmp/git-events-runner]
      --config-map-name <CONFIG_MAP_NAME>
          Name of the ConfigMap with dynamic controller config [default: git-events-runner-config]
      --leader-lease-name <LEADER_LEASE_NAME>
          Name of the Lease for leader locking [default: git-events-runner-leader-lock]
      --leader-lease-duration <LEADER_LEASE_DURATION>
          Leader lease duration, seconds [default: 30]
      --leader-lease-grace <LEADER_LEASE_GRACE>
          Leader lease grace interval, seconds [default: 20]
  -d, --debug
          Enable extreme logging (debug)
  -v, --verbose
          Enable additional logging (info)
  -h, --help
          Print help
```

## gitrepo-cloner options

Usually you don't use this image (or app) directly, but the controller runs Jobs with `gitrepo-cloner` init image.
It doesn't have configuration files but command line options only.

Just for the sake of curiosity or for debugging purpose:

```bash
docker run --rm ghcr.io/alex-karpenko/git-events-runner/gitrepo-cloner:latest --help

Git repo cloner, supplementary tool for git-events-runner operator

Usage: gitrepo-cloner [OPTIONS] --kind <SOURCE_KIND> --source <SOURCE_NAME> --destination <DESTINATION> <--branch <BRANCH>|--tag <TAG>|--commit <COMMIT>>

Options:
  -k, --kind <SOURCE_KIND>         Source kind
  -s, --source <SOURCE_NAME>       Source name
  -b, --branch <BRANCH>            Branch name
  -t, --tag <TAG>                  Tag name
  -c, --commit <COMMIT>            Commit hash
  -d, --destination <DESTINATION>  Destination folder
  -p, --preserve-git-folder        Don't remove .git folder after clone
      --debug                      Set log level to debug
  -h, --help                       Print help
  -V, --version                    Print version
```

> Note: `--branch`, `--tag` and `--commit` options are mutually exclusive, but one of them is mandatory;
