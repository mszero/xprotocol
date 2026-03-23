use crate::protocol::{HandshakeRequest, HandshakeResponse, ServiceRequest, ServiceResponse};
use async_trait::async_trait;
use std::sync::Arc;

#[async_trait]
pub trait ServiceHandler: Send + Sync {
    fn service_name(&self) -> &str;

    fn capabilities(&self) -> Vec<String> {
        vec!["json".to_string()]
    }

    async fn on_handshake(&self, _request: HandshakeRequest) -> HandshakeResponse {
        HandshakeResponse {
            server_id: self.service_name().to_string(),
            session_id: uuid::Uuid::new_v4().to_string(),
            success: true,
            encoding: "json".to_string(),
        }
    }

    async fn handle(&self, request: ServiceRequest) -> ServiceResponse;

    fn health_status(&self) -> HealthStatus {
        HealthStatus::healthy(self.service_name())
    }
}

#[derive(Debug, Clone)]
pub enum HealthStatus {
    Healthy(String),
    Unhealthy(String, String),
}

impl HealthStatus {
    pub fn healthy(service: &str) -> Self {
        Self::Healthy(service.to_string())
    }

    pub fn unhealthy(service: &str, reason: &str) -> Self {
        Self::Unhealthy(service.to_string(), reason.to_string())
    }
}

pub struct FnServiceHandler<F> {
    name: String,
    handler: Arc<F>,
}

impl<F> FnServiceHandler<F> {
    pub fn new(name: &str, handler: F) -> Self
    where
        F: Fn(ServiceRequest) -> ServiceResponse + Send + Sync + 'static,
    {
        Self {
            name: name.to_string(),
            handler: Arc::new(handler),
        }
    }
}

#[async_trait]
impl<F> ServiceHandler for FnServiceHandler<F>
where
    F: Fn(ServiceRequest) -> ServiceResponse + Send + Sync + 'static,
{
    fn service_name(&self) -> &str {
        &self.name
    }

    async fn handle(&self, request: ServiceRequest) -> ServiceResponse {
        (self.handler)(request)
    }
}

pub struct FnAsyncServiceHandler<F> {
    name: String,
    handler: Arc<F>,
}

impl<F> FnAsyncServiceHandler<F> {
    pub fn new(name: &str, handler: F) -> Self
    where
        F: Fn(
                ServiceRequest,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = ServiceResponse> + Send>>
            + Send
            + Sync
            + 'static,
    {
        Self {
            name: name.to_string(),
            handler: Arc::new(handler),
        }
    }
}

#[async_trait]
impl<F> ServiceHandler for FnAsyncServiceHandler<F>
where
    F: Fn(
            ServiceRequest,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ServiceResponse> + Send>>
        + Send
        + Sync
        + 'static,
{
    fn service_name(&self) -> &str {
        &self.name
    }

    async fn handle(&self, request: ServiceRequest) -> ServiceResponse {
        (self.handler)(request).await
    }
}
