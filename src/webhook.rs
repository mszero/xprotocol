use crate::error::ProtocolError;
use chrono;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event")]
pub enum WebhookEvent {
    RequestReceived {
        request_id: String,
        service_name: String,
        method: String,
        timestamp: String,
    },
    ResponseSent {
        request_id: String,
        service_name: String,
        status: String,
        duration_ms: u64,
        timestamp: String,
    },
    Error {
        request_id: String,
        service_name: String,
        error: String,
        timestamp: String,
    },
    HealthCheck {
        service_name: String,
        status: String,
        timestamp: String,
    },
}

impl WebhookEvent {
    pub fn event_type(&self) -> &'static str {
        match self {
            WebhookEvent::RequestReceived { .. } => "request_received",
            WebhookEvent::ResponseSent { .. } => "response_sent",
            WebhookEvent::Error { .. } => "error",
            WebhookEvent::HealthCheck { .. } => "health_check",
        }
    }

    pub fn timestamp() -> String {
        chrono::Utc::now().to_rfc3339()
    }
}

#[derive(Debug, Clone)]
pub struct WebhookConfig {
    pub url: String,
    pub headers: HashMap<String, String>,
    pub timeout_secs: u64,
    pub retry_policy: WebhookRetryPolicy,
}

impl WebhookConfig {
    pub fn new(url: &str) -> Self {
        Self {
            url: url.to_string(),
            headers: HashMap::new(),
            timeout_secs: 30,
            retry_policy: WebhookRetryPolicy::default(),
        }
    }

    pub fn with_header(mut self, key: &str, value: &str) -> Self {
        self.headers.insert(key.to_string(), value.to_string());
        self
    }

    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }

    pub fn with_retry_policy(mut self, policy: WebhookRetryPolicy) -> Self {
        self.retry_policy = policy;
        self
    }
}

#[derive(Debug, Clone)]
pub struct WebhookRetryPolicy {
    pub max_retries: u32,
    pub initial_delay_ms: u64,
    pub max_delay_ms: u64,
    pub backoff_multiplier: f64,
}

impl Default for WebhookRetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_delay_ms: 100,
            max_delay_ms: 10000,
            backoff_multiplier: 2.0,
        }
    }
}

impl WebhookRetryPolicy {
    pub fn exponential(max_retries: u32) -> Self {
        Self {
            max_retries,
            initial_delay_ms: 100,
            max_delay_ms: 30000,
            backoff_multiplier: 2.0,
        }
    }

    pub fn linear(max_retries: u32, delay_ms: u64) -> Self {
        Self {
            max_retries,
            initial_delay_ms: delay_ms,
            max_delay_ms: delay_ms,
            backoff_multiplier: 1.0,
        }
    }

    pub fn delay_for_retry(&self, attempt: u32) -> Duration {
        let delay =
            (self.initial_delay_ms as f64 * self.backoff_multiplier.powi(attempt as i32)) as u64;
        Duration::from_millis(delay.min(self.max_delay_ms))
    }
}

pub struct WebhookSender {
    client: reqwest::Client,
    config: WebhookConfig,
}

impl WebhookSender {
    pub fn new(config: WebhookConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .expect("Failed to build webhook client");

        Self { client, config }
    }

    pub async fn send(&self, event: WebhookEvent) -> std::result::Result<(), ProtocolError> {
        let payload =
            serde_json::to_vec(&event).map_err(|e| ProtocolError::Encoding(e.to_string()))?;

        let mut attempt = 0;
        let max_attempts = self.config.retry_policy.max_retries + 1;

        loop {
            let mut request = self
                .client
                .post(&self.config.url)
                .header("Content-Type", "application/json")
                .header("X-Webhook-Event", event.event_type());

            for (key, value) in &self.config.headers {
                request = request.header(key, value);
            }

            match request.body(payload.clone()).send().await {
                Ok(response) if response.status().is_success() => {
                    return Ok(());
                }
                Ok(response) => {
                    if attempt >= max_attempts - 1 {
                        return Err(ProtocolError::Server(format!(
                            "Webhook failed after {} attempts: HTTP {}",
                            max_attempts,
                            response.status()
                        )));
                    }
                }
                Err(e) => {
                    if attempt >= max_attempts - 1 {
                        return Err(ProtocolError::ConnectionFailed(format!(
                            "Webhook failed after {} attempts: {}",
                            max_attempts, e
                        )));
                    }
                }
            }

            attempt += 1;
            let delay = self.config.retry_policy.delay_for_retry(attempt);
            tokio::time::sleep(delay).await;
        }
    }
}

pub struct WebhookService {
    senders: Vec<WebhookSender>,
}

impl WebhookService {
    pub fn new() -> Self {
        Self { senders: vec![] }
    }

    pub fn with_webhook(mut self, config: WebhookConfig) -> Self {
        self.senders.push(WebhookSender::new(config));
        self
    }

    pub async fn notify(&self, event: WebhookEvent) -> std::result::Result<(), Vec<ProtocolError>> {
        let mut errors = Vec::new();
        for sender in &self.senders {
            if let Err(e) = sender.send(event.clone()).await {
                tracing::warn!("Webhook notification failed: {}", e);
                errors.push(e);
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    pub async fn notify_request_received(
        &self,
        request_id: &str,
        service_name: &str,
        method: &str,
    ) -> std::result::Result<(), Vec<ProtocolError>> {
        let event = WebhookEvent::RequestReceived {
            request_id: request_id.to_string(),
            service_name: service_name.to_string(),
            method: method.to_string(),
            timestamp: WebhookEvent::timestamp(),
        };
        self.notify(event).await
    }

    pub async fn notify_response_sent(
        &self,
        request_id: &str,
        service_name: &str,
        status: &str,
        duration_ms: u64,
    ) -> std::result::Result<(), Vec<ProtocolError>> {
        let event = WebhookEvent::ResponseSent {
            request_id: request_id.to_string(),
            service_name: service_name.to_string(),
            status: status.to_string(),
            duration_ms,
            timestamp: WebhookEvent::timestamp(),
        };
        self.notify(event).await
    }

    pub async fn notify_error(
        &self,
        request_id: &str,
        service_name: &str,
        error: &str,
    ) -> std::result::Result<(), Vec<ProtocolError>> {
        let event = WebhookEvent::Error {
            request_id: request_id.to_string(),
            service_name: service_name.to_string(),
            error: error.to_string(),
            timestamp: WebhookEvent::timestamp(),
        };
        self.notify(event).await
    }
}

impl Default for WebhookService {
    fn default() -> Self {
        Self::new()
    }
}
