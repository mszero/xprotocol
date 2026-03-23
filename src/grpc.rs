use crate::error::{ProtocolError, Result};
use crate::handler::ServiceHandler;
use crate::protocol::{ServiceRequest, ServiceResponse};
use std::sync::Arc;
use std::time::Duration;

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

        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| ProtocolError::ConnectionFailed(e.to_string()))?;

        tracing::info!("gRPC server listening on {}", addr);

        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let handler = self.handler.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(stream, handler).await {
                            tracing::error!("gRPC connection error: {}", e);
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

async fn handle_connection<H: ServiceHandler>(
    stream: tokio::net::TcpStream,
    handler: Arc<H>,
) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = stream;
    let mut buf = [0u8; 4096];

    loop {
        let result = tokio::time::timeout(Duration::from_secs(30), stream.read(&mut buf)).await;

        let n = match result {
            Ok(Ok(n)) => n,
            Ok(Err(e)) => return Err(ProtocolError::ConnectionFailed(e.to_string())),
            Err(_) => return Err(ProtocolError::Timeout),
        };

        if n == 0 {
            break;
        }

        let data = &buf[..n];

        if data.len() > 5 {
            let _payload_len = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
            let _compression = data[4];
            let payload = &data[5..];

            if let Ok(request) = serde_json::from_slice::<ServiceRequest>(payload) {
                let response = handler.handle(request).await;

                let response_data = serde_json::to_vec(&response)
                    .map_err(|e| ProtocolError::Encoding(e.to_string()))?;

                let mut response_frame = vec![0u8; 5];
                response_frame[0..4].copy_from_slice(&(response_data.len() as u32).to_be_bytes());
                response_frame[4] = 0;
                response_frame.extend(response_data);

                stream
                    .write_all(&response_frame)
                    .await
                    .map_err(|e| ProtocolError::ConnectionFailed(e.to_string()))?;
            }
        }
    }

    Ok(())
}

pub struct GrpcClient {
    addr: String,
}

impl GrpcClient {
    pub fn new(addr: &str) -> Self {
        Self {
            addr: addr.to_string(),
        }
    }

    pub async fn call(&mut self, request: ServiceRequest) -> Result<ServiceResponse> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut stream = tokio::net::TcpStream::connect(&self.addr)
            .await
            .map_err(|e| ProtocolError::ConnectionFailed(e.to_string()))?;

        let request_data =
            serde_json::to_vec(&request).map_err(|e| ProtocolError::Encoding(e.to_string()))?;

        let mut frame = vec![0u8; 5];
        frame[0..4].copy_from_slice(&(request_data.len() as u32).to_be_bytes());
        frame[4] = 0;
        frame.extend(request_data);

        stream
            .write_all(&frame)
            .await
            .map_err(|e| ProtocolError::ConnectionFailed(e.to_string()))?;

        let mut header = [0u8; 5];
        stream
            .read_exact(&mut header)
            .await
            .map_err(|e| ProtocolError::ConnectionFailed(e.to_string()))?;

        let payload_len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;

        let mut payload = vec![0u8; payload_len];
        stream
            .read_exact(&mut payload)
            .await
            .map_err(|e| ProtocolError::ConnectionFailed(e.to_string()))?;

        let response: ServiceResponse =
            serde_json::from_slice(&payload).map_err(|e| ProtocolError::Encoding(e.to_string()))?;

        Ok(response)
    }
}
