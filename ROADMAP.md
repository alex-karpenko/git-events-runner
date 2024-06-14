# Roadmap

## In work

- chart: update to specify container registry and use docker.io by default.

## Next release

- controller: metrics

## Wishes

- tests: automate everything possible
- refactor: looks like scheduler shouldn't be under RwLock because it's `add` method uses internal mutability
- gitrepo: update to use `gix` instead of `git2`, if possible
- hooks: implement hook requests rate control/throttling
- hooks/gitrepo: rework secrets cache to watch requested secrets and update it on changes
- cli: deploy, config, get state, remove
- cli: configure webhook auth secrets and store them as hashes
- hooks: tls listener for webhooks
- gitrepo: add support for private keys with passphrase
- gitrepo: use Mozilla CA bundle instead of system and build controller/cloner images `FROM scratch`
- controller: get rid of kubert dependency

## Done

- images: publish images to docker hub
- images: update default action-worker image to use the latest Ubuntu LTS version
- chart/cli: Make new subcommand to dump out default config and verify it in the chart as part of CI
- action:
    - config parameters to specify additional annotations and labels for action job;
    - config parameters to specify node affinity/selector, toleration;
    - config to restrict jobs' duration;
    - config to restrict the maximum number of running acton jobs:
- gitrepo: extend file sensor to use globs instead of single file
- doc: make documentation site versioned.
- refactor: improve logging, make it more formal and short, with just relevant info only
- controller: tracing
- jobs: reschedule jobs if config was changed
- jobs: stop scheduling after shutdown signal
