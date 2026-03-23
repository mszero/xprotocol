use futures::Future;
use pingora_memory_cache::MemoryCache;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub type Cache = MemoryCache<String, bytes::Bytes>;

#[derive(Debug, Default)]
pub struct Metrics {
    requests_total: AtomicUsize,
    requests_success: AtomicUsize,
    requests_failure: AtomicUsize,
    connections_active: AtomicUsize,
    bytes_sent: AtomicUsize,
    bytes_received: AtomicUsize,
    latency_sum: std::sync::atomic::AtomicU64,
    latency_count: AtomicUsize,
}

impl Metrics {
    pub fn record_request(&self) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_success(&self) {
        self.requests_success.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_failure(&self) {
        self.requests_failure.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_latency(&self, duration: Duration) {
        self.latency_sum
            .fetch_add(duration.as_millis() as u64, Ordering::Relaxed);
        self.latency_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_connections(&self) {
        self.connections_active.fetch_add(1, Ordering::Relaxed);
    }

    pub fn dec_connections(&self) {
        self.connections_active.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn add_bytes_sent(&self, bytes: usize) {
        self.bytes_sent.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn add_bytes_received(&self, bytes: usize) {
        self.bytes_received.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        let total = self.requests_total.load(Ordering::Relaxed);
        let success = self.requests_success.load(Ordering::Relaxed);
        let latency_count = self.latency_count.load(Ordering::Relaxed);
        let latency_sum = self.latency_sum.load(Ordering::Relaxed);

        MetricsSnapshot {
            requests_total: total,
            requests_success: success,
            requests_failure: self.requests_failure.load(Ordering::Relaxed),
            connections_active: self.connections_active.load(Ordering::Relaxed),
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
            bytes_received: self.bytes_received.load(Ordering::Relaxed),
            avg_latency_ms: if latency_count > 0 {
                latency_sum / latency_count as u64
            } else {
                0
            },
            success_rate: if total > 0 {
                (success as f64 / total as f64) * 100.0
            } else {
                0.0
            },
        }
    }
}

#[derive(Debug)]
pub struct MetricsSnapshot {
    pub requests_total: usize,
    pub requests_success: usize,
    pub requests_failure: usize,
    pub connections_active: usize,
    pub bytes_sent: usize,
    pub bytes_received: usize,
    pub avg_latency_ms: u64,
    pub success_rate: f64,
}

#[derive(Debug, Clone)]
pub struct RetryConfig {
    pub max_retries: usize,
    pub initial_delay: Duration,
    pub max_delay: Duration,
    pub backoff_multiplier: f64,
    pub jitter: bool,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(30),
            backoff_multiplier: 2.0,
            jitter: true,
        }
    }
}

impl RetryConfig {
    pub fn calculate_delay(&self, attempt: usize) -> Duration {
        let delay = self.initial_delay.as_secs_f64() * self.backoff_multiplier.powi(attempt as i32);
        let delay = delay.min(self.max_delay.as_secs_f64());

        if self.jitter {
            let jitter = rand::random::<f64>() * 0.5 + 0.5;
            Duration::from_secs_f64(delay * jitter)
        } else {
            Duration::from_secs_f64(delay)
        }
    }
}

pub struct RateLimiter {
    max_requests: usize,
    window: Duration,
    requests: parking_lot::Mutex<Vec<Instant>>,
}

impl RateLimiter {
    pub fn new(max_requests: usize, window: Duration) -> Self {
        Self {
            max_requests,
            window,
            requests: parking_lot::Mutex::new(Vec::new()),
        }
    }

    pub fn try_acquire(&self) -> bool {
        let now = Instant::now();
        let mut requests = self.requests.lock();

        requests.retain(|t| now.duration_since(*t) < self.window);

        if requests.len() < self.max_requests {
            requests.push(now);
            return true;
        }

        false
    }

    pub async fn acquire(&self) -> crate::error::Result<()> {
        while !self.try_acquire() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok(())
    }

    pub fn available(&self) -> usize {
        let now = Instant::now();
        let mut requests = self.requests.lock();
        requests.retain(|t| now.duration_since(*t) < self.window);
        self.max_requests.saturating_sub(requests.len())
    }
}

#[derive(Clone)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    pub fn cancelled(&self) -> impl Future<Output = ()> + Send + '_ {
        async move {
            while !self.cancelled.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn fork(&self) -> Self {
        Self {
            cancelled: self.cancelled.clone(),
        }
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

pub struct HealthChecker {
    interval: Duration,
    timeout: Duration,
}

impl HealthChecker {
    pub fn new() -> Self {
        Self {
            interval: Duration::from_secs(30),
            timeout: Duration::from_secs(5),
        }
    }

    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub async fn check(&self, addr: &str) -> bool {
        tokio::time::timeout(self.timeout, tokio::net::TcpStream::connect(addr))
            .await
            .is_ok()
    }

    pub fn interval(&self) -> Duration {
        self.interval
    }

    pub fn timeout_duration(&self) -> Duration {
        self.timeout
    }
}

impl Default for HealthChecker {
    fn default() -> Self {
        Self::new()
    }
}

use tokio::sync::broadcast;

pub struct MessageQueue {
    topics: parking_lot::RwLock<HashMap<String, broadcast::Sender<bytes::Bytes>>>,
}

impl MessageQueue {
    pub fn new() -> Self {
        Self {
            topics: parking_lot::RwLock::new(HashMap::new()),
        }
    }

    pub fn publish(&self, topic: &str, data: bytes::Bytes) -> usize {
        let topics = self.topics.read();
        if let Some(sender) = topics.get(topic) {
            sender.send(data).unwrap_or(0)
        } else {
            0
        }
    }

    pub fn subscribe(&self, topic: &str) -> broadcast::Receiver<bytes::Bytes> {
        let (sender, receiver) = broadcast::channel(1024);
        self.topics.write().insert(topic.to_string(), sender);
        receiver
    }

    pub fn topics(&self) -> Vec<String> {
        let topics = self.topics.read();
        topics.keys().cloned().collect()
    }
}

impl Default for MessageQueue {
    fn default() -> Self {
        Self::new()
    }
}

use crate::protocol::ServiceRequest;

#[derive(Debug, Clone)]
pub struct DeadLetter {
    pub request: ServiceRequest,
    pub error: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub retry_count: usize,
}

pub struct DeadLetterQueue {
    queue: parking_lot::RwLock<VecDeque<DeadLetter>>,
    max_size: usize,
}

impl DeadLetterQueue {
    pub fn new(max_size: usize) -> Self {
        Self {
            queue: parking_lot::RwLock::new(VecDeque::new()),
            max_size,
        }
    }

    pub fn push(&self, request: ServiceRequest, error: String) {
        let mut queue = self.queue.write();
        if queue.len() >= self.max_size {
            queue.pop_front();
        }
        queue.push_back(DeadLetter {
            request,
            error,
            timestamp: chrono::Utc::now(),
            retry_count: 0,
        });
    }

    pub fn pop(&self) -> Option<DeadLetter> {
        self.queue.write().pop_front()
    }

    pub fn get_all(&self) -> Vec<DeadLetter> {
        self.queue.read().iter().cloned().collect()
    }

    pub fn clear(&self) {
        self.queue.write().clear();
    }

    pub fn len(&self) -> usize {
        self.queue.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.read().is_empty()
    }
}
