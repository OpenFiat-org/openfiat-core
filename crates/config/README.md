# openfiat-config

Configuration loading, validation, and layering (file/env/flags) for the
`openfiat-node` binary. Architecture scaffolding today — the composition
root (`cli`) will drive this once its own config shape (listen addresses,
data directory, wallet path, bootstrap peers) is settled.

## Depends on

- `openfiat-types` — shares the workspace's basic value types where
  configuration fields need them.

## Used by

- `openfiat-cli` — the only consumer; loads a node's configuration before
  wiring up the rest of the workspace's crates.
