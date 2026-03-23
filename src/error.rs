use thiserror::Error;

#[derive(Error, Debug)]
pub enum ProtocolError {
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),
    #[error("Connection closed")]
    ConnectionClosed,
    #[error("Request timeout")]
    Timeout,
    #[error("Invalid message: {0}")]
    InvalidMessage(String),
    #[error("Service not found: {0}")]
    NotFound(String),
    #[error("Protocol error: {0}")]
    Protocol(String),
    #[error("Encoding error: {0}")]
    Encoding(String),
    #[error("Resource exhausted: {0}")]
    ResourceExhausted(String),
    #[error("Server error: {0}")]
    Server(String),
}

impl From<std::io::Error> for ProtocolError {
    fn from(e: std::io::Error) -> Self {
        ProtocolError::ConnectionFailed(e.to_string())
    }
}

impl From<serde_json::Error> for ProtocolError {
    fn from(e: serde_json::Error) -> Self {
        ProtocolError::Encoding(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, ProtocolError>;

#[allow(non_upper_case_globals)]
#[deprecated(since = "0.1.0", note = "Use ProtocolError instead")]
pub const protocolError: () = ();
