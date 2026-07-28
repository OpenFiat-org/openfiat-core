//! A small metrics registry rendering the Prometheus text exposition
//! format (no external dependency: the format is a handful of lines per
//! metric, not worth pulling in a client library for). Metrics have no
//! label support today — every metric this workspace needs so far is a
//! single node-wide value; add labels here if a real need for
//! per-dimension breakdowns shows up.

use crate::counter::Counter;
use crate::gauge::Gauge;
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

enum Metric {
    Counter(Arc<Counter>),
    Gauge(Arc<Gauge>),
}

#[derive(Default)]
pub struct MetricsRegistry {
    metrics: RwLock<BTreeMap<String, (String, Metric)>>,
}

impl MetricsRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a counter under `name`, or returns the existing handle
    /// if it's already registered — idempotent so call sites can just
    /// call this wherever they need the handle, rather than threading a
    /// single registration point through the whole node.
    pub fn counter(&self, name: &str, help: &str) -> Arc<Counter> {
        validate_name(name);
        let mut metrics = self.metrics.write().expect("metrics registry lock poisoned");
        match metrics.get(name) {
            Some((_, Metric::Counter(counter))) => Arc::clone(counter),
            Some((_, Metric::Gauge(_))) => panic!("metric {name} is already registered as a gauge"),
            None => {
                let counter = Arc::new(Counter::default());
                metrics.insert(name.to_string(), (help.to_string(), Metric::Counter(Arc::clone(&counter))));
                counter
            }
        }
    }

    pub fn gauge(&self, name: &str, help: &str) -> Arc<Gauge> {
        validate_name(name);
        let mut metrics = self.metrics.write().expect("metrics registry lock poisoned");
        match metrics.get(name) {
            Some((_, Metric::Gauge(gauge))) => Arc::clone(gauge),
            Some((_, Metric::Counter(_))) => panic!("metric {name} is already registered as a counter"),
            None => {
                let gauge = Arc::new(Gauge::default());
                metrics.insert(name.to_string(), (help.to_string(), Metric::Gauge(Arc::clone(&gauge))));
                gauge
            }
        }
    }

    /// Renders every registered metric in Prometheus text exposition
    /// format 0.0.4, sorted by name for deterministic output.
    pub fn render(&self) -> String {
        let metrics = self.metrics.read().expect("metrics registry lock poisoned");
        let mut output = String::new();
        for (name, (help, metric)) in metrics.iter() {
            let type_name = match metric {
                Metric::Counter(_) => "counter",
                Metric::Gauge(_) => "gauge",
            };
            output.push_str(&format!("# HELP {name} {help}\n"));
            output.push_str(&format!("# TYPE {name} {type_name}\n"));
            match metric {
                Metric::Counter(counter) => output.push_str(&format!("{name} {}\n", counter.get())),
                Metric::Gauge(gauge) => output.push_str(&format!("{name} {}\n", gauge.get())),
            }
        }
        output
    }
}

/// Prometheus metric names: `[a-zA-Z_:][a-zA-Z0-9_:]*`. A programming
/// error (a bad literal chosen by this workspace's own code), not
/// untrusted input, so this panics rather than returning a `Result`.
fn validate_name(name: &str) {
    let valid = name.chars().enumerate().all(|(i, c)| {
        if i == 0 { c.is_ascii_alphabetic() || c == '_' || c == ':' } else { c.is_ascii_alphanumeric() || c == '_' || c == ':' }
    });
    assert!(!name.is_empty() && valid, "invalid Prometheus metric name: {name:?}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registering_the_same_counter_name_twice_returns_the_same_handle() {
        let registry = MetricsRegistry::new();
        let a = registry.counter("requests_total", "Total requests");
        let b = registry.counter("requests_total", "Total requests");
        a.inc();
        assert_eq!(b.get(), 1);
    }

    #[test]
    #[should_panic(expected = "already registered as a gauge")]
    fn registering_a_counter_under_a_gauge_name_panics() {
        let registry = MetricsRegistry::new();
        registry.gauge("peers", "Connected peers");
        registry.counter("peers", "Connected peers");
    }

    #[test]
    #[should_panic(expected = "invalid Prometheus metric name")]
    fn an_invalid_metric_name_panics() {
        let registry = MetricsRegistry::new();
        registry.counter("1_starts_with_a_digit", "bad");
    }

    #[test]
    fn render_produces_help_type_and_value_lines() {
        let registry = MetricsRegistry::new();
        let counter = registry.counter("gossip_events_received_total", "Total gossip events received");
        counter.add(3);
        let gauge = registry.gauge("connected_peers", "Currently connected peers");
        gauge.set(2);

        let rendered = registry.render();
        assert!(rendered.contains("# HELP connected_peers Currently connected peers\n"));
        assert!(rendered.contains("# TYPE connected_peers gauge\n"));
        assert!(rendered.contains("connected_peers 2\n"));
        assert!(rendered.contains("# TYPE gossip_events_received_total counter\n"));
        assert!(rendered.contains("gossip_events_received_total 3\n"));
    }
}
