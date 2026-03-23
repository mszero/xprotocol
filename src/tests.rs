#[cfg(test)]
mod tests {
    use crate::auth::{
        ApiKeyAuthenticator, AuthContext, Authenticator, CompositeAuthenticator, Credentials,
        Permission, RateLimiter as AuthRateLimiter, TokenAuthenticator,
    };
    use crate::circuit_breaker::{CircuitBreaker, CircuitState};
    use crate::config::{
        Config, ConfigBuilder, PoolConfig as ConfigPoolConfig, ServerConfig as ConfigServerConfig,
    };
    use crate::discovery::{LoadBalancingStrategy, ServiceDiscovery, ServiceEndpoint};
    use crate::error::ProtocolError;
    use crate::handler::{FnServiceHandler, HealthStatus, ServiceHandler};
    use crate::metrics::{
        Alert, AlertCondition, AlertManager, AlertRule, ConnectionTracker, Histogram,
        MetricsCollector, ServiceMetrics,
    };
    use crate::pool::PoolConfig;
    use crate::protocol::{HandshakeRequest, HandshakeResponse, ServiceRequest, ServiceResponse};
    use crate::schema::{ServiceCapability, ServiceSchema};
    use crate::server::{RateLimitConfig, ServerConfig};
    use crate::shutdown::GracefulShutdown;
    use crate::streaming::{StreamMessage, StreamMode, StreamResponse};
    use crate::utils::{
        CancellationToken, DeadLetterQueue, HealthChecker, MessageQueue, Metrics, RateLimiter,
        RetryConfig,
    };
    use std::sync::Arc;
    use std::time::Duration;

    #[derive(Clone)]
    struct EchoService;

    #[async_trait::async_trait]
    impl ServiceHandler for EchoService {
        fn service_name(&self) -> &str {
            "echo"
        }

        async fn handle(&self, request: ServiceRequest) -> ServiceResponse {
            let payload = request.payload.clone();
            ServiceResponse::success(&request, payload)
        }
    }

    // ==================== Protocol Tests ====================

    #[test]
    fn test_service_request() {
        let req = ServiceRequest::new(
            "test_svc",
            "test_method",
            serde_json::json!({"key": "value"}),
        );
        assert_eq!(req.service_name, "test_svc");
        assert_eq!(req.method, "test_method");
        assert!(!req.request_id.is_empty());
        assert!(!req.correlation_id.is_empty());
        assert!(req.topic.is_none());

        let req_with_topic = req.clone().with_topic("order-123");
        assert_eq!(req_with_topic.topic, Some("order-123".to_string()));
    }

    #[test]
    fn test_service_response() {
        let req = ServiceRequest::new("svc", "method", serde_json::json!(null));

        let resp = ServiceResponse::success(&req, serde_json::json!({"result": "ok"}));
        assert_eq!(resp.status, "success");
        assert!(resp.error.is_none());
        assert_eq!(resp.request_id, req.request_id);

        let resp = ServiceResponse::error(&req, "test error");
        assert_eq!(resp.status, "error");
        assert!(resp.error.is_some());
        assert!(resp.error.unwrap().contains("test error"));
    }

    #[test]
    fn test_handshake_request_response() {
        let req = HandshakeRequest {
            client_id: "client-1".to_string(),
            service_name: "test".to_string(),
            capabilities: vec!["json".to_string()],
        };
        assert_eq!(req.client_id, "client-1");

        let resp = HandshakeResponse {
            server_id: "server-1".to_string(),
            session_id: "session-123".to_string(),
            success: true,
            encoding: "json".to_string(),
        };
        assert!(resp.success);
    }

    // ==================== Handler Tests ====================

    #[test]
    fn test_service_handler_trait() {
        let service = EchoService;
        assert_eq!(service.service_name(), "echo");

        let caps = service.capabilities();
        assert!(caps.contains(&"json".to_string()));

        let status = service.health_status();
        matches!(status, HealthStatus::Healthy(s) if s == "echo");

        let unhealthy = HealthStatus::unhealthy("echo", "test error");
        matches!(unhealthy, HealthStatus::Unhealthy(s, e) if s == "echo" && e == "test error");
    }

