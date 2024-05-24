# Configuration

The Helm chart has two dedicated controllers' configuration sections, pretty well commented in
the [`values.yaml`](https://github.com/alex-karpenko/helm-charts/blob/main/charts/git-events-runner/values.yaml):

* `controllerOptions`: defines values of some controllers' command line options.
* `runtimeConfig`: defines content of the ConfigMap with dynamic controllers' config, or, in other words, with
  overridden defaults.
  It will be reloaded by controller each time it's changed.

Let's explain all of them with some details.

## Controller options

### logLevel

Allowed values: `info`, `debug`.

Controller hs three possible log levels: debug, info, warning.
If nothing is specified (this is equivalent to omit both `--debug` and `--verbose` command line options) the default
level is warning.
Using this parameter, you can change log level.

### leaderLease

This section declares the parameters of using Lease resource to manage leader elections. There are two parameters:

* `duration`: leader election will be started if the current leaseholder doesn't confirm its lock during this time (in
  seconds).
* `grace`: leaseholder re-confirms (renews) its lock each this interval (in seconds).

Logically, grace should be less than duration and difference between these two parameters should be enough to re-confirm
lock.

### scheduleParallelism, webhooksParallelism

Allowed values: integer from 1 to 255.

These parameters set, for each type of triggers, the maximum number of parallel tasks that the controller runs to watch
for changes in the sources.

> __Important__: This is not a maximum number of simultaneous action jobs.
> This is a way to restrict controller only from running of huge number of simultaneous source verification tasks.
> So there is no way (at least now) to restrict number of action jobs.

### secretsCacheTime

This parameter specifies the maximum number of seconds to hold values in the cache of secrets.

Controller resolves content of the secrets each time it needs secret value (for example, WebhookTrigger can use auth
token stored in a secret).
To eliminate Kubernetes API overloading by huge number of requests for secret values, the controller has some kind of
shared cache for secrets.
It holds only vales used by controller.

### sourceCloneFolder

This section defines which folder (`mountPath`) will be used inside the controller to clone content of the sources
during periodic
verification for changes.

At the same time, parameter `volumeName` defines name of the Pods' volume, which should be used for that folder.
Helm chart defines controllers' Pod with this volume of type `emptyDir`.

## Runtime config

### trigger

Defines triggers defaults:

* `webhook.defaultAuthHeader`: default header name with authentication token in a WebhookTrigger request. May be changed
  in the trigger config. Default is `x-trigger-auth`.

### action

This section defines lots of defaults of action jobs.

| Parameter name                    | Default value                                                                  | Description                                                                                                                                |
|-----------------------------------|--------------------------------------------------------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------|
| ttlSecondsAfterFinished           | 7200                                                                           | Default time to leave of Job after finishing. After this time Jobs will be removed from the cluster with its Pod. Useful fo debug purpose. |
| defaultServiceAccount             | `{{ fullname }}-action-job`                                                    | Default service account name for action jobs. Actual default depends on release name and `fullnameOverride` global parameter.              | 
| workdir.mountPath                 | /action_workdir                                                                | Default folder to clone source content to. It's used for both cloner and worker container.                                                 |
| workdir.volumeName                | action-workdir                                                                 | Volume name of workdir `emptyDir` volume.                                                                                                  |
| containers.cloner.name            | action-cloner                                                                  | Name of the source cloner initContainer in the action Job.                                                                                 |
| containers.cloner.image           | ghcr.io/alex-karpenko/git-events-runner/gitrepo-cloner:{{ .Chart.AppVersion }} | Default image to use for source cloner container.                                                                                          |
| containers.worker.name            | action-worker                                                                  | Name of the action worker container in the action Job.                                                                                     |
| containers.worker.image           | ghcr.io/alex-karpenko/git-events-runner/action-worker:{{ .Chart.AppVersion }}  | Default image to use for the action worker container.                                                                                      |
| containers.worker.variablesPrefix | ACTION_JOB_                                                                    | Prefix for environment variables of the action worker container, that controller creates to pass trigger and sources parameters.           |

## Command line parameters

Controller application has two subcommands:

* `crds`: to print CRD manifests to the stdout.
  It's useful to install CRD declarations to your cluster directly.
  It has no additional options.
* `run`: to run controller, all command line parameters are optional and have defaults.
  All of them are described as a static configuration options above in this section.

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
