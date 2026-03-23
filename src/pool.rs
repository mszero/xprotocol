use crate::circuit_breaker::{CircuitBreaker, CircuitState};
use crate::client::SharedUdsClient;
use crate::error::{ProtocolError, Result};
use crate::protocol::ServiceResponse;
use crate::wire::{read_message, write_message, MSG_REQUEST, MSG_RESPONSE};
use pingora_pool::ConnectionMeta;
use pingora_pool::ConnectionPool;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug)]
pub struct PooledConnectionMeta {
    pub id: u64,
    pub path: String,
    pub created_at: std::time::Instant,
    pub last_used_at: std::time::Instant,
    pub use_count: AtomicUsize,
}

impl PooledConnectionMeta {
    pub fn new(id: u64, path: String) -> Self {
        let now = std::time::Instant::now();
        Self {
            id,
            path,
            created_at: now,
            last_used_at: now,
            use_count: AtomicUsize::new(0),
        }
    }

    pub fn record_use(&mut self) {
        self.use_count.fetch_add(1, Ordering::Relaxed);
        self.last_used_at = std::time::Instant::now();
    }

    pub fn is_idle_timeout(&self, max_idle_secs: u64) -> bool {
        self.last_used_at.elapsed().as_secs() > max_idle_secs
    }
}

#[derive(Debug, Default)]
pub struct ConnectionPoolMetrics {
    pub total_requests: AtomicUsize,
    pub connections_created: AtomicUsize,
    pub connections_reused: AtomicUsize,
    pub pool_hits: AtomicUsize,
    pub pool_misses: AtomicUsize,
    pub connections_evicted: AtomicUsize,
}

impl ConnectionPoolMetrics {
    pub fn total_requests(&self) -> usize {
        self.total_requests.load(Ordering::Relaxed)
    }
    pub fn connections_created(&self) -> usize {
        self.connections_created.load(Ordering::Relaxed)
    }
    pub fn connections_reused(&self) -> usize {
        self.connections_reused.load(Ordering::Relaxed)
    }
    pub fn pool_hits(&self) -> usize {
        self.pool_hits.load(Ordering::Relaxed)
    }
    pub fn pool_misses(&self) -> usize {
        self.pool_misses.load(Ordering::Relaxed)
    }
    pub fn connections_evicted(&self) -> usize {
        self.connections_evicted.load(Ordering::Relaxed)
    }

    pub fn reuse_rate(&self) -> f64 {
        let created = self.connections_created() as f64;
        let reused = self.connections_reused() as f64;
        let total = created + reused;
        if total == 0.0 {
            0.0
        } else {
            (reused / total) * 100.0
        }
    }
}

pub struct UdsConnectionPool {
    pool: ConnectionPool<tokio::net::UnixStream>,
    path: String,
    max_idle_secs: u64,
    connection_id: AtomicU64,
    metrics: Arc<ConnectionPoolMetrics>,
}

impl UdsConnectionPool {
    pub fn new(path: &str, max_connections: usize) -> Self {
        Self {
            pool: ConnectionPool::new(max_connections),
            path: path.to_string(),
            max_idle_secs: 300,
            connection_id: AtomicU64::new(0),
            metrics: Arc::new(ConnectionPoolMetrics::default()),
        }
    }

    pub fn with_max_idle_secs(mut self, secs: u64) -> Self {
        self.max_idle_secs = secs;
        self
    }

    pub fn get(&self) -> Option<tokio::net::UnixStream> {
        let key: u64 = 1;
        self.pool.get(&key)
    }

    pub fn put(&self, stream: tokio::net::UnixStream) {
        let key: u64 = 1;
        let id = self.connection_id.fetch_add(1, Ordering::Relaxed) as i32;
        let meta = ConnectionMeta::new(key, id);
        let (_, receiver) = self.pool.put(&meta, stream);

        tokio::spawn(async move {
            match receiver.await {
                Ok(true) => {}
                Ok(false) => {}
                Err(_) => {}
            }
        });
    }

