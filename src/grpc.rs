use crate::error::{ProtocolError, Result};
use crate::handler::ServiceHandler;
use crate::protocol::{ServiceRequest, ServiceResponse};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tonic::Request;
use tokio_stream as _;
use xprotocol::v1::{
    ServiceRequest as GrpcServiceRequest, ServiceResponse as GrpcServiceResponse,
    service_server::{Service, ServiceServer},
};

mod xprotocol {
    pub mod v1 {
        include!("xprotocol.v1.rs");
    }
}

pub const MAX_MESSAGE_SIZE: usize = 4 * 1024 * 1024;
pub const HTTP2_KEEPALIVE_INTERVAL_SECS: u64 = 10;
pub const HTTP2_KEEPALIVE_TIMEOUT_SECS: u64 = 20;
pub const TCP_KEEPALIVE_SECS: u64 = 60;
pub const INITIAL_CONNECTION_WINDOW_SIZE: u32 = 1024 * 1024;
pub const INITIAL_STREAM_WINDOW_SIZE: u32 = 512 * 1024;
pub const MAX_CONCURRENT_STREAMS: u32 = 200;

#[derive(Debug, Clone)]
pub struct GrpcServerConfig {
    pub max_message_size: usize,
    pub http2_keepalive_interval_secs: u64,
    pub http2_keepalive_timeout_secs: u64,
    pub tcp_keepalive_secs: u64,
    pub tcp_nodelay: bool,
    pub initial_connection_window_size: u32,
    pub initial_stream_window_size: u32,
    pub max_concurrent_streams: u32,
}

