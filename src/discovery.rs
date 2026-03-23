use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ServiceEndpoint {
    pub name: String,
    pub address: String,
    pub weight: usize,
    pub metadata: HashMap<String, String>,
}

impl ServiceEndpoint {
    pub fn new(name: &str, address: &str) -> Self {
        Self {
            name: name.to_string(),
            address: address.to_string(),
            weight: 1,
            metadata: HashMap::new(),
        }
    }

    pub fn with_weight(mut self, weight: usize) -> Self {
        self.weight = weight;
        self
    }

    pub fn with_metadata(mut self, key: &str, value: &str) -> Self {
        self.metadata.insert(key.to_string(), value.to_string());
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoadBalancingStrategy {
    #[default]
    RoundRobin,
    Random,
    ConsistentHash,
}

pub struct ServiceDiscovery {
    endpoints: parking_lot::RwLock<HashMap<String, Vec<ServiceEndpoint>>>,
    round_robin_lbs: parking_lot::RwLock<
        HashMap<
            String,
            Arc<
                pingora_load_balancing::LoadBalancer<pingora_load_balancing::selection::RoundRobin>,
            >,
        >,
    >,
    random_lbs: parking_lot::RwLock<
        HashMap<
            String,
            Arc<pingora_load_balancing::LoadBalancer<pingora_load_balancing::selection::Random>>,
        >,
    >,
    consistent_lbs: parking_lot::RwLock<
        HashMap<
            String,
            Arc<
                pingora_load_balancing::LoadBalancer<pingora_load_balancing::selection::Consistent>,
            >,
        >,
    >,
    strategy: LoadBalancingStrategy,
}

impl ServiceDiscovery {
    pub fn new() -> Self {
        Self {
            endpoints: parking_lot::RwLock::new(HashMap::new()),
            round_robin_lbs: parking_lot::RwLock::new(HashMap::new()),
            random_lbs: parking_lot::RwLock::new(HashMap::new()),
            consistent_lbs: parking_lot::RwLock::new(HashMap::new()),
            strategy: LoadBalancingStrategy::RoundRobin,
        }
    }

    pub fn with_strategy(mut self, strategy: LoadBalancingStrategy) -> Self {
        self.strategy = strategy;
        self
    }

    pub fn register(&self, endpoint: ServiceEndpoint) {
        let mut endpoints = self.endpoints.write();
        let name = endpoint.name.clone();
        endpoints.entry(name.clone()).or_default().push(endpoint);

        drop(endpoints);
        self.rebuild_load_balancers(&name);
    }

    pub fn unregister(&self, name: &str, address: &str) {
        let mut endpoints = self.endpoints.write();
        if let Some(eps) = endpoints.get_mut(name) {
            eps.retain(|e| e.address != address);
        }
        drop(endpoints);
        self.rebuild_load_balancers(name);
    }

    fn rebuild_load_balancers(&self, name: &str) {
        let endpoints = self.endpoints.read();
        if let Some(eps) = endpoints.get(name) {
            let addrs: Vec<&str> = eps.iter().map(|e| e.address.as_str()).collect();

            if let Ok(lb) = pingora_load_balancing::LoadBalancer::<
                pingora_load_balancing::selection::RoundRobin,
            >::try_from_iter(addrs.clone())
            {
                self.round_robin_lbs
                    .write()
                    .insert(name.to_string(), Arc::new(lb));
            }
            if let Ok(lb) = pingora_load_balancing::LoadBalancer::<
                pingora_load_balancing::selection::Random,
            >::try_from_iter(addrs.clone())
            {
                self.random_lbs
                    .write()
                    .insert(name.to_string(), Arc::new(lb));
            }
            if let Ok(lb) = pingora_load_balancing::LoadBalancer::<
                pingora_load_balancing::selection::Consistent,
            >::try_from_iter(addrs)
            {
                self.consistent_lbs
                    .write()
                    .insert(name.to_string(), Arc::new(lb));
            }
        }
    }

    pub fn discover(&self, name: &str) -> Vec<ServiceEndpoint> {
        let endpoints = self.endpoints.read();
        endpoints.get(name).cloned().unwrap_or_default()
    }

    pub fn get_load_balancer(
        &self,
        name: &str,
    ) -> Option<
        Arc<pingora_load_balancing::LoadBalancer<pingora_load_balancing::selection::RoundRobin>>,
    > {
        self.round_robin_lbs.read().get(name).cloned()
    }

    pub fn select(&self, name: &str) -> Option<pingora_load_balancing::Backend> {
        match self.strategy {
            LoadBalancingStrategy::RoundRobin => self
                .round_robin_lbs
                .read()
                .get(name)
                .and_then(|lb| lb.select(b"", 256)),
            LoadBalancingStrategy::Random => self
                .random_lbs
                .read()
                .get(name)
                .and_then(|lb| lb.select(b"", 256)),
            LoadBalancingStrategy::ConsistentHash => self
                .consistent_lbs
                .read()
                .get(name)
                .and_then(|lb| lb.select(b"", 256)),
        }
    }

    pub fn select_with_key(
        &self,
        name: &str,
        key: &[u8],
    ) -> Option<pingora_load_balancing::Backend> {
        match self.strategy {
            LoadBalancingStrategy::RoundRobin => self
                .round_robin_lbs
                .read()
                .get(name)
                .and_then(|lb| lb.select(key, 256)),
            LoadBalancingStrategy::Random => self
                .random_lbs
                .read()
                .get(name)
                .and_then(|lb| lb.select(key, 256)),
            LoadBalancingStrategy::ConsistentHash => self
                .consistent_lbs
                .read()
                .get(name)
                .and_then(|lb| lb.select(key, 256)),
        }
    }

    pub fn set_backend_health(&self, name: &str, address: &str, healthy: bool) {
        let _ = (name, address, healthy);
    }

    pub fn get_backend_health(&self, name: &str, address: &str) -> bool {
        let _ = (name, address);
        true
    }

    pub fn strategy(&self) -> LoadBalancingStrategy {
        self.strategy
    }

    pub fn list_services(&self) -> Vec<String> {
        let endpoints = self.endpoints.read();
        endpoints.keys().cloned().collect()
    }
}

impl Default for ServiceDiscovery {
    fn default() -> Self {
        Self::new()
    }
}

pub struct BackgroundHealthChecker {
    service_discovery: Arc<ServiceDiscovery>,
    interval: Duration,
    timeout: Duration,
    running: AtomicBool,
}

impl BackgroundHealthChecker {
    pub fn new(service_discovery: Arc<ServiceDiscovery>) -> Self {
        Self {
            service_discovery,
            interval: Duration::from_secs(30),
            timeout: Duration::from_secs(5),
            running: AtomicBool::new(false),
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

    pub fn start(&self, shutdown: crate::utils::CancellationToken) {
        if self.running.swap(true, Ordering::SeqCst) {
            return;
        }

        let interval = self.interval;
        let timeout = self.timeout;
        let sd = Arc::clone(&self.service_discovery);

        tokio::spawn(async move {
            let mut interval_timer = tokio::time::interval(interval);

            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => {
                        break;
                    }
                    _ = interval_timer.tick() => {
                        Self::check_all_services(&sd, timeout).await;
                    }
                }
            }
        });
    }

    async fn check_all_services(sd: &ServiceDiscovery, timeout: Duration) {
        let services = sd.list_services();

        for service_name in services {
            let endpoints = sd.discover(&service_name);

            for endpoint in endpoints {
                let addr = &endpoint.address;
                let healthy = tokio::time::timeout(timeout, tokio::net::TcpStream::connect(addr))
                    .await
                    .map(|r| r.is_ok())
                    .unwrap_or(false);

                sd.set_backend_health(&service_name, addr, healthy);
            }
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}