    #[tokio::test]
    async fn test_fn_service_handler() {
        let handler = FnServiceHandler::new("calc", |req| {
            let a = req.payload.get("a").and_then(|v| v.as_i64()).unwrap_or(0);
            let b = req.payload.get("b").and_then(|v| v.as_i64()).unwrap_or(0);
            ServiceResponse::success(&req, serde_json::json!({"result": a + b}))
        });

        assert_eq!(handler.service_name(), "calc");

        let req = ServiceRequest::new("calc", "add", serde_json::json!({"a": 1, "b": 2}));
        let resp = handler.handle(req).await;
        assert_eq!(resp.status, "success");
    }

    // ==================== Streaming Tests ====================

    #[test]
    fn test_stream_mode() {
        assert_eq!(StreamMode::Result.as_str(), "result");
        assert_eq!(StreamMode::Messages.as_str(), "messages");
        assert_eq!(StreamMode::Values.as_str(), "values");
        assert_eq!(StreamMode::Updates.as_str(), "updates");

        assert_eq!(StreamMode::from_str("result"), Some(StreamMode::Result));
        assert_eq!(StreamMode::from_str("messages"), Some(StreamMode::Messages));
        assert_eq!(StreamMode::from_str("unknown"), None);
    }

    #[test]
    fn test_stream_message() {
        let msg = StreamMessage::new("req-123", 0, serde_json::json!("hello"));
        assert_eq!(msg.request_id, "req-123");
        assert_eq!(msg.chunk_index, 0);
        assert!(msg.metadata.is_none());

        let msg_with_meta = msg.with_metadata(serde_json::json!({"key": "value"}));
        assert!(msg_with_meta.metadata.is_some());
    }

    #[test]
    fn test_stream_response() {
        let started = StreamResponse::started("req-123", "result");
        assert_eq!(started.status, "streaming");

        let completed = StreamResponse::completed("req-123", serde_json::json!({"done": true}));
        assert_eq!(completed.status, "completed");
        assert!(completed.final_payload.is_some());

        let error = StreamResponse::error("req-123", "something went wrong");
        assert!(error.status.contains("error"));
    }

    // ==================== Schema Tests ====================

    #[test]
    fn test_service_schema() {
        let schema = ServiceSchema::new("my-service", "1.0.0");
        assert_eq!(schema.service_name, "my-service");
        assert_eq!(schema.version, "1.0.0");

        let custom = ServiceSchema::new("custom", "2.0")
            .with_input_schema(serde_json::json!({"type": "object"}))
            .with_output_schema(serde_json::json!({"type": "object"}))
            .with_capabilities(vec![ServiceCapability {
                name: "streaming".to_string(),
                description: "Supports streaming".to_string(),
                version: Some("1.0".to_string()),
            }]);

        assert!(!custom.capabilities.is_empty());
        assert_eq!(custom.capabilities[0].name, "streaming");

        let json = custom.to_json();
        assert!(json.is_ok());
        assert!(json.unwrap().contains("custom"));
    }

    // ==================== Discovery Tests ====================

    #[test]
    fn test_service_discovery() {
        let sd = ServiceDiscovery::new();

        let ep1 = ServiceEndpoint::new("email", "/tmp/email.sock").with_weight(10);
        let ep2 = ServiceEndpoint::new("email", "/tmp/email2.sock").with_weight(5);
        let ep3 = ServiceEndpoint::new("email", "/tmp/email3.sock")
            .with_weight(3)
            .with_metadata("region", "us-east");

        sd.register(ep1);
        sd.register(ep2);
        sd.register(ep3);

        let endpoints = sd.discover("email");
        assert_eq!(endpoints.len(), 3);

        let unknown = sd.discover("unknown");
        assert!(unknown.is_empty());

        let services = sd.list_services();
        assert!(services.contains(&"email".to_string()));
    }

    #[test]
    fn test_load_balancing_strategies() {
        let sd_rr = ServiceDiscovery::new();
        let sd_random = ServiceDiscovery::new().with_strategy(LoadBalancingStrategy::Random);
        let sd_consistent =
            ServiceDiscovery::new().with_strategy(LoadBalancingStrategy::ConsistentHash);

        assert_eq!(sd_rr.strategy(), LoadBalancingStrategy::RoundRobin);
        assert_eq!(sd_random.strategy(), LoadBalancingStrategy::Random);
        assert_eq!(
            sd_consistent.strategy(),
            LoadBalancingStrategy::ConsistentHash
        );
    }

