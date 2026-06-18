use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryMetricEntry {
    pub kind: &'static str,
    pub service: String,
    pub endpoint: String,
    pub source: String,
    pub reason: String,
    pub count: u64,
}

pub const DISCOVERY_METRIC_KINDS: [&str; 5] = [
    "discovery_success",
    "discovery_failure",
    "fallback_used",
    "no_healthy_instance",
    "endpoint_missing",
];

static DISCOVERY_METRICS: LazyLock<Mutex<HashMap<DiscoveryMetricKey, u64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct DiscoveryMetricKey {
    kind: &'static str,
    service: String,
    endpoint: String,
    source: String,
    reason: String,
}

pub fn record_discovery_metric(
    service: impl Into<String>,
    endpoint: impl Into<String>,
    source: impl Into<String>,
    reason: impl Into<String>,
) {
    let service = service.into();
    let endpoint = endpoint.into();
    let source = source.into();
    let reason = reason.into();
    let Some(kind) = discovery_metric_kind(&source, &reason) else {
        return;
    };

    let key = DiscoveryMetricKey {
        kind,
        service,
        endpoint,
        source,
        reason,
    };
    let mut metrics = DISCOVERY_METRICS.lock().unwrap();
    *metrics.entry(key).or_insert(0) += 1;
}

pub fn get_discovery_metrics_snapshot() -> Vec<DiscoveryMetricEntry> {
    let metrics = DISCOVERY_METRICS.lock().unwrap();
    let mut snapshot = metrics
        .iter()
        .map(|(key, count)| DiscoveryMetricEntry {
            kind: key.kind,
            service: key.service.clone(),
            endpoint: key.endpoint.clone(),
            source: key.source.clone(),
            reason: key.reason.clone(),
            count: *count,
        })
        .collect::<Vec<_>>();
    snapshot.sort_by(|a, b| {
        a.kind
            .cmp(b.kind)
            .then_with(|| a.service.cmp(&b.service))
            .then_with(|| a.endpoint.cmp(&b.endpoint))
            .then_with(|| a.source.cmp(&b.source))
            .then_with(|| a.reason.cmp(&b.reason))
    });
    snapshot
}

pub fn reset_discovery_metrics() {
    DISCOVERY_METRICS.lock().unwrap().clear();
}

pub fn collect_discovery_metric_fields(reset: bool) -> Vec<(String, String)> {
    let snapshot = get_discovery_metrics_snapshot();
    let mut totals = HashMap::new();
    for kind in DISCOVERY_METRIC_KINDS {
        totals.insert(kind, 0_u64);
    }
    for entry in snapshot {
        *totals.entry(entry.kind).or_insert(0) += entry.count;
    }

    if reset {
        reset_discovery_metrics();
    }

    DISCOVERY_METRIC_KINDS
        .into_iter()
        .map(|kind| {
            (
                format!("{kind}_total"),
                totals.get(kind).copied().unwrap_or(0).to_string(),
            )
        })
        .collect()
}

fn discovery_metric_kind(source: &str, reason: &str) -> Option<&'static str> {
    if source == "fallback" && reason == "fallback_used" {
        return Some("fallback_used");
    }
    match reason {
        "discovered" if source == "registry" => Some("discovery_success"),
        "registry_error" | "registry_disabled" | "fallback_forbidden" => {
            Some("discovery_failure")
        }
        "no_healthy_instance" => Some("no_healthy_instance"),
        "endpoint_missing" => Some("endpoint_missing"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_collects_discovery_metrics() {
        reset_discovery_metrics();

        record_discovery_metric("game-server", "admin", "registry", "discovered");
        record_discovery_metric("game-server", "admin", "registry", "endpoint_missing");
        record_discovery_metric("game-server", "admin", "fallback", "fallback_used");

        let snapshot = get_discovery_metrics_snapshot();
        assert_eq!(snapshot.len(), 3);
        assert!(snapshot.iter().any(|entry| {
            entry.kind == "discovery_success"
                && entry.service == "game-server"
                && entry.endpoint == "admin"
                && entry.count == 1
        }));

        let fields = collect_discovery_metric_fields(true);
        assert!(fields.contains(&("discovery_success_total".to_string(), "1".to_string())));
        assert!(fields.contains(&("endpoint_missing_total".to_string(), "1".to_string())));
        assert!(fields.contains(&("fallback_used_total".to_string(), "1".to_string())));
        assert!(get_discovery_metrics_snapshot().is_empty());
    }
}
