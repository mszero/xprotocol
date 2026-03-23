use crate::auth::RateLimiter;
use crate::error::{ProtocolError, Result};
use crate::handler::ServiceHandler;
use crate::metrics::MetricsCollector;
use crate::protocol::{HandshakeRequest, ServiceRequest};
use crate::schema::ServiceSchema;
use crate::streaming::StreamMode;
use crate::webhook::{WebhookConfig, WebhookService};
use crate::wire::{
    read_message, write_message, MSG_CLOSE, MSG_ERROR, MSG_HANDSHAKE, MSG_HANDSHAKE_ACK, MSG_PING,
    MSG_PONG, MSG_REQUEST, MSG_RESPONSE, MSG_SCHEMA_REQUEST, MSG_SCHEMA_RESPONSE, MSG_STREAM_END,
    MSG_STREAM_REQUEST, MSG_STREAM_START,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

static ACTIVE_CONNECTIONS: AtomicUsize = AtomicUsize::new(0);

pub struct UdsServer {
    path: String,
    webhook_url: Option<String>,
    metrics: Option<Arc<MetricsCollector>>,
}

impl UdsServer {
    pub fn new(path: &str) -> Self {
        Self {
            path: path.to_string(),
            webhook_url: None,
            metrics: None,
        }
    }

    pub fn with_webhook(mut self, url: &str) -> Self {
        self.webhook_url = Some(url.to_string());
        self
    }

    pub fn with_metrics(mut self, metrics: Arc<MetricsCollector>) -> Self {
        self.metrics = Some(metrics);
        self
    }

    pub async fn run<H>(&self, handler: Arc<H>) -> Result<()>
    where
        H: ServiceHandler + 'static,
    {
        let mut config = ServerConfig::default();
        if let Some(ref url) = self.webhook_url {
            config = config.with_webhook(url);
        }
        self.run_with_config(handler, config).await
    }

    pub async fn run_with_config<H>(&self, handler: Arc<H>, config: ServerConfig) -> Result<()>
    where
        H: ServiceHandler + 'static,
    {
        if std::path::Path::new(&self.path).exists() {
            std::fs::remove_file(&self.path)?;
        }

        let listener = tokio::net::UnixListener::bind(&self.path)?;

        tracing::info!(
            "UDS Server listening on {} (max_connections: {})",
            self.path,
            config.max_connections
        );

        let webhook_service = if let Some(ref url) = config.webhook_url {
            Some(Arc::new(
                WebhookService::new().with_webhook(WebhookConfig::new(url)),
            ))
        } else {
            None
        };

        let rate_limiter = Arc::new(RateLimiter::new(
            config.rate_limit.max_requests,
            Duration::from_secs(config.rate_limit.window_secs),
        ));

        let metrics = self.metrics.clone();
        let max_connections = config.max_connections;

        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let current = ACTIVE_CONNECTIONS.load(Ordering::Relaxed);
                    if current >= max_connections {
                        tracing::warn!(
                            "Connection rejected: max connections ({}) reached",
                            max_connections
                        );
                        continue;
                    }

                    let h = handler.clone();
                    let ws = webhook_service.clone();
                    let cfg = config.clone();
                    let rl = rate_limiter.clone();
                    let m = metrics.clone();

                    ACTIVE_CONNECTIONS.fetch_add(1, Ordering::Relaxed);

                    tokio::spawn(async move {
                        if let Err(e) =
                            Self::handle_connection_with_config(stream, h, ws, cfg, rl, m).await
                        {
                            tracing::error!("Connection error: {}", e);
                        }
                        ACTIVE_CONNECTIONS.fetch_sub(1, Ordering::Relaxed);
                    });
                }
                Err(e) => {
                    tracing::error!("Accept error: {}", e);
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        }
    }

    async fn handle_connection_with_config<H>(
        stream: tokio::net::UnixStream,
        handler: Arc<H>,
        webhook_service: Option<Arc<WebhookService>>,
        config: ServerConfig,
        rate_limiter: Arc<RateLimiter>,
        metrics: Option<Arc<MetricsCollector>>,
    ) -> Result<()>
    where
        H: ServiceHandler + 'static,
    {
        let start_time = Instant::now();

        let (reader, writer) = stream.into_split();
        let mut reader = tokio::io::BufReader::new(reader);
        let mut writer = tokio::io::BufWriter::new(writer);

        let (msg_type, payload) = read_message(&mut reader).await?;

        if msg_type != MSG_HANDSHAKE {
            return Err(ProtocolError::Protocol("expected handshake".into()));
        }

        if payload.len() > config.max_request_size {
            return Err(ProtocolError::InvalidMessage(format!(
                "handshake payload too large: {} > {}",
                payload.len(),
                config.max_request_size
            )));
        }

        let handshake_request: HandshakeRequest = serde_json::from_slice(&payload)?;
        let client_id = handshake_request.client_id.clone();

        if let Some(ref ws) = webhook_service {
            let _ = ws
                .notify_request_received(&client_id, handler.service_name(), "handshake")
                .await;
        }

        if !rate_limiter.check(&client_id) {
            let error_response = serde_json::json!({
                "error": "rate limit exceeded"
            });
            let error_data = serde_json::to_vec(&error_response)?;
            write_message(&mut writer, MSG_ERROR, &error_data).await?;
            return Err(ProtocolError::ResourceExhausted(
                "rate limit exceeded".into(),
            ));
        }

        let response = handler.on_handshake(handshake_request).await;
        let data = serde_json::to_vec(&response)?;
        write_message(&mut writer, MSG_HANDSHAKE_ACK, &data).await?;

        loop {
            let msg_result =
                tokio::time::timeout(config.request_timeout, read_message(&mut reader)).await;

            let (msg_type, payload) = match msg_result {
                Ok(Ok((t, p))) => (t, p),
                Ok(Err(e)) => {
                    tracing::warn!("Read error: {}", e);
                    break;
                }
                Err(_) => {
                    tracing::warn!("Request timeout");
                    break;
                }
            };

            if payload.len() > config.max_request_size {
                let error_response = serde_json::json!({
                    "error": format!("payload too large: {} > {}", payload.len(), config.max_request_size)
                });
                let error_data = serde_json::to_vec(&error_response)?;
                write_message(&mut writer, MSG_ERROR, &error_data).await?;
                continue;
            }

            match msg_type {
                MSG_REQUEST => {
                    let req_metrics = metrics
                        .as_ref()
                        .map(|m| m.get_or_create_service(handler.service_name()));
                    if let Some(ref m) = req_metrics {
                        m.request_start();
                    }

                    let req_start = Instant::now();

                    if let Some(ref ws) = webhook_service {
                        let _ = ws
                            .notify_request_received(
                                &client_id,
                                handler.service_name(),
                                &msg_type.to_string(),
                            )
                            .await;
                    }

                    let request: ServiceRequest = match serde_json::from_slice(&payload) {
                        Ok(r) => r,
                        Err(e) => {
                            if let Some(ref m) = req_metrics {
                                m.request_end(req_start.elapsed(), false, false);
                            }
                            let error_response = serde_json::json!({
                                "error": format!("invalid request: {}", e)
                            });
                            let error_data = serde_json::to_vec(&error_response)?;
                            write_message(&mut writer, MSG_ERROR, &error_data).await?;
                            continue;
                        }
                    };

                    if !Self::validate_request(&request) {
                        if let Some(ref m) = req_metrics {
                            m.request_end(req_start.elapsed(), false, false);
                        }
                        let error_response = serde_json::json!({
                            "error": "invalid request format"
                        });
                        let error_data = serde_json::to_vec(&error_response)?;
                        write_message(&mut writer, MSG_ERROR, &error_data).await?;
                        continue;
                    }

                    let response = handler.handle(request.clone()).await;
                    let duration = req_start.elapsed();
                    let data = serde_json::to_vec(&response)?;
                    write_message(&mut writer, MSG_RESPONSE, &data).await?;

                    if let Some(ref m) = req_metrics {
                        m.request_end(duration, response.status == "success", false);
                    }

                    if let Some(ref ws) = webhook_service {
                        let _ = ws
                            .notify_response_sent(
                                &response.request_id,
                                handler.service_name(),
                                &response.status,
                                duration.as_millis() as u64,
                            )
                            .await;
                    }
                }
                MSG_STREAM_REQUEST => {
                    if config.enable_streaming {
                        let request: ServiceRequest = serde_json::from_slice(&payload)?;
                        let stream_mode = request
                            .payload
                            .get("stream_mode")
                            .and_then(|m| m.as_str())
                            .and_then(StreamMode::from_str)
                            .unwrap_or(StreamMode::Result);

                        let start_msg = serde_json::json!({
                            "request_id": request.request_id,
                            "mode": stream_mode.as_str(),
                            "status": "streaming"
                        });
                        let start_data = serde_json::to_vec(&start_msg)?;
                        write_message(&mut writer, MSG_STREAM_START, &start_data).await?;

                        let response = handler.handle(request.clone()).await;

                        let end_msg = serde_json::json!({
                            "request_id": response.request_id,
                            "status": response.status,
                            "payload": response.payload
                        });
                        let end_data = serde_json::to_vec(&end_msg)?;
                        write_message(&mut writer, MSG_STREAM_END, &end_data).await?;
                    }
                }
                MSG_PING => {
                    write_message(&mut writer, MSG_PONG, &[]).await?;
                }
                MSG_SCHEMA_REQUEST => {
                    if config.enable_schema_introspection {
                        let schema = ServiceSchema::new(handler.service_name(), "1.0");
                        let schema_data = serde_json::to_vec(&schema)?;
                        write_message(&mut writer, MSG_SCHEMA_RESPONSE, &schema_data).await?;
                    }
                }
                MSG_CLOSE => break,
                _ => {}
            }
        }

        let total_duration = start_time.elapsed().as_millis() as u64;
        tracing::debug!("Connection closed, total duration: {}ms", total_duration);
        Ok(())
    }

    fn validate_request(request: &ServiceRequest) -> bool {
        if request.service_name.is_empty() || request.service_name.len() > 256 {
            return false;
        }
        if request.method.is_empty() || request.method.len() > 256 {
            return false;
        }
        if request.request_id.is_empty() || request.request_id.len() > 128 {
            return false;
        }
        true
    }

    pub fn active_connections() -> usize {
        ACTIVE_CONNECTIONS.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub request_timeout: Duration,
    pub idle_timeout: Duration,
    pub max_request_size: usize,
    pub max_connections: usize,
    pub webhook_url: Option<String>,
    pub enable_streaming: bool,
    pub enable_schema_introspection: bool,
    pub rate_limit: RateLimitConfig,
}

#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    pub max_requests: usize,
    pub window_secs: u64,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(300),
            max_request_size: 16 * 1024 * 1024,
            max_connections: 10000,
            webhook_url: None,
            enable_streaming: true,
            enable_schema_introspection: true,
            rate_limit: RateLimitConfig {
                max_requests: 1000,
                window_secs: 60,
            },
        }
    }
}

impl ServerConfig {
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    pub fn with_webhook(mut self, url: &str) -> Self {
        self.webhook_url = Some(url.to_string());
        self
    }

    pub fn with_streaming(mut self, enabled: bool) -> Self {
        self.enable_streaming = enabled;
        self
    }

    pub fn with_schema_introspection(mut self, enabled: bool) -> Self {
        self.enable_schema_introspection = enabled;
        self
    }

    pub fn with_max_connections(mut self, max: usize) -> Self {
        self.max_connections = max;
        self
    }

    pub fn with_rate_limit(mut self, max_requests: usize, window_secs: u64) -> Self {
        self.rate_limit = RateLimitConfig {
            max_requests,
            window_secs,
        };
        self
    }
}