    pub async fn call(
        &self,
        service: &str,
        method: &str,
        payload: serde_json::Value,
    ) -> Result<ServiceResponse> {
        self.metrics.total_requests.fetch_add(1, Ordering::Relaxed);

        let mut stream = match self.get() {
            Some(s) => {
                self.metrics.pool_hits.fetch_add(1, Ordering::Relaxed);
                self.metrics
                    .connections_reused
                    .fetch_add(1, Ordering::Relaxed);
                s
            }
            None => {
                self.metrics.pool_misses.fetch_add(1, Ordering::Relaxed);
                self.metrics
                    .connections_created
                    .fetch_add(1, Ordering::Relaxed);
                match tokio::net::UnixStream::connect(&self.path).await {
                    Ok(s) => s,
                    Err(e) => {
                        self.metrics
                            .connections_evicted
                            .fetch_add(1, Ordering::Relaxed);
                        return Err(ProtocolError::ConnectionFailed(e.to_string()));
                    }
                }
            }
        };

        let result = self.do_call(&mut stream, service, method, payload).await;

        match &result {
            Ok(_) => {
                self.put(stream);
            }
            Err(_) => {
                self.put(stream);
            }
        }

        result
    }

    async fn do_call(
        &self,
        stream: &mut tokio::net::UnixStream,
        service: &str,
        method: &str,
        payload: serde_json::Value,
    ) -> Result<ServiceResponse> {
        let (reader, writer) = stream.split();
        let mut reader = tokio::io::BufReader::new(reader);
        let mut writer = tokio::io::BufWriter::new(writer);

        let request = crate::protocol::ServiceRequest::new(service, method, payload);
        let data = serde_json::to_vec(&request)?;
        write_message(&mut writer, MSG_REQUEST, &data).await?;

        let (msg_type, payload) = read_message(&mut reader).await?;

        if msg_type != MSG_RESPONSE {
            return Err(ProtocolError::Protocol(format!(
                "invalid response type: expected {}, got {}",
                MSG_RESPONSE, msg_type
            )));
        }

        let response: ServiceResponse = serde_json::from_slice(&payload)?;
        Ok(response)
    }

    pub fn metrics(&self) -> &ConnectionPoolMetrics {
        &self.metrics
    }

    pub fn cleanup(&self) {
        let key: u64 = 1;
        let id = 0;
        let meta = ConnectionMeta::new(key, id);
        self.pool.pop_closed(&meta);
    }

    pub fn close(&self) {}
}

pub struct UdsClientPool {
    path: String,
    semaphore: Arc<tokio::sync::Semaphore>,
    pool: Arc<tokio::sync::Mutex<Vec<SharedUdsClient>>>,
    max_pool_size: usize,
    created_clients: Arc<AtomicUsize>,
}

impl UdsClientPool {
    pub fn new(path: &str, max_concurrent: usize) -> Self {
        Self::with_max_pool_size(path, max_concurrent, max_concurrent)
    }

