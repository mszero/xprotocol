use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct Histogram {
    buckets: Vec<AtomicU64>,
    bounds: Vec<u64>,
}

impl Histogram {
    pub fn new(bounds: &[u64]) -> Self {
        Self {
            buckets: (0..bounds.len() + 1).map(|_| AtomicU64::new(0)).collect(),
            bounds: bounds.to_vec(),
        }
    }

    pub fn observe(&self, value: u64) {
        let index = match self.bounds.binary_search(&value) {
            Ok(i) => i,
            Err(i) => i,
        };
        self.buckets[index].fetch_add(1, Ordering::Relaxed);
    }

    pub fn percentile(&self, p: f64, total: u64) -> u64 {
        if total == 0 {
            return 0;
        }
        let target = (total as f64 * p / 100.0).ceil() as u64;
        let mut cumulative = 0u64;

        for (i, bucket) in self.buckets.iter().enumerate() {
            cumulative += bucket.load(Ordering::Relaxed);
            if cumulative >= target {
                return if i < self.bounds.len() {
                    self.bounds[i]
                } else {
                    *self.bounds.last().unwrap_or(&100)
                };
            }
        }
        *self.bounds.last().unwrap_or(&100)
    }
}

#[derive(Debug, Clone)]
pub struct RequestMetrics {
    pub total: u64,
    pub success: u64,
    pub failure: u64,
    pub timeout: u64,
    pub latency_avg_ms: u64,
    pub latency_p50_ms: u64,
    pub latency_p95_ms: u64,
    pub latency_p99_ms: u64,
    pub latency_max_ms: u64,
}

pub struct ServiceMetrics {
    requests_total: AtomicU64,
    requests_success: AtomicU64,
    requests_failure: AtomicU64,
    requests_timeout: AtomicU64,
    latency_sum_ms: AtomicU64,
    latency_count: AtomicU64,
    latency_max_ms: AtomicU64,
    active_requests: AtomicU64,
    latency_histogram: Arc<Histogram>,
}

impl ServiceMetrics {
    pub fn new() -> Self {
        Self {
            requests_total: AtomicU64::new(0),
            requests_success: AtomicU64::new(0),
            requests_failure: AtomicU64::new(0),
            requests_timeout: AtomicU64::new(0),
            latency_sum_ms: AtomicU64::new(0),
            latency_count: AtomicU64::new(0),
            latency_max_ms: AtomicU64::new(0),
            active_requests: AtomicU64::new(0),
            latency_histogram: Arc::new(Histogram::new(&[
                10, 50, 100, 250, 500, 1000, 2500, 5000, 10000, 30000, 60000,
            ])),
        }
    }

