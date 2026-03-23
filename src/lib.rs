//! Distributed Service Communication Framework
//!
//! A production-ready framework for service-to-service communication.
//! Built on pingora for reliability and performance.
//!
//! # Architecture
//!
//! - [`handler::ServiceHandler`]: Trait for implementing service logic
//! - [`server::UdsServer`]: Unix Domain Socket server
//! - [`client::UdsClient`]: Unix Domain Socket client
//! - [`pool::UdsConnectionPool`]: pingora-based connection pool
//! - [`discovery::ServiceDiscovery`]: Service registry with pingora

pub mod auth;
pub mod circuit_breaker;
pub mod client;
pub mod config;
pub mod discovery;
pub mod error;
pub mod grpc;
pub mod handler;
pub mod metrics;
pub mod pool;
pub mod protocol;
pub mod runtime;
pub mod schema;
pub mod server;
pub mod session;
pub mod shutdown;
pub mod socket;
pub mod streaming;
pub mod tcp;
pub mod tls;
pub mod utils;
pub mod webhook;
pub mod wire;

pub use auth::{
    ApiKeyAuthenticator, AuthContext, Authenticator, CompositeAuthenticator, Credentials,
    Permission, RateLimiter as AuthRateLimiter, TokenAuthenticator,
};
pub use circuit_breaker::{CircuitBreaker, CircuitState};
pub use client::{
    ConnectionConfig, ConnectionState, ConnectionStats, ReliableUdsClient, SharedUdsClient,
    UdsClient,
};
pub use config::{Config, ConfigBuilder, ConfigError};
pub use discovery::{
    BackgroundHealthChecker, LoadBalancingStrategy, ServiceDiscovery, ServiceEndpoint,
};
pub use error::{ProtocolError, Result};

#[cfg(test)]
mod tests;
pub use handler::{FnAsyncServiceHandler, FnServiceHandler, HealthStatus, ServiceHandler};
pub use metrics::{
    Alert, AlertCallback, AlertCondition, AlertManager, AlertRule, AlertSeverity,
    ConnectionMetrics, ConnectionTracker, Histogram, MetricsCollector, RequestMetrics,
    ServiceMetrics,
};
pub use pool::{
    PoolConfig, PoolMetrics, ProductionPool, TopicBasedPool, UdsClientPool, UdsConnectionPool,
};
pub use protocol::{HandshakeRequest, HandshakeResponse, ServiceRequest, ServiceResponse};
pub use schema::{ServiceCapability, ServiceSchema, ServiceSchemaProvider};
pub use server::{ServerConfig, UdsServer};
pub use session::SessionManager;
pub use shutdown::{run_with_graceful_shutdown, shutdown_signal, GracefulShutdown};
pub use socket::SocketPath;
pub use streaming::{
    DefaultStreamingHandler, StreamMessage, StreamMode, StreamResponse, StreamingHandler,
};
pub use tcp::{TcpClient, TcpServer};
pub use utils::{
    Cache, CancellationToken, DeadLetter, DeadLetterQueue, HealthChecker, MessageQueue, Metrics,
    MetricsSnapshot, RateLimiter, RetryConfig,
};
pub use webhook::{WebhookConfig, WebhookEvent, WebhookRetryPolicy, WebhookSender, WebhookService};

#[cfg(feature = "msgpack")]
pub use wire::{decode_msgpack, encode_msgpack};
pub use wire::{
    read_message, write_message, MSG_CLOSE, MSG_ERROR, MSG_HANDSHAKE, MSG_HANDSHAKE_ACK, MSG_PING,
    MSG_PONG, MSG_REQUEST, MSG_RESPONSE, MSG_SCHEMA_REQUEST, MSG_SCHEMA_RESPONSE, MSG_STREAM_CHUNK,
    MSG_STREAM_END, MSG_STREAM_REQUEST, MSG_STREAM_START,
};