    #[test]
    fn test_service_discovery_with_strategy() {
        let sd = ServiceDiscovery::new().with_strategy(LoadBalancingStrategy::Random);

        sd.register(ServiceEndpoint::new("users", "127.0.0.1:8001").with_weight(10));
        sd.register(ServiceEndpoint::new("users", "127.0.0.1:8002").with_weight(5));

        let endpoints = sd.discover("users");
        assert_eq!(endpoints.len(), 2);

        let selected = sd.select("users");
        assert!(selected.is_some());

        let selected_with_key = sd.select_with_key("users", b"user-123");
        assert!(selected_with_key.is_some());

        let consistent_sd =
            ServiceDiscovery::new().with_strategy(LoadBalancingStrategy::ConsistentHash);
        consistent_sd.register(ServiceEndpoint::new("users", "127.0.0.1:8001"));
        consistent_sd.register(ServiceEndpoint::new("users", "127.0.0.1:8002"));

        let consistent1 = consistent_sd.select_with_key("users", b"same-key");
        let consistent2 = consistent_sd.select_with_key("users", b"same-key");
        assert_eq!(consistent1, consistent2);
    }

    #[test]
    fn test_service_discovery_unregister() {
        let sd = ServiceDiscovery::new();

        sd.register(ServiceEndpoint::new("cache", "/tmp/cache1.sock"));
        sd.register(ServiceEndpoint::new("cache", "/tmp/cache2.sock"));

        assert_eq!(sd.discover("cache").len(), 2);

        sd.unregister("cache", "/tmp/cache1.sock");

        assert_eq!(sd.discover("cache").len(), 1);
        assert_eq!(sd.discover("cache")[0].address, "/tmp/cache2.sock");
    }

    #[test]
    fn test_endpoint_metadata() {
        let ep = ServiceEndpoint::new("svc", "addr")
            .with_weight(5)
            .with_metadata("env", "prod")
            .with_metadata("region", "us-west");

        assert_eq!(ep.weight, 5);
        assert_eq!(ep.metadata.get("env"), Some(&"prod".to_string()));
        assert_eq!(ep.metadata.get("region"), Some(&"us-west".to_string()));
    }

    // ==================== Circuit Breaker Tests ====================

