use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeRequest {
    pub client_id: String,
    pub service_name: String,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeResponse {
    pub server_id: String,
    pub session_id: String,
    pub success: bool,
    pub encoding: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceRequest {
    pub request_id: String,
    pub correlation_id: String,
    pub service_name: String,
    pub method: String,
    pub payload: serde_json::Value,
    pub topic: Option<String>,
}

impl ServiceRequest {
    pub fn new(service: &str, method: &str, payload: serde_json::Value) -> Self {
        Self {
            request_id: uuid::Uuid::new_v4().to_string(),
            correlation_id: uuid::Uuid::new_v4().to_string(),
            service_name: service.to_string(),
            method: method.to_string(),
            payload,
            topic: None,
        }
    }

    pub fn with_topic(mut self, topic: &str) -> Self {
        self.topic = Some(topic.to_string());
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceResponse {
    pub request_id: String,
    pub correlation_id: String,
    pub status: String,
    pub payload: serde_json::Value,
    pub error: Option<String>,
}

impl ServiceResponse {
    pub fn success(request: &ServiceRequest, payload: serde_json::Value) -> Self {
        Self {
            request_id: request.request_id.clone(),
            correlation_id: request.correlation_id.clone(),
            status: "success".to_string(),
            payload,
            error: None,
        }
    }

    pub fn error(request: &ServiceRequest, err: &str) -> Self {
        Self {
            request_id: request.request_id.clone(),
            correlation_id: request.correlation_id.clone(),
            status: "error".to_string(),
            payload: serde_json::Value::Null,
            error: Some(err.to_string()),
        }
    }
}