impl Default for GrpcServerConfig {
    fn default() -> Self {
        Self {
            max_message_size: MAX_MESSAGE_SIZE,
            http2_keepalive_interval_secs: HTTP2_KEEPALIVE_INTERVAL_SECS,
            http2_keepalive_timeout_secs: HTTP2_KEEPALIVE_TIMEOUT_SECS,
            tcp_keepalive_secs: TCP_KEEPALIVE_SECS,
            tcp_nodelay: true,
            initial_connection_window_size: INITIAL_CONNECTION_WINDOW_SIZE,
            initial_stream_window_size: INITIAL_STREAM_WINDOW_SIZE,
            max_concurrent_streams: MAX_CONCURRENT_STREAMS,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GrpcTlsConfig {
    pub enabled: bool,
    pub cert_path: Option<String>,
    pub key_path: Option<String>,
}

impl GrpcTlsConfig {
    pub fn from_env() -> Self {
        let enabled = std::env::var("GRPC_TLS_ENABLED")
            .map(|v| v == "true" || v == "1" || v == "yes")
            .unwrap_or(false);

        let cert_path = std::env::var("GRPC_TLS_CERT_PATH").ok();
        let key_path = std::env::var("GRPC_TLS_KEY_PATH").ok();

        Self {
            enabled,
            cert_path,
            key_path,
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.enabled {
            if self.cert_path.is_none() {
                return Err(ProtocolError::InvalidMessage(
                    "GRPC_TLS_CERT_PATH is required when GRPC_TLS_ENABLED=true".into(),
                ));
            }
            if self.key_path.is_none() {
                return Err(ProtocolError::InvalidMessage(
                    "GRPC_TLS_KEY_PATH is required when GRPC_TLS_ENABLED=true".into(),
                ));
            }
        }
        Ok(())
    }
}

pub struct GrpcService<H: ServiceHandler> {
    handler: Arc<H>,
}

impl<H: ServiceHandler> GrpcService<H> {
    pub fn new(handler: Arc<H>) -> Self {
        Self { handler }
    }

    pub async fn serve(self, addr: &str) -> Result<()>
    where
        H: 'static,
    {
        self.serve_with_config(addr, GrpcServerConfig::default(), GrpcTlsConfig::from_env())
            .await
    }

    pub async fn serve_with_config(
        self,
        addr: &str,
        config: GrpcServerConfig,
        tls_config: GrpcTlsConfig,
    ) -> Result<()>
    where
        H: 'static,
    {
        let addr = addr
            .parse::<SocketAddr>()
            .map_err(|e: std::net::AddrParseError| ProtocolError::InvalidMessage(e.to_string()))?;

        tls_config.validate()?;

        let service = GrpcServiceWrapper::new(self.handler);

        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| ProtocolError::ConnectionFailed(e.to_string()))?;

        tracing::info!("gRPC server listening on {}", addr);

        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let service = service.clone();
                    tokio::spawn(async move {
                        let service = ServiceServer::new(service)
                            .max_decoding_message_size(config.max_message_size)
                            .max_encoding_message_size(config.max_message_size);

                        let server = tonic::transport::Server::builder()
                            .initial_connection_window_size(config.initial_connection_window_size)
                            .initial_stream_window_size(config.initial_stream_window_size)
                            .max_concurrent_streams(config.max_concurrent_streams)
                            .http2_keepalive_interval(Some(Duration::from_secs(
                                config.http2_keepalive_interval_secs,
                            )))
                            .http2_keepalive_timeout(Some(Duration::from_secs(
                                config.http2_keepalive_timeout_secs,
                            )))
                            .tcp_keepalive(Some(Duration::from_secs(config.tcp_keepalive_secs)))
                            .tcp_nodelay(config.tcp_nodelay)
                            .add_service(service);

                        if let Err(e) = server
                            .serve_with_incoming(futures::stream::iter([Ok::<_, std::io::Error>(
                                stream,
                            )]))
                            .await
                        {
                            tracing::error!("gRPC serve error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    tracing::error!("Accept error: {}", e);
                }
            }
        }
    }
}

struct GrpcServiceWrapper<H: ServiceHandler> {
    handler: Arc<H>,
}

impl<H: ServiceHandler> Clone for GrpcServiceWrapper<H> {
    fn clone(&self) -> Self {
        Self {
            handler: self.handler.clone(),
        }
    }
}

impl<H: ServiceHandler> GrpcServiceWrapper<H> {
    fn new(handler: Arc<H>) -> Self {
        Self { handler }
    }
}

fn current_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[tonic::async_trait]
impl<H: ServiceHandler + 'static> Service for GrpcServiceWrapper<H> {
    async fn call(
        &self,
        request: Request<GrpcServiceRequest>,
    ) -> std::result::Result<tonic::Response<GrpcServiceResponse>, tonic::Status> {
        let req = request.into_inner();

        let payload: serde_json::Value = serde_json::from_slice(&req.payload)
            .map_err(|e| tonic::Status::invalid_argument(format!("invalid payload: {}", e)))?;

        let internal_request = ServiceRequest {
            request_id: req.request_id,
            correlation_id: req.correlation_id,
            service_name: req.service_name,
            method: req.method,
            payload,
            topic: req.topic,
        };

        let response = self.handler.handle(internal_request).await;

        let payload_bytes = serde_json::to_vec(&response.payload)
            .map_err(|e| tonic::Status::internal(format!("serialization error: {}", e)))?;

        let grpc_response = GrpcServiceResponse {
            request_id: response.request_id,
            correlation_id: response.correlation_id,
            status: response.status,
            payload: payload_bytes,
            error: response.error,
            error_code: None,
            timestamp: current_timestamp(),
        };

        Ok(tonic::Response::new(grpc_response))
    }

    type CallStreamStream = futures::stream::Empty<std::result::Result<GrpcServiceResponse, tonic::Status>>;

    async fn call_stream(
        &self,
        _request: Request<tonic::Streaming<GrpcServiceRequest>>,
    ) -> std::result::Result<tonic::Response<Self::CallStreamStream>, tonic::Status> {
        Ok(tonic::Response::new(futures::stream::empty()))
    }

    type CallServerStreamStream = futures::stream::Empty<std::result::Result<GrpcServiceResponse, tonic::Status>>;

    async fn call_server_stream(
        &self,
        _request: Request<GrpcServiceRequest>,
    ) -> std::result::Result<tonic::Response<Self::CallServerStreamStream>, tonic::Status> {
        Ok(tonic::Response::new(futures::stream::empty()))
    }
}

pub struct GrpcClient {
    client: xprotocol::v1::service_client::ServiceClient<tonic::transport::Channel>,
}

impl GrpcClient {
    pub async fn connect(addr: &str) -> Result<Self> {
        Self::connect_with_config(addr, GrpcServerConfig::default()).await
    }

    pub async fn connect_with_config(addr: &str, config: GrpcServerConfig) -> Result<Self> {
        let endpoint = format!("http://{}", addr);
        let endpoint = tonic::transport::Endpoint::new(endpoint)?
            .clone()
            .connect_timeout(Duration::from_secs(10))
            .tcp_keepalive(Some(Duration::from_secs(config.tcp_keepalive_secs)))
            .http2_adaptive_window(true)
            .initial_connection_window_size(config.initial_connection_window_size)
            .initial_stream_window_size(config.initial_stream_window_size);

        let channel = endpoint
            .connect()
            .await
            .map_err(|e| ProtocolError::ConnectionFailed(e.to_string()))?;

        let client = xprotocol::v1::service_client::ServiceClient::new(channel)
            .max_decoding_message_size(config.max_message_size)
            .max_encoding_message_size(config.max_message_size);

        Ok(Self { client })
    }

    pub async fn call(&mut self, request: ServiceRequest) -> Result<ServiceResponse> {
        let payload_bytes = serde_json::to_vec(&request.payload)
            .map_err(|e| ProtocolError::Encoding(e.to_string()))?;

        let grpc_request = GrpcServiceRequest {
            request_id: request.request_id,
            correlation_id: request.correlation_id,
            service_name: request.service_name,
            method: request.method,
            payload: payload_bytes,
            topic: request.topic,
            metadata: None,
            timestamp: current_timestamp(),
        };

        let response = self
            .client
            .call(Request::new(grpc_request))
            .await
            .map_err(|e| ProtocolError::ConnectionFailed(e.to_string()))?;

        let resp = response.into_inner();

        let payload: serde_json::Value = serde_json::from_slice(&resp.payload)
            .map_err(|e| ProtocolError::Encoding(e.to_string()))?;

        Ok(ServiceResponse {
            request_id: resp.request_id,
            correlation_id: resp.correlation_id,
            status: resp.status,
            payload,
            error: resp.error,
        })
    }
}