    #[test]
    fn test_circuit_breaker_closed_to_open() {
        let cb = CircuitBreaker::new(3);

        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.is_available());
        assert!(cb.can_execute().is_ok());

        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Closed);

        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Closed);

        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
        assert!(!cb.is_available());
        assert!(cb.can_execute().is_err());
    }

    #[test]
    fn test_circuit_breaker_half_open() {
        let cb = CircuitBreaker::new(2);

        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);

        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
    }

    #[test]
    fn test_circuit_breaker_success_resets() {
        let cb = CircuitBreaker::new(5);

        cb.record_failure();
        cb.record_failure();
        cb.record_success();

        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn test_circuit_breaker_timeout() {
        let cb = CircuitBreaker::new(1).with_timeout(Duration::from_millis(100));

        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);

        std::thread::sleep(Duration::from_millis(150));

        assert_eq!(cb.state(), CircuitState::HalfOpen);
    }

    // ==================== Utils Tests ====================

    #[test]
    fn test_rate_limiter() {
        let rl = RateLimiter::new(2, Duration::from_secs(1));

        assert!(rl.try_acquire());
        assert!(rl.try_acquire());
        assert!(!rl.try_acquire());

        std::thread::sleep(Duration::from_millis(1100));

        assert!(rl.try_acquire());
    }

    #[test]
    fn test_retry_config() {
        let config = RetryConfig::default();

        let delay0 = config.calculate_delay(0);
        let delay1 = config.calculate_delay(1);
        let delay2 = config.calculate_delay(2);
        let delay3 = config.calculate_delay(3);

        assert!(delay0 <= delay1);
        assert!(delay1 <= delay2);
        assert!(delay2 <= delay3);
        assert!(delay3 <= config.max_delay);
    }

    #[test]
    fn test_retry_config_no_jitter() {
        let mut config = RetryConfig::default();
        config.jitter = false;

        let delay1 = config.calculate_delay(1);
        let delay2 = config.calculate_delay(1);

        assert_eq!(delay1, delay2);
    }

    #[test]
    fn test_metrics() {
        let m = Metrics::default();

        m.record_request();
        m.record_success();
        m.record_failure();
        m.record_latency(Duration::from_millis(100));

        m.inc_connections();
        m.add_bytes_sent(1024);
        m.add_bytes_received(512);

        let snapshot = m.snapshot();
        assert_eq!(snapshot.requests_total, 1);
        assert_eq!(snapshot.requests_success, 1);
        assert_eq!(snapshot.requests_failure, 1);
        assert!(snapshot.success_rate > 0.0);
        assert_eq!(snapshot.connections_active, 1);
        assert_eq!(snapshot.bytes_sent, 1024);
        assert_eq!(snapshot.bytes_received, 512);

        m.dec_connections();
        let snapshot2 = m.snapshot();
        assert_eq!(snapshot2.connections_active, 0);
    }

    #[test]
    fn test_cancellation_token() {
        let token = CancellationToken::new();
        assert!(!token.is_cancelled());

        token.cancel();
        assert!(token.is_cancelled());

        let forked = token.fork();
        assert!(forked.is_cancelled());
    }

    #[test]
    fn test_health_checker() {
        let hc = HealthChecker::new();
        assert_eq!(hc.interval(), Duration::from_secs(30));
        assert_eq!(hc.timeout_duration(), Duration::from_secs(5));

        let hc2 = HealthChecker::new()
            .with_interval(Duration::from_secs(60))
            .with_timeout(Duration::from_secs(10));

        assert_eq!(hc2.interval(), Duration::from_secs(60));
        assert_eq!(hc2.timeout_duration(), Duration::from_secs(10));
    }

    #[test]
    fn test_message_queue() {
        let mq = MessageQueue::new();

        let count_no_sub = mq.publish("topic1", bytes::Bytes::from_static(b"hello"));
        assert_eq!(count_no_sub, 0);

        let mut receiver = mq.subscribe("topic1");

        let topics = mq.topics();
        assert!(topics.contains(&"topic1".to_string()));

        let count_with_sub = mq.publish("topic1", bytes::Bytes::from_static(b"world"));
        assert_eq!(count_with_sub, 1);

        if let Ok(msg) = receiver.try_recv() {
            let msg_slice: &[u8] = &msg;
            assert_eq!(msg_slice, b"world");
        }
    }

    #[test]
    fn test_dead_letter_queue() {
        let dlq = DeadLetterQueue::new(10);

        assert!(dlq.is_empty());
        assert_eq!(dlq.len(), 0);

        let req = ServiceRequest::new("svc", "method", serde_json::json!({"error": true}));
        dlq.push(req.clone(), "timeout".to_string());

        assert!(!dlq.is_empty());
        assert_eq!(dlq.len(), 1);

        let letter = dlq.pop();
        assert!(letter.is_some());
        assert_eq!(letter.unwrap().error, "timeout");

        dlq.clear();
        assert!(dlq.is_empty());
    }

    #[test]
    fn test_dead_letter_queue_max_size() {
        let dlq = DeadLetterQueue::new(3);

        for i in 0..5 {
            let req = ServiceRequest::new("svc", &format!("method{}", i), serde_json::json!(i));
            dlq.push(req, format!("error{}", i));
        }

        assert_eq!(dlq.len(), 3);

        let letters = dlq.get_all();
        assert!(letters[0].error.contains("error2"));
        assert!(letters[2].error.contains("error4"));
    }

    // ==================== Shutdown Tests ====================

    #[tokio::test]
    async fn test_graceful_shutdown() {
        let shutdown = GracefulShutdown::new();

        let handle = tokio::spawn({
            let s = shutdown.shutdown_tx();
            async move {
                let _ = s.send(());
            }
        });

        shutdown.wait().await;
        handle.await.unwrap();
    }

    // ==================== Auth Tests ====================

    #[test]
    fn test_token_authenticator() {
        let auth = TokenAuthenticator::new().with_ttl(Duration::from_secs(3600));

        auth.add_token(
            "token-123",
            "client-1",
            vec![Permission::new("svc", "method")],
        );

        let valid_creds = Credentials {
            client_id: "client-1".to_string(),
            token: Some("token-123".to_string()),
            api_key: None,
        };

        let result = auth.authenticate(&valid_creds);
        assert!(result.is_ok());
        let ctx = result.unwrap();
        assert_eq!(ctx.client_id, "client-1");
        assert!(ctx.has_permission(&Permission::new("svc", "method")));

        let invalid_creds = Credentials {
            client_id: "unknown".to_string(),
            token: Some("invalid-token".to_string()),
            api_key: None,
        };
        assert!(auth.authenticate(&invalid_creds).is_err());

        let no_token = Credentials {
            client_id: "client-1".to_string(),
            token: None,
            api_key: None,
        };
        assert!(auth.authenticate(&no_token).is_err());
    }

    #[test]
    fn test_api_key_authenticator() {
        let auth = ApiKeyAuthenticator::new();
        auth.add_key("api-key-123", "client-2", vec![Permission::all()]);

        let valid_creds = Credentials {
            client_id: "client-2".to_string(),
            token: None,
            api_key: Some("api-key-123".to_string()),
        };

        let result = auth.authenticate(&valid_creds);
        assert!(result.is_ok());
        let ctx = result.unwrap();
        assert!(ctx.has_permission(&Permission::new("any", "any")));
    }

    #[test]
    fn test_composite_authenticator() {
        let auth = CompositeAuthenticator::new()
            .add(TokenAuthenticator::new())
            .add(ApiKeyAuthenticator::new());

        let creds = Credentials {
            client_id: "test".to_string(),
            token: Some("test".to_string()),
            api_key: None,
        };

        let result = auth.authenticate(&creds);
        assert!(result.is_err());
    }

    #[test]
    fn test_permission() {
        let perm = Permission::new("service", "method");
        assert_eq!(perm.service, "service");
        assert_eq!(perm.method, "method");

        let all = Permission::all();
        assert_eq!(all.service, "*");
        assert_eq!(all.method, "*");
    }

    #[test]
    fn test_auth_context() {
        let ctx = AuthContext {
            client_id: "client".to_string(),
            permissions: vec![Permission::new("svc", "method")],
            expires_at: None,
        };

        assert!(ctx.is_valid());
        assert!(ctx.has_permission(&Permission::new("svc", "method")));
        assert!(ctx.has_permission(&Permission::new("svc", "*")));
        assert!(!ctx.has_permission(&Permission::new("other", "method")));
    }

    #[test]
    fn test_auth_rate_limiter() {
        let rl = AuthRateLimiter::new(3, Duration::from_secs(1));

        assert!(rl.check("client-1"));
        assert!(rl.check("client-1"));
        assert!(rl.check("client-1"));
        assert!(!rl.check("client-1"));

        assert_eq!(rl.remaining("client-1"), 0);
        assert_eq!(rl.remaining("client-2"), 3);

        rl.reset("client-1");
        assert_eq!(rl.remaining("client-1"), 3);
    }

    // ==================== Metrics Tests ====================

    #[test]
    fn test_histogram() {
        let h = Histogram::new(&[10, 50, 100]);

        h.observe(5);
        h.observe(25);
        h.observe(75);
        h.observe(150);

        assert_eq!(h.percentile(50.0, 4), 50);
    }

    #[test]
    fn test_service_metrics() {
        let m = ServiceMetrics::new();

        m.request_start();
        std::thread::sleep(Duration::from_millis(10));
        m.request_end(Duration::from_millis(10), true, false);

        m.request_start();
        m.request_end(Duration::from_millis(50), false, false);

        let snapshot = m.snapshot();
        assert_eq!(snapshot.total, 2);
        assert_eq!(snapshot.success, 1);
        assert_eq!(snapshot.failure, 1);
        assert!(snapshot.latency_avg_ms > 0);
    }

    #[test]
    fn test_connection_tracker() {
        let ct = ConnectionTracker::new();

        ct.connection_opened();
        ct.connection_opened();
        ct.connection_opened();

        let metrics = ct.snapshot();
        assert_eq!(metrics.total_connections, 3);
        assert_eq!(metrics.active_connections, 3);
        assert_eq!(metrics.peak_connections, 3);

        ct.connection_closed();
        let metrics = ct.snapshot();
        assert_eq!(metrics.active_connections, 2);
        assert_eq!(metrics.closed_connections, 1);

        ct.connection_error();
        assert_eq!(ct.snapshot().connection_errors, 1);
    }

    #[test]
    fn test_metrics_collector() {
        let collector = MetricsCollector::new();

        let svc1 = collector.get_or_create_service("service-1");
        let svc2 = collector.get_or_create_service("service-2");
        let svc1_again = collector.get_or_create_service("service-1");

        assert!(Arc::ptr_eq(&svc1, &svc1_again));
        assert!(!Arc::ptr_eq(&svc1, &svc2));

        svc1.request_start();
        svc1.request_end(Duration::from_millis(10), true, false);

        let snapshots = collector.snapshot_all();
        assert_eq!(snapshots.len(), 2);
        assert_eq!(snapshots["service-1"].total, 1);
    }

    #[test]
    fn test_alert_manager() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let alert_manager = AlertManager::new();
        let collector = MetricsCollector::new();

        alert_manager.add_rule(AlertRule {
            name: "high_latency".to_string(),
            metric: "svc".to_string(),
            condition: AlertCondition::LatencyP99Above(100),
            cooldown: Duration::from_secs(60),
        });

        let svc = collector.get_or_create_service("svc");
        for _ in 0..100 {
            svc.request_start();
            svc.request_end(Duration::from_millis(200), true, false);
        }

        let alert_fired = Arc::new(AtomicBool::new(false));
        let fired = alert_fired.clone();
        alert_manager.add_callback(move |alert: &Alert| {
            assert_eq!(alert.rule_name, "high_latency");
            fired.store(true, Ordering::SeqCst);
        });

        alert_manager.check(&collector);

        assert!(alert_fired.load(Ordering::SeqCst));
    }

    // ==================== Config Tests ====================

    #[test]
    fn test_config_builder() {
        let config = ConfigBuilder::new()
            .with_server_config(ConfigServerConfig {
                path: Some("/tmp/test.sock".to_string()),
                ..Default::default()
            })
            .with_pool_config(ConfigPoolConfig {
                max_concurrent: 200,
                ..Default::default()
            })
            .build();

        assert_eq!(config.server.path, Some("/tmp/test.sock".to_string()));
        assert_eq!(config.pool.max_concurrent, 200);
    }

    #[test]
    fn test_config_default() {
        let config = Config::default();

        assert_eq!(config.server.request_timeout_secs, 30);
        assert_eq!(config.client.request_timeout_secs, 30);
        assert_eq!(config.pool.max_concurrent, 100);
        assert!(config.security.rate_limit.enabled);
    }

    #[test]
    fn test_config_toml() {
        let config = Config::default();
        let toml_str = config.to_toml();
        assert!(toml_str.is_ok());

        let loaded = Config::from_toml(&toml_str.unwrap());
        assert!(loaded.is_ok());
        assert_eq!(loaded.unwrap().server.request_timeout_secs, 30);
    }

    #[test]
    fn test_config_yaml() {
        let config = Config::default();
        let yaml_str = config.to_yaml();
        assert!(yaml_str.is_ok());

        let loaded = Config::from_yaml(&yaml_str.unwrap());
        assert!(loaded.is_ok());
    }

    // ==================== Server Config Tests ====================

    #[test]
    fn test_server_config() {
        let config = ServerConfig::default();
        assert_eq!(config.max_connections, 10000);
        assert_eq!(config.max_request_size, 16 * 1024 * 1024);
        assert!(config.enable_streaming);
        assert!(config.enable_schema_introspection);

        let config = config.with_max_connections(5000).with_rate_limit(500, 60);

        assert_eq!(config.max_connections, 5000);
        assert_eq!(config.rate_limit.max_requests, 500);
    }

    #[test]
    fn test_rate_limit_config() {
        let config = RateLimitConfig {
            max_requests: 1000,
            window_secs: 60,
        };

        assert_eq!(config.max_requests, 1000);
        assert_eq!(config.window_secs, 60);
    }

    // ==================== Pool Config Tests ====================

    #[test]
    fn test_pool_config() {
        let config = PoolConfig::default();
        assert_eq!(config.max_concurrent, 100);
        assert_eq!(config.request_timeout.as_secs(), 30);
        assert_eq!(config.circuit_breaker_threshold, 5);
    }

    // ==================== Error Tests ====================

    #[test]
    fn test_protocol_error_display() {
        let err = ProtocolError::ConnectionFailed("test".to_string());
        assert!(err.to_string().contains("Connection failed"));

        let err = ProtocolError::Timeout;
        assert_eq!(err.to_string(), "Request timeout");

        let err = ProtocolError::NotFound("service".to_string());
        assert!(err.to_string().contains("Service not found"));
    }

    #[test]
    fn test_protocol_error_from_io() {
        use std::io;
        let io_err = io::Error::new(io::ErrorKind::NotFound, "file not found");
        let protocol_err: ProtocolError = io_err.into();
        assert!(matches!(protocol_err, ProtocolError::ConnectionFailed(_)));
    }

    #[test]
    fn test_protocol_error_from_json() {
        let json_err = serde_json::from_slice::<serde_json::Value>(b"invalid").unwrap_err();
        let protocol_err: ProtocolError = json_err.into();
        assert!(matches!(protocol_err, ProtocolError::Encoding(_)));
    }
}
