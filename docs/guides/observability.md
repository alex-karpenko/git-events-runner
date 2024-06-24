# Observability

## Metrics

Controller exposes a set of metrics to make possible state monitoring, alerting and efficiency analyzing.   
All metrics have the same prefix ("namespace") which may be configured using a command line option `--metrics-prefix`
or `controllerOptions.metricsPrefix` Helm chart parameter.
The default prefix is `git_events_runner`.

Below is a detailed explanation of all the metrics.
Metric names are specified without the prefix (just for the sake of more compact presentation).

### Controller metrics

| Name                       | Type      | Labels                                          | Description                                                                   |
|----------------------------|-----------|-------------------------------------------------|-------------------------------------------------------------------------------|
| reconcile_count            | Counter   | namespace <br/>resource_kind <br/>resource_name | Total number of reconciliations (updates) of each resource, including failed. |
| failed_reconcile_count     | Counter   | namespace <br/>resource_kind <br/>resource_name | Number of failed reconciliations.                                             |
| reconcile_duration_seconds | Histogram | namespace <br/>resource_kind <br/>resource_name | Reconciliation duration for each resource.                                    |

> Note: `ScheduleTriggers` need to be reconciled so far. All other resources have no external state.

### Triggers metrics

| Name                           | Type      | Labels                                        | Description                                             |
|--------------------------------|-----------|-----------------------------------------------|---------------------------------------------------------|
| trigger_check_count            | Counter   | namespace <br/>trigger_kind <br/>trigger_name | Total number of source checks executed by each trigger. |
| trigger_check_duration_seconds | Histogram | namespace <br/>trigger_kind <br/>trigger_name | Execution duration of source checks for each trigger.   |

### Webhooks metrics

These metrics are related to incoming webhook requests processing.

| Name                              | Type      | Labels                                  | Description                                                                                                                 |
|-----------------------------------|-----------|-----------------------------------------|-----------------------------------------------------------------------------------------------------------------------------|
| webhook_requests_count            | Counter   | namespace <br/>trigger_name <br/>status | Total number of webhook requests served by each trigger.                                                                    |
| webhook_requests_duration_seconds | Histogram | namespace <br/>trigger_name <br/>status | HTTP request processing duration for each trigger. <br/>This is time to schedule check task but not time of task execution. |

> Notes:
> - Since all webhook triggers have the same resource kind (`WebhookTrigger`) corresponding label is omitted.
> - `status` label represents HTTP response status code which was returned on request.

### Jobs metrics

These metrics are related to actual action jobs executing.

| Name                                  | Type      | Labels                                                                                                                        | Description                                                                                                                                                                 |
|---------------------------------------|-----------|-------------------------------------------------------------------------------------------------------------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| jobs_queue_limit                      | Gauge     | -                                                                                                                             | Current limit of the simultaneously running Jobs (actual `action.maxRunningJobs` config parameter).                                                                         |
| jobs_queue_waiting                    | Gauge     | -                                                                                                                             | Current number of Jobs waiting for start in the queue.                                                                                                                      |
| jobs_queue_running                    | Gauge     | -                                                                                                                             | Current number of running Jobs.                                                                                                                                             |
| jobs_queue_waiting_duration_seconds   | Histogram | -                                                                                                                             | Duration of time each Job spent in the waiting queue before starting.                                                                                                       |
| jobs_queue_completed_count            | Counter   | namespace <br/>trigger_kind <br/>trigger_name <br/>source_kind <br/>source_name <br/>action_kind <br/>action_name <br/>status | Total number of completed Jobs with respect to trigger, source, action and final status.                                                                                    |
| jobs_queue_completed_duration_seconds | Histogram | namespace <br/>trigger_kind <br/>trigger_name <br/>source_kind <br/>source_name <br/>action_kind <br/>action_name <br/>status | Duration of time each Job was in a running state. <br/>This metric is actual for finished (successfully of failed) jobs only. <br/>It's absent for deleted or expired Jobs. |

Status label explanation:

| Value       | Description                                                                                                                                                 |
|-------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Succeed     | Job finished successfully.                                                                                                                                  |
| Failed      | Job finished but failed.                                                                                                                                    |
| Expired     | Job was not running because it was waiting in the queue more than its waiting time limit, defined by config or action parameter `jobWaitingTimeoutSeconds`. |
| Deleted     | Job was deleted before it was finished.                                                                                                                     |
| CreateError | Job was not created due to Kubernetes error.                                                                                                                |
