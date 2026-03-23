use crate::handler::ServiceHandler;
use crate::protocol::{ServiceRequest, ServiceResponse};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StreamMode {
    #[default]
    Messages,
    Values,
    Updates,
    Result,
}

impl StreamMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            StreamMode::Messages => "messages",
            StreamMode::Values => "values",
            StreamMode::Updates => "updates",
            StreamMode::Result => "result",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "messages" => Some(StreamMode::Messages),
            "values" => Some(StreamMode::Values),
            "updates" => Some(StreamMode::Updates),
            "result" => Some(StreamMode::Result),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamMessage {
    pub request_id: String,
    pub chunk_index: u64,
    pub content: serde_json::Value,
    pub metadata: Option<serde_json::Value>,
}

impl StreamMessage {
    pub fn new(request_id: &str, chunk_index: u64, content: serde_json::Value) -> Self {
        Self {
            request_id: request_id.to_string(),
            chunk_index,
            content,
            metadata: None,
        }
    }

    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = Some(metadata);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamResponse {
    pub request_id: String,
    pub mode: String,
    pub status: String,
    pub final_payload: Option<serde_json::Value>,
}

impl StreamResponse {
    pub fn started(request_id: &str, mode: &str) -> Self {
        Self {
            request_id: request_id.to_string(),
            mode: mode.to_string(),
            status: "streaming".to_string(),
            final_payload: None,
        }
    }

    pub fn completed(request_id: &str, payload: serde_json::Value) -> Self {
        Self {
            request_id: request_id.to_string(),
            mode: "result".to_string(),
            status: "completed".to_string(),
            final_payload: Some(payload),
        }
    }

    pub fn error(request_id: &str, error: &str) -> Self {
        Self {
            request_id: request_id.to_string(),
            mode: "result".to_string(),
            status: format!("error: {}", error),
            final_payload: None,
        }
    }
}

#[async_trait]
pub trait StreamingHandler: Send + Sync {
    fn service_name(&self) -> &str;

    fn supported_stream_modes(&self) -> Vec<StreamMode> {
        vec![StreamMode::Result]
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    async fn handle(&self, request: ServiceRequest) -> ServiceResponse;

    async fn handle_streaming(
        &self,
        request: ServiceRequest,
        mode: StreamMode,
    ) -> Vec<StreamMessage> {
        let response = self.handle(request).await;

        match mode {
            StreamMode::Messages => {
                vec![StreamMessage::new(
                    &response.request_id,
                    0,
                    response.payload,
                )]
            }
            StreamMode::Values => {
                vec![
                    StreamMessage::new(
                        &response.request_id,
                        0,
                        serde_json::json!({
                            "status": "processing"
                        }),
                    ),
                    StreamMessage::new(&response.request_id, 1, response.payload.clone()),
                ]
            }
            StreamMode::Updates => {
                vec![StreamMessage::new(
                    &response.request_id,
                    0,
                    serde_json::json!({
                        "type": "update",
                        "data": response.payload
                    }),
                )]
            }
            StreamMode::Result => {
                vec![
                    StreamMessage::new(
                        &response.request_id,
                        0,
                        serde_json::json!({
                            "status": "started"
                        }),
                    ),
                    StreamMessage::new(
                        &response.request_id,
                        1,
                        serde_json::json!({
                            "status": "processing"
                        }),
                    ),
                    StreamMessage::new(
                        &response.request_id,
                        2,
                        serde_json::json!({
                            "status": "completed",
                            "payload": response.payload
                        }),
                    ),
                ]
            }
        }
    }
}

pub struct DefaultStreamingHandler<H: ServiceHandler> {
    handler: H,
}

impl<H: ServiceHandler> DefaultStreamingHandler<H> {
    pub fn new(handler: H) -> Self {
        Self { handler }
    }
}

#[async_trait]
impl<H: ServiceHandler> StreamingHandler for DefaultStreamingHandler<H> {
    fn service_name(&self) -> &str {
        self.handler.service_name()
    }

    fn supported_stream_modes(&self) -> Vec<StreamMode> {
        vec![StreamMode::Result]
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    async fn handle(&self, request: ServiceRequest) -> ServiceResponse {
        self.handler.handle(request).await
    }

    async fn handle_streaming(
        &self,
        request: ServiceRequest,
        mode: StreamMode,
    ) -> Vec<StreamMessage> {
        let response = self.handler.handle(request).await;

        match mode {
            StreamMode::Messages => {
                vec![StreamMessage::new(
                    &response.request_id,
                    0,
                    response.payload,
                )]
            }
            StreamMode::Values => {
                vec![StreamMessage::new(
                    &response.request_id,
                    0,
                    response.payload,
                )]
            }
            StreamMode::Updates => {
                vec![StreamMessage::new(
                    &response.request_id,
                    0,
                    response.payload,
                )]
            }
            StreamMode::Result => {
                vec![StreamMessage::new(
                    &response.request_id,
                    0,
                    serde_json::json!({
                        "status": "completed",
                        "payload": response.payload
                    }),
                )]
            }
        }
    }
}
