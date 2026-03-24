use crate::error::{ProtocolError, Result};
use crate::handler::ServiceHandler;
use crate::protocol::{ServiceRequest, ServiceResponse};
use std::sync::Arc;
use tonic::Request;
use xprotocol::v1::{
    ServiceRequest as GrpcServiceRequest, ServiceResponse as GrpcServiceResponse,
    service_server::{Service, ServiceServer},
};

mod xprotocol {
    pub mod v1 {
        include!("xprotocol.v1.rs");
    }
}

const MAX_MESSAGE_SIZE: usize = 4 * 1024 * 1024;
const HTTP2_KEEPALIVE_INTERVAL_SECS: u64 = 10;
const HTTP2_KEEPALIVE_TIMEOUT_SECS: u64 = 20;
const TCP_KEEPALIVE_SECS: u64 = 60;
const INITIAL_CONNECTION_WINDOW_SIZE: u32 = 1024 * 1024;
const INITIAL_STREAM_WINDOW_SIZE: u32 = 512 * 1024;
const MAX_CONCURRENT_STREAMS: u32 = 200;

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
        let addr = addr
            .parse::<std::net::SocketAddr>()
            .map_err(|e: std::net::AddrParseError| ProtocolError::InvalidMessage(e.to_string()))?;

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
                            .max_decoding_message_size(MAX_MESSAGE_SIZE)
                            .max_encoding_message_size(MAX_MESSAGE_SIZE);
                        if let Err(e) = tonic::transport::Server::builder()
                            .initial_connection_window_size(INITIAL_CONNECTION_WINDOW_SIZE)
                            .initial_stream_window_size(INITIAL_STREAM_WINDOW_SIZE)
                            .max_concurrent_streams(MAX_CONCURRENT_STREAMS)
                            .http2_keepalive_interval(Some(std::time::Duration::from_secs(
                                HTTP2_KEEPALIVE_INTERVAL_SECS,
                            )))
                            .http2_keepalive_timeout(Some(std::time::Duration::from_secs(
                                HTTP2_KEEPALIVE_TIMEOUT_SECS,
                            )))
                            .tcp_keepalive(Some(std::time::Duration::from_secs(TCP_KEEPALIVE_SECS)))
                            .add_service(service)
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

#[tonic::async_trait]
impl<H: ServiceHandler + 'static> Service for GrpcServiceWrapper<H> {
    async fn call(
        &self,
        request: Request<GrpcServiceRequest>,
    ) -> std::result::Result<tonic::Response<GrpcServiceResponse>, tonic::Status> {
        let req = request.into_inner();

        let payload: serde_json::Value = serde_json::from_str(&req.payload)
            .map_err(|e| tonic::Status::invalid_argument(format!("invalid payload: {}", e)))?;

        let internal_request = ServiceRequest {
            request_id: req.request_id,
            correlation_id: req.correlation_id,
            service_name: req.service_name,
            method: req.method,
            payload,
            topic: None,
        };

        let response = self.handler.handle(internal_request).await;

        let payload_str = serde_json::to_string(&response.payload)
            .map_err(|e| tonic::Status::internal(format!("serialization error: {}", e)))?;

        let grpc_response = GrpcServiceResponse {
            request_id: response.request_id,
            correlation_id: response.correlation_id,
            status: response.status,
            payload: payload_str,
            error: response.error,
        };

        Ok(tonic::Response::new(grpc_response))
    }
}

pub struct GrpcClient {
    client: xprotocol::v1::service_client::ServiceClient<tonic::transport::Channel>,
}

impl GrpcClient {
    pub async fn connect(addr: &str) -> Result<Self> {
        let endpoint = format!("http://{}", addr);
        let endpoint = tonic::transport::Endpoint::new(endpoint)?
            .clone()
            .connect_timeout(std::time::Duration::from_secs(10))
            .tcp_keepalive(Some(std::time::Duration::from_secs(TCP_KEEPALIVE_SECS)))
            .http2_adaptive_window(true)
            .initial_connection_window_size(INITIAL_CONNECTION_WINDOW_SIZE)
            .initial_stream_window_size(INITIAL_STREAM_WINDOW_SIZE);

        let channel = endpoint
            .connect()
            .await
            .map_err(|e| ProtocolError::ConnectionFailed(e.to_string()))?;

        let client = xprotocol::v1::service_client::ServiceClient::new(channel)
            .max_decoding_message_size(MAX_MESSAGE_SIZE)
            .max_encoding_message_size(MAX_MESSAGE_SIZE);

        Ok(Self { client })
    }

    pub async fn call(&mut self, request: ServiceRequest) -> Result<ServiceResponse> {
        let payload_str = serde_json::to_string(&request.payload)
            .map_err(|e| ProtocolError::Encoding(e.to_string()))?;

        let grpc_request = GrpcServiceRequest {
            request_id: request.request_id,
            correlation_id: request.correlation_id,
            service_name: request.service_name,
            method: request.method,
            payload: payload_str,
        };

        let response = self
            .client
            .call(Request::new(grpc_request))
            .await
            .map_err(|e| ProtocolError::ConnectionFailed(e.to_string()))?;

        let resp = response.into_inner();

        let payload: serde_json::Value = serde_json::from_str(&resp.payload)
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
