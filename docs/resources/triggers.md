# Triggers

Both types of the triggers are almost identical except one principal section:

* `ScheduleTrigger` has a **schedule** section which describes how often it should check sources for changes.
* `WebhookTrigger` has a **webhook** section which describes triggers behavior on HTTP requests.

**sources** section contains list (one or more) of sources to check.
Every time ScheduleTrigger runs, it checks all sources one by one and creates Job for each source that has been changed
from the previous check.

It's possible to specify additional constrain on the source check: list of the file glob patterns, that should be
changed to get the trigger fired.
Adding or removing files, as well as changing of any file content (hash), means that source has been changed.
Pattern format is a well-known `.gitignore` format, described in details in
the [git documentation](https://git-scm.com/docs/gitignore#_pattern_format).

WebhookTrigger can be called by specifying path to the trigger in one of the two forms:

* By trigger name without a source name: `/namespace/trigger`, like `/default/trigger-example`.
  In this case, trigger checks all defined sources one by one like a ScheduleTrigger.
* With full source name: /namespace/trigger/source, like `/default/trigger-example/source-repo-1`.
  In this case, trigger check specified source only.

Second form is disabled by default to avoid possible unpredictable workload burst, and may be enabled by `multiSource`
flag.

If `authConfig` section of the WebhookTrigger is defined,
authorization header should be present in each request to the trigger.
Header name may be changed.

## ScheduleTrigger

```yaml title="ScheduleTrigger"
--8<-- "docs/examples/scheduletrigger.yaml"
```

## WebhookTrigger

```yaml title="WebhookTrigger"
--8<-- "docs/examples/webhooktrigger.yaml"
```