    pub fn with_max_pool_size(path: &str, max_concurrent: usize, max_pool_size: usize) -> Self {
        Self {
            path: path.to_string(),
            semaphore: Arc::new(tokio::sync::Semaphore::new(max_concurrent)),
            pool: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            max_pool_size,
            created_clients: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub async fn call(
        &self,
        service: &str,
        method: &str,
        payload: serde_json::Value,
    ) -> Result<ServiceResponse> {
        let permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|_| ProtocolError::ResourceExhausted("pool exhausted".into()))?;

        let client = self.get_or_create_client().await;

        let result = client.call(service, method, payload).await;

        self.return_client(client, &result).await;

        drop(permit);
        result
    }

    async fn get_or_create_client(&self) -> SharedUdsClient {
        {
            let mut pool = self.pool.lock().await;
            if let Some(client) = pool.pop() {
                return client;
            }
        }

        let created = self.created_clients.fetch_add(1, Ordering::Relaxed);
        if created < self.max_pool_size {
            return SharedUdsClient::new(&self.path);
        }

        self.created_clients.fetch_sub(1, Ordering::Relaxed);
        let mut retries = 3;
        while retries > 0 {
            {
                let mut pool = self.pool.lock().await;
                if let Some(client) = pool.pop() {
                    return client;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
            retries -= 1;
        }

        SharedUdsClient::new(&self.path)
    }

    async fn return_client(&self, client: SharedUdsClient, result: &Result<ServiceResponse>) {
        let should_pool = result.is_ok() || {
            if let Err(e) = result {
                matches!(
                    e,
                    ProtocolError::ConnectionClosed | ProtocolError::ConnectionFailed(_)
                )
            } else {
                false
            }
        };

        if should_pool {
            let mut pool = self.pool.lock().await;
            if pool.len() < self.max_pool_size {
                pool.push(client);
            }
        }
    }

    pub async fn call_batch(
        &self,
        requests: Vec<(String, String, serde_json::Value)>,
    ) -> Vec<Result<ServiceResponse>> {
        let semaphore = self.semaphore.clone();
        let pool = self.pool.clone();
        let path = self.path.clone();
        let max_pool_size = self.max_pool_size;
        let created_clients = self.created_clients.clone();

        let futures: Vec<_> = requests
            .into_iter()
            .map(move |(service, method, payload)| {
                let sem = semaphore.clone();
                let p = pool.clone();
                let created = created_clients.clone();
                let svc = service.clone();
                let mtd = method.clone();
                let path_clone = path.clone();
                let mps = max_pool_size;

                async move {
                    let permit = sem
                        .acquire()
                        .await
                        .map_err(|_| ProtocolError::ResourceExhausted("pool exhausted".into()))?;

                    let client = {
                        if let Some(c) = {
                            let mut guard = p.lock().await;
                            guard.pop()
                        } {
                            c
                        } else {
                            let c = created.fetch_add(1, Ordering::Relaxed);
                            if c < mps {
                                SharedUdsClient::new(&path_clone)
                            } else {
                                created.fetch_sub(1, Ordering::Relaxed);
                                SharedUdsClient::new(&path_clone)
                            }
                        }
                    };

                    let result = client.call(&svc, &mtd, payload).await;

                    let should_pool = result.is_ok() || {
                        if let Err(e) = &result {
                            matches!(
                                e,
                                ProtocolError::ConnectionClosed
                                    | ProtocolError::ConnectionFailed(_)
                            )
                        } else {
                            false
                        }
                    };

                    if should_pool {
                        let mut guard = p.lock().await;
                        if guard.len() < mps {
                            guard.push(client);
                        }
                    }

                    drop(permit);
                    result
                }
            })
            .collect();

        futures::future::join_all(futures).await
    }

    pub async fn close(&self) {
        let mut pool = self.pool.lock().await;
        for client in pool.iter() {
            client.close().await;
        }
        pool.clear();
        self.created_clients.store(0, Ordering::Relaxed);
    }

    pub fn pooled_count(&self) -> usize {
        self.created_clients.load(Ordering::Relaxed)
    }
}

pub struct TopicBasedPool {
    path: String,
    default_pool: Arc<UdsClientPool>,
    topic_clients:
        Arc<tokio::sync::RwLock<HashMap<String, Arc<tokio::sync::Mutex<SharedUdsClient>>>>>,
}

impl TopicBasedPool {
    pub fn new(path: &str, max_concurrent: usize) -> Self {
        Self {
            path: path.to_string(),
            default_pool: Arc::new(UdsClientPool::new(path, max_concurrent)),
            topic_clients: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
        }
    }

    pub async fn call(
        &self,
        service: &str,
        method: &str,
        payload: serde_json::Value,
    ) -> Result<ServiceResponse> {
        self.call_with_topic(service, method, payload, None).await
    }

    pub async fn call_with_topic(
        &self,
        service: &str,
        method: &str,
        payload: serde_json::Value,
        topic: Option<String>,
    ) -> Result<ServiceResponse> {
        match topic {
            Some(ref t) if !t.is_empty() => self.call_serial(service, method, payload, t).await,
            _ => self.default_pool.call(service, method, payload).await,
        }
    }

    async fn call_serial(
        &self,
        service: &str,
        method: &str,
        payload: serde_json::Value,
        topic: &str,
    ) -> Result<ServiceResponse> {
        let client = {
            let mut clients = self.topic_clients.write().await;
            clients
                .entry(topic.to_string())
                .or_insert_with(|| {
                    Arc::new(tokio::sync::Mutex::new(SharedUdsClient::new(&self.path)))
                })
                .clone()
        };

        let client = client.lock().await;
        client.call(service, method, payload).await
    }

    pub async fn call_with_topics(
        &self,
        requests: Vec<(String, String, serde_json::Value, Option<String>)>,
    ) -> Vec<Result<ServiceResponse>> {
        let default_pool = self.default_pool.clone();
        let topic_clients = self.topic_clients.clone();
        let path = self.path.clone();

        let futures: Vec<_> = requests
            .into_iter()
            .map(move |(service, method, payload, topic)| {
                let dp = default_pool.clone();
                let tc = topic_clients.clone();
                let p = path.clone();
                let svc = service.clone();
                let mtd = method.clone();

                async move {
                    match topic {
                        Some(ref t) if !t.is_empty() => {
                            let client = {
                                let mut clients = tc.write().await;
                                clients
                                    .entry(t.to_string())
                                    .or_insert_with(|| {
                                        Arc::new(tokio::sync::Mutex::new(SharedUdsClient::new(&p)))
                                    })
                                    .clone()
                            };
                            let c = client.lock().await;
                            c.call(&svc, &mtd, payload).await
                        }
                        _ => dp.call(&svc, &mtd, payload).await,
                    }
                }
            })
            .collect();

        futures::future::join_all(futures).await
    }

    pub async fn close(&self) {
        self.default_pool.close().await;
        let clients = self.topic_clients.read().await;
        for (_, client) in clients.iter() {
            client.lock().await.close().await;
        }
    }
}

#[derive(Debug, Clone)]
pub struct PoolConfig {
    pub max_concurrent: usize,
    pub request_timeout: Duration,
    pub circuit_breaker_threshold: usize,
    pub circuit_breaker_timeout: Duration,
    pub connection_timeout: Duration,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 100,
            request_timeout: Duration::from_secs(30),
            circuit_breaker_threshold: 5,
            circuit_breaker_timeout: Duration::from_secs(30),
            connection_timeout: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolMetrics {
    pub total_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub circuit_breaker_opens: u64,
    pub circuit_breaker_state: String,
    pub timeout_errors: u64,
    pub active_requests: usize,
}

pub struct ProductionPool {
    path: String,
    config: PoolConfig,
    semaphore: Arc<tokio::sync::Semaphore>,
    client_pool: Arc<tokio::sync::Mutex<Vec<SharedUdsClient>>>,
    circuit_breaker: Arc<CircuitBreaker>,
    metrics: Arc<PoolMetricsInner>,
    shutdown: Arc<AtomicBool>,
}

struct PoolMetricsInner {
    total_requests: AtomicU64,
    successful_requests: AtomicU64,
    failed_requests: AtomicU64,
    circuit_breaker_opens: AtomicU64,
    timeout_errors: AtomicU64,
    active_requests: AtomicUsize,
}

impl ProductionPool {
    pub fn new(path: &str) -> Self {
        Self::with_config(path, PoolConfig::default())
    }

    pub fn with_config(path: &str, config: PoolConfig) -> Self {
        Self {
            path: path.to_string(),
            config: config.clone(),
            semaphore: Arc::new(tokio::sync::Semaphore::new(config.max_concurrent)),
            client_pool: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            circuit_breaker: Arc::new(
                CircuitBreaker::new(config.circuit_breaker_threshold)
                    .with_timeout(config.circuit_breaker_timeout),
            ),
            metrics: Arc::new(PoolMetricsInner {
                total_requests: AtomicU64::new(0),
                successful_requests: AtomicU64::new(0),
                failed_requests: AtomicU64::new(0),
                circuit_breaker_opens: AtomicU64::new(0),
                timeout_errors: AtomicU64::new(0),
                active_requests: AtomicUsize::new(0),
            }),
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    pub async fn call(
        &self,
        service: &str,
        method: &str,
        payload: serde_json::Value,
    ) -> Result<ServiceResponse> {
        self.call_with_timeout(service, method, payload, self.config.request_timeout)
            .await
    }

    pub async fn call_with_timeout(
        &self,
        service: &str,
        method: &str,
        payload: serde_json::Value,
        timeout: Duration,
    ) -> Result<ServiceResponse> {
        if self.shutdown.load(Ordering::SeqCst) {
            return Err(ProtocolError::Server("pool is shutting down".into()));
        }

        self.circuit_breaker.can_execute()?;

        self.metrics.total_requests.fetch_add(1, Ordering::Relaxed);
        self.metrics.active_requests.fetch_add(1, Ordering::Relaxed);

        let permit = self.semaphore.acquire().await.map_err(|_| {
            self.metrics.active_requests.fetch_sub(1, Ordering::Relaxed);
            ProtocolError::ResourceExhausted("pool exhausted".into())
        })?;

        let client = {
            let mut pool = self.client_pool.lock().await;
            pool.pop()
                .unwrap_or_else(|| SharedUdsClient::new(&self.path))
        };

        let result = tokio::time::timeout(timeout, client.call(service, method, payload)).await;

        {
            let mut pool = self.client_pool.lock().await;
            pool.push(client);
        }

        drop(permit);
        self.metrics.active_requests.fetch_sub(1, Ordering::Relaxed);

        match result {
            Ok(Ok(response)) => {
                self.circuit_breaker.record_success();
                self.metrics
                    .successful_requests
                    .fetch_add(1, Ordering::Relaxed);
                Ok(response)
            }
            Ok(Err(e)) => {
                self.circuit_breaker.record_failure();
                self.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
                Err(e)
            }
            Err(_) => {
                self.circuit_breaker.record_failure();
                self.metrics.timeout_errors.fetch_add(1, Ordering::Relaxed);
                self.metrics.failed_requests.fetch_add(1, Ordering::Relaxed);
                Err(ProtocolError::Timeout)
            }
        }
    }

    pub async fn call_batch(
        &self,
        requests: Vec<(String, String, serde_json::Value)>,
    ) -> Vec<Result<ServiceResponse>> {
        self.call_batch_with_timeout(requests, self.config.request_timeout)
            .await
    }

    pub async fn call_batch_with_timeout(
        &self,
        requests: Vec<(String, String, serde_json::Value)>,
        timeout: Duration,
    ) -> Vec<Result<ServiceResponse>> {
        let sem = self.semaphore.clone();
        let pool = self.client_pool.clone();
        let path = self.path.clone();
        let metrics = self.metrics.clone();
        let cb = self.circuit_breaker.clone();
        let shutdown = self.shutdown.clone();
        let total_timeout = timeout;

        let futures: Vec<_> = requests
            .into_iter()
            .map(move |(service, method, payload)| {
                let sem = sem.clone();
                let p = pool.clone();
                let m = metrics.clone();
                let cb = cb.clone();
                let sh = shutdown.clone();
                let svc = service.clone();
                let mtd = method.clone();
                let path_clone = path.clone();

                async move {
                    if sh.load(Ordering::SeqCst) {
                        return Err(ProtocolError::Server("pool is shutting down".into()));
                    }

                    cb.can_execute()?;

                    m.total_requests.fetch_add(1, Ordering::Relaxed);
                    m.active_requests.fetch_add(1, Ordering::Relaxed);

                    let permit = sem.acquire().await.map_err(|_| {
                        m.active_requests.fetch_sub(1, Ordering::Relaxed);
                        ProtocolError::ResourceExhausted("pool exhausted".into())
                    })?;

                    let client = {
                        let mut guard = p.lock().await;
                        guard
                            .pop()
                            .unwrap_or_else(|| SharedUdsClient::new(&path_clone))
                    };

                    let result =
                        tokio::time::timeout(total_timeout, client.call(&svc, &mtd, payload)).await;

                    {
                        let mut guard = p.lock().await;
                        guard.push(client);
                    }

                    drop(permit);
                    m.active_requests.fetch_sub(1, Ordering::Relaxed);

                    match result {
                        Ok(Ok(response)) => {
                            cb.record_success();
                            m.successful_requests.fetch_add(1, Ordering::Relaxed);
                            Ok(response)
                        }
                        Ok(Err(e)) => {
                            cb.record_failure();
                            m.failed_requests.fetch_add(1, Ordering::Relaxed);
                            Err(e)
                        }
                        Err(_) => {
                            cb.record_failure();
                            m.timeout_errors.fetch_add(1, Ordering::Relaxed);
                            m.failed_requests.fetch_add(1, Ordering::Relaxed);
                            Err(ProtocolError::Timeout)
                        }
                    }
                }
            })
            .collect();

        futures::future::join_all(futures).await
    }

    pub fn metrics(&self) -> PoolMetrics {
        PoolMetrics {
            total_requests: self.metrics.total_requests.load(Ordering::Relaxed),
            successful_requests: self.metrics.successful_requests.load(Ordering::Relaxed),
            failed_requests: self.metrics.failed_requests.load(Ordering::Relaxed),
            circuit_breaker_opens: self.metrics.circuit_breaker_opens.load(Ordering::Relaxed),
            circuit_breaker_state: format!("{:?}", self.circuit_breaker.state()),
            timeout_errors: self.metrics.timeout_errors.load(Ordering::Relaxed),
            active_requests: self.metrics.active_requests.load(Ordering::Relaxed),
        }
    }

    pub fn circuit_breaker_state(&self) -> CircuitState {
        self.circuit_breaker.state()
    }

    pub async fn close(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        self.semaphore.close();

        let mut pool = self.client_pool.lock().await;
        for client in pool.iter() {
            client.close().await;
        }
        pool.clear();

        tracing::info!("ProductionPool closed");
    }

    pub fn is_shutting_down(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }
}

impl Clone for ProductionPool {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            config: self.config.clone(),
            semaphore: self.semaphore.clone(),
            client_pool: self.client_pool.clone(),
            circuit_breaker: self.circuit_breaker.clone(),
            metrics: self.metrics.clone(),
            shutdown: self.shutdown.clone(),
        }
    }
}
