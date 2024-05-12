# Roadmap

## In work

- doc: just create it

## Next release

- gitrepo: extend file sensor to use globs instead of single file
- controller: tracing
- controller: metrics

## Wishes

- tests: automate everything possible
- docker: publish images to docker hub too
- refactor: improve logging, make it more formal and short, with just relevant info only
- chart/cli: Make new subcommand to dump out default config and update it in the chart as part of CD
- refactor: looks like scheduler shouldn't be under RwLock because it's `add` method uses internal mutability
- gitrepo: update to use `gix` instead if `git2`, if possible
- hooks: implement hook requests rate control/throttling
- hooks/gitrepo: rework secrets cache to watch requested secrets and update it on changes
- cli: deploy, config, get state, remove
- cli: configure webhook auth secrets and store them as hashes
- hooks: tls listener for webhooks
- gitrepo: add support for private keys with passphrase
- gitrepo: use Mozilla CA bundle instead of system and build controller/cloner images `FROM scratch`
