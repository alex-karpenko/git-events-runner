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

Allowed values: `error`, `warn`, `info`, `debug`, `trace`.

Using this parameter, you can change log level.
If nothing is specified the default level is `info`.

### otlp

This section describes OpenTelemetry configuration.
If `otlp.enabled` is `true` then `otlp.endpoint` should point to the OpenTelemetry collector endpoint.

### leaderLease

This section declares the parameters of using Lease resource to manage leader elections. There are two parameters:

* `duration`: leader election will be started if the current leaseholder doesn't confirm its lock during this time (in
  seconds).
* `grace`: leaseholder re-confirms (renews) its lock the grace interval (in seconds) before lock expires.

Logically, grace should be less than duration and the grace interval should be enough to re-confirm lock.

### scheduleParallelism, webhooksParallelism

Allowed values: integer from 1 to 255.

These parameters set, for each type of triggers, the maximum number of parallel tasks that the controller runs to watch
for changes in the sources.

> __Important__: This is not a maximum number of simultaneous action jobs.
> This is a way to restrict controller only from running of huge number of simultaneous source verification tasks.
> So there is no way (at least now) to restrict number of action jobs.

### webhooksRateLimits

This section defines Webhooks requests rate limits.
By default, the controller doesn't restrict the rate of the webhook requests,
but rate limiting is a good approach to prevent request flood in some environments,
especially if you're not responsible for configuration of the calling side.

Limits' values are defined as `number-of-requests/period-in-seconds`, for example, "1/10" or "20/60".
But at the same time, the number of requests defines the size of the "burst bucket"
— the number of requests that will be accepted before the rate limiter starts request throttling.
So two equal limits like "1/10" and "10/100" have slightly different meaning:

- 1/10 means that the controller accepts one request and blocks all others during 10 seconds;
- 10/100 means that the controller accepts the first 10 requests and starts blocking all others for 10 seconds, and
  returns one "free seat" into the burst bucket every 10 seconds.

There are three types of limits:

* `global` - limits total webhooks request rate regardless of the trigger of source;
* `trigger` - per trigger limit which restricts rate for each webhook trigger separately, it can't exceed global
  value;
* `source` - per trigger source restriction, it works as previous one but uses each source of each trigger as a rate
  limiting object.

### webhooksTls

This section defines optional TLS config for webhooks server.
If it's omitted (default), webhooks listener works without TLS.

To force webhooks server to use TLS transport, we have to provide a valid server certificate and its private key in PEM
format.
There are two possible ways to do this:

- provide files with certificate and key (or mount Secret volume inside a Pod);
- use existing Secret of the `kubernetes.io/tls` type.

TLS configuration may be provided using the following config parameters:

- `certificate`: path to the file with certificate in PEM format;
- `key`: path to the file with certificate private key in PEM format;
- `secret`: name of the TLS secret to load certificate and key from;
- `secretNamespace`: namespace where certificate secret is located.

If you specified one of the `certificate` or `key`, the second option becomes mandatory because certificate doesn't work
without a key and vise versa.

`certificate/key` and `secret/secretNamespace` are mutually exclusive.
is optional, if it's not specified default namespace will be used: if controller is running inside Kubernetes cluster
(usual way) the default is controllers' namespace, otherwise namespace `default` will be used.

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

### metricsPrefix

The parameter specifies prefix ("namespace") to use for Prometheus metrics.
The default value is `git_events_runner`.

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
| activeDeadlineSeconds             | 3600                                                                           | Default time limit to run Job. After this time incomplete Job will be terminated.                                                          |
| maxRunningJobs                    | 16                                                                             | Maximum number of simultaneously running Jobs, per controller replica. Jobs that can't be running will be queued and waiting.              |
| jobWaitingTimeoutSeconds          | 300                                                                            | Maximum time (in seconds) Jobs can wait in queue before start because `maxRunningJobs` is exceeded.                                        |
| defaultServiceAccount             | `{{ fullname }}-action-job`                                                    | Default service account name for action jobs. Actual default depends on release name and `fullnameOverride` global parameter.              |
| workdir.mountPath                 | /action_workdir                                                                | Default folder to clone source content to. It's used for both cloner and worker container.                                                 |
| workdir.volumeName                | action-workdir                                                                 | Volume name of workdir `emptyDir` volume.                                                                                                  |
| containers.cloner.name            | action-cloner                                                                  | Name of the source cloner initContainer in the action Job.                                                                                 |
| containers.cloner.image           | ghcr.io/alex-karpenko/git-events-runner/gitrepo-cloner:{{ .Chart.AppVersion }} | Default image to use for source cloner container.                                                                                          |
| containers.worker.name            | action-worker                                                                  | Name of the action worker container in the action Job.                                                                                     |
| containers.worker.image           | ghcr.io/alex-karpenko/git-events-runner/action-worker:{{ .Chart.AppVersion }}  | Default image to use for the action worker container.                                                                                      |
| containers.worker.variablesPrefix | ACTION_JOB_                                                                    | Prefix for environment variables of the action worker container, that controller creates to pass trigger and sources parameters.           |

## Command line parameters

Controller application has three subcommands:

* `crds`: to print CRD manifests to the stdout.
  It's useful to install CRD declarations to your cluster directly.
  It has no additional options.
* `config`: to print a default dynamic config to the stdout.
  It's useful to get base config and modify parts you want to customize.
  Has only one optional parameter `--helm-template` to dump some templates instead of default values:
  this option is useful in CI/CD pipelines to automate Helm chart linting and templating.
* `run`: to run controller, all command line parameters are optional and have defaults.
  All of them are described as a static configuration options above in this section.

```bash
docker run --rm ghcr.io/alex-karpenko/git-events-runner/git-events-runner:latest --help

Kubernetes operator to run Jobs on events from Git

Usage: git-events-runner <COMMAND>

Commands:
  crds    Print CRD definitions to stdout
  config  Print default dynamic config YAML to stdout
  run     Run K8s controller
  help    Print this message or the help of the given subcommand(s)

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
          Leader lease grace interval, seconds [default: 5]
      --metrics-prefix <METRICS_PREFIX>
          Name of the ConfigMap with dynamic controller config [default: git_events_runner]
      --hooks-rrl-global <HOOKS_RRL_GLOBAL>
          Global webhooks requests rate limit (burst limit/seconds)
      --hooks-rrl-trigger <HOOKS_RRL_TRIGGER>
          Requests rate limit (burst limit/seconds) per webhook trigger
      --hooks-rrl-source <HOOKS_RRL_SOURCE>
          Requests rate limit (burst limit/seconds) per webhook source
      --tls-cert <TLS_CERT>
          Path to TLS certificate file
      --tls-key <TLS_KEY>
          Path to TLS key file
      --tls-secret-name <TLS_SECRET_NAME>
          Secret name with TLS certificate and key
      --tls-secret-namespace <TLS_SECRET_NAMESPACE>
          Namespace of the  TLS secret
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
