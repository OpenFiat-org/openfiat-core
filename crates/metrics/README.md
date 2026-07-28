# openfiat-metrics

`Counter`/`Gauge` (plain atomics, `Arc`-shareable across threads — unlike
this workspace's `Rc`-based domain registries) and a `MetricsRegistry`
that renders them in Prometheus's text exposition format. No external
metrics-client dependency: the format is a handful of lines per metric.

## Depends on

Nothing in this workspace — a pure, standalone utility.

## Used by

- `openfiat-rpc` — request/error counters exposed at `GET /metrics`.
- `openfiat-cli` — the composition root's own node-level metrics.