    pub fn request_start(&self) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
        self.active_requests.fetch_add(1, Ordering::Relaxed);
    }

    pub fn request_end(&self, latency: Duration, success: bool, is_timeout: bool) {
        self.active_requests.fetch_sub(1, Ordering::Relaxed);
        let latency_ms = latency.as_millis() as u64;

        self.latency_count.fetch_add(1, Ordering::Relaxed);
        self.latency_sum_ms.fetch_add(latency_ms, Ordering::Relaxed);

        loop {
            let current = self.latency_max_ms.load(Ordering::Relaxed);
            if latency_ms <= current {
                break;
            }
            if self
                .latency_max_ms
                .compare_exchange(current, latency_ms, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }
        }

        self.latency_histogram.observe(latency_ms);

        if is_timeout {
            self.requests_timeout.fetch_add(1, Ordering::Relaxed);
        } else if success {
            self.requests_success.fetch_add(1, Ordering::Relaxed);
        } else {
            self.requests_failure.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn snapshot(&self) -> RequestMetrics {
        let total = self.requests_total.load(Ordering::Relaxed);
        let count = self.latency_count.load(Ordering::Relaxed);
        let sum = self.latency_sum_ms.load(Ordering::Relaxed);

        RequestMetrics {
            total,
            success: self.requests_success.load(Ordering::Relaxed),
            failure: self.requests_failure.load(Ordering::Relaxed),
            timeout: self.requests_timeout.load(Ordering::Relaxed),
            latency_avg_ms: if count > 0 { sum / count } else { 0 },
            latency_p50_ms: self.latency_histogram.percentile(50.0, total),
            latency_p95_ms: self.latency_histogram.percentile(95.0, total),
            latency_p99_ms: self.latency_histogram.percentile(99.0, total),
            latency_max_ms: self.latency_max_ms.load(Ordering::Relaxed),
        }
    }

    pub fn active_count(&self) -> u64 {
        self.active_requests.load(Ordering::Relaxed)
    }
}

impl Default for ServiceMetrics {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct ConnectionMetrics {
    pub total_connections: u64,
    pub active_connections: u64,
    pub peak_connections: u64,
    pub closed_connections: u64,
    pub connection_errors: u64,
}

pub struct ConnectionTracker {
    total_connections: AtomicU64,
    active_connections: AtomicU64,
    peak_connections: AtomicU64,
    closed_connections: AtomicU64,
    connection_errors: AtomicU64,
}

impl ConnectionTracker {
    pub fn new() -> Self {
        Self {
            total_connections: AtomicU64::new(0),
            active_connections: AtomicU64::new(0),
            peak_connections: AtomicU64::new(0),
            closed_connections: AtomicU64::new(0),
            connection_errors: AtomicU64::new(0),
        }
    }

    pub fn connection_opened(&self) {
        self.total_connections.fetch_add(1, Ordering::Relaxed);
        let active = self.active_connections.fetch_add(1, Ordering::Relaxed) + 1;

        loop {
            let peak = self.peak_connections.load(Ordering::Relaxed);
            if active <= peak {
                break;
            }
            if self
                .peak_connections
                .compare_exchange(peak, active, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }
        }
    }

    pub fn connection_closed(&self) {
        self.active_connections.fetch_sub(1, Ordering::Relaxed);
        self.closed_connections.fetch_add(1, Ordering::Relaxed);
    }

    pub fn connection_error(&self) {
        self.connection_errors.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> ConnectionMetrics {
        ConnectionMetrics {
            total_connections: self.total_connections.load(Ordering::Relaxed),
            active_connections: self.active_connections.load(Ordering::Relaxed),
            peak_connections: self.peak_connections.load(Ordering::Relaxed),
            closed_connections: self.closed_connections.load(Ordering::Relaxed),
            connection_errors: self.connection_errors.load(Ordering::Relaxed),
        }
    }
}

impl Default for ConnectionTracker {
    fn default() -> Self {
        Self::new()
    }
}

pub struct MetricsCollector {
    services: parking_lot::RwLock<HashMap<String, Arc<ServiceMetrics>>>,
    connections: Arc<ConnectionTracker>,
}

impl MetricsCollector {
    pub fn new() -> Self {
        Self {
            services: parking_lot::RwLock::new(HashMap::new()),
            connections: Arc::new(ConnectionTracker::new()),
        }
    }

    pub fn get_or_create_service(&self, service_name: &str) -> Arc<ServiceMetrics> {
        let services = self.services.read();
        if let Some(metrics) = services.get(service_name) {
            return metrics.clone();
        }
        drop(services);

        let mut services = self.services.write();
        if let Some(metrics) = services.get(service_name) {
            return metrics.clone();
        }

        let metrics = Arc::new(ServiceMetrics::new());
        services.insert(service_name.to_string(), metrics.clone());
        metrics
    }

    pub fn connection_tracker(&self) -> Arc<ConnectionTracker> {
        self.connections.clone()
    }

    pub fn snapshot_all(&self) -> HashMap<String, RequestMetrics> {
        let services = self.services.read();
        services
            .iter()
            .map(|(name, metrics)| (name.clone(), metrics.snapshot()))
            .collect()
    }
}

impl Default for MetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}

pub struct AlertManager {
    rules: parking_lot::RwLock<Vec<AlertRule>>,
    callbacks: parking_lot::RwLock<Vec<Box<dyn AlertCallback>>>,
}

pub struct AlertRule {
    pub name: String,
    pub metric: String,
    pub condition: AlertCondition,
    pub cooldown: Duration,
}

pub enum AlertCondition {
    LatencyP99Above(u64),
    ErrorRateAbove(f64),
    ActiveConnectionsAbove(u64),
    CircuitBreakerOpen,
}

pub trait AlertCallback: Send + Sync {
    fn on_alert(&self, alert: &Alert);
}

impl<F> AlertCallback for F
where
    F: Fn(&Alert) + Send + Sync + 'static,
{
    fn on_alert(&self, alert: &Alert) {
        self(alert)
    }
}

#[derive(Debug, Clone)]
pub struct Alert {
    pub rule_name: String,
    pub message: String,
    pub severity: AlertSeverity,
    pub timestamp: Instant,
}

#[derive(Debug, Clone, Copy)]
pub enum AlertSeverity {
    Warning,
    Critical,
}

impl AlertManager {
    pub fn new() -> Self {
        Self {
            rules: parking_lot::RwLock::new(Vec::new()),
            callbacks: parking_lot::RwLock::new(Vec::new()),
        }
    }

    pub fn add_rule(&self, rule: AlertRule) {
        self.rules.write().push(rule);
    }

    pub fn add_callback<C: AlertCallback + 'static>(&self, callback: C) {
        self.callbacks.write().push(Box::new(callback));
    }

    pub fn check(&self, metrics: &MetricsCollector) {
        let rules = self.rules.read();
        let snapshots = metrics.snapshot_all();

        for (service_name, snapshot) in &snapshots {
            for rule in rules.iter() {
                if &rule.metric != service_name {
                    continue;
                }

                let should_alert = match &rule.condition {
                    AlertCondition::LatencyP99Above(threshold) => {
                        snapshot.latency_p99_ms > *threshold
                    }
                    AlertCondition::ErrorRateAbove(threshold) => {
                        if snapshot.total > 0 {
                            (snapshot.failure as f64 / snapshot.total as f64) > *threshold
                        } else {
                            false
                        }
                    }
                    AlertCondition::CircuitBreakerOpen => false,
                    AlertCondition::ActiveConnectionsAbove(_) => false,
                };

                if should_alert {
                    let alert = Alert {
                        rule_name: rule.name.clone(),
                        message: format!("{}: threshold exceeded", service_name),
                        severity: AlertSeverity::Warning,
                        timestamp: Instant::now(),
                    };

                    let callbacks = self.callbacks.read();
                    for callback in callbacks.iter() {
                        callback.on_alert(&alert);
                    }
                }
            }
        }
    }
}

impl Default for AlertManager {
    fn default() -> Self {
        Self::new()
    }
}

use std::fmt;

impl fmt::Display for AlertSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AlertSeverity::Warning => write!(f, "WARNING"),
            AlertSeverity::Critical => write!(f, "CRITICAL"),
        }
    }
}

impl fmt::Display for Alert {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}] {} - {}",
            self.severity, self.rule_name, self.message
        )
    }
}
