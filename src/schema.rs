use crate::error::ProtocolError;
use crate::handler::ServiceHandler;
use crate::streaming::StreamMode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceSchema {
    pub service_name: String,
    pub version: String,
    pub input_schema: Value,
    pub output_schema: Value,
    pub state_schema: Option<Value>,
    pub capabilities: Vec<ServiceCapability>,
    pub streaming_modes: Vec<String>,
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceCapability {
    pub name: String,
    pub description: String,
    pub version: Option<String>,
}

impl ServiceSchema {
    pub fn new(service_name: &str, version: &str) -> Self {
        Self {
            service_name: service_name.to_string(),
            version: version.to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "method": { "type": "string" },
                    "payload": { "type": "object" }
                }
            }),
            output_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "status": { "type": "string" },
                    "payload": {}
                }
            }),
            state_schema: None,
            capabilities: vec![],
            streaming_modes: vec!["result".to_string()],
            metadata: serde_json::json!({}),
        }
    }

    pub fn with_input_schema(mut self, schema: Value) -> Self {
        self.input_schema = schema;
        self
    }

    pub fn with_output_schema(mut self, schema: Value) -> Self {
        self.output_schema = schema;
        self
    }

    pub fn with_state_schema(mut self, schema: Value) -> Self {
        self.state_schema = Some(schema);
        self
    }

    pub fn with_capabilities(mut self, caps: Vec<ServiceCapability>) -> Self {
        self.capabilities = caps;
        self
    }

    pub fn with_streaming_modes(mut self, modes: Vec<StreamMode>) -> Self {
        self.streaming_modes = modes.iter().map(|m| m.as_str().to_string()).collect();
        self
    }

    pub fn with_metadata(mut self, metadata: Value) -> Self {
        self.metadata = metadata;
        self
    }

    pub fn to_json(&self) -> std::result::Result<String, ProtocolError> {
        serde_json::to_string_pretty(self).map_err(|e| ProtocolError::Encoding(e.to_string()))
    }
}

pub trait ServiceSchemaProvider: Send + Sync {
    fn get_schema(&self) -> ServiceSchema {
        ServiceSchema::new("unknown", "1.0")
    }
}

impl<T: ServiceHandler + Send + Sync> ServiceSchemaProvider for T {}

impl<T: ServiceHandler + Send + Sync> ServiceSchemaProvider for Arc<T> {
    fn get_schema(&self) -> ServiceSchema {
        ServiceSchema::new(self.service_name(), "1.0")
    }
}
