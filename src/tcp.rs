use crate::error::{ProtocolError, Result};
use crate::handler::ServiceHandler;
use crate::protocol::{HandshakeRequest, HandshakeResponse, ServiceRequest, ServiceResponse};
use crate::wire::{
    read_message, write_message, MSG_CLOSE, MSG_HANDSHAKE, MSG_HANDSHAKE_ACK, MSG_PING, MSG_PONG,
    MSG_REQUEST, MSG_RESPONSE,
};
use std::time::Duration;

pub struct TcpServer {
    addr: String,
}

impl TcpServer {
    pub fn new(addr: &str) -> Self {
        Self {
            addr: addr.to_string(),
        }
    }

    pub async fn run<H>(&self, handler: std::sync::Arc<H>) -> Result<()>
    where
        H: ServiceHandler + 'static,
    {
        let listener = tokio::net::TcpListener::bind(&self.addr).await?;

        tracing::info!("TCP Server listening on {}", self.addr);

        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let h = handler.clone();
                    tokio::spawn(async move {
                        if let Err(e) = Self::handle_connection(stream, h).await {
                            tracing::error!("Connection error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    tracing::error!("Accept error: {}", e);
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        }
    }

    async fn handle_connection<H>(
        stream: tokio::net::TcpStream,
        handler: std::sync::Arc<H>,
    ) -> Result<()>
    where
        H: ServiceHandler,
    {
        let (reader, writer) = stream.into_split();
        let mut reader = tokio::io::BufReader::new(reader);
        let mut writer = tokio::io::BufWriter::new(writer);

        let (msg_type, payload) = read_message(&mut reader).await?;
        if msg_type != MSG_HANDSHAKE {
            return Err(ProtocolError::Protocol("expected handshake".into()));
        }

        let request: HandshakeRequest = serde_json::from_slice(&payload)?;
        let response = handler.on_handshake(request).await;
        let data = serde_json::to_vec(&response)?;
        write_message(&mut writer, MSG_HANDSHAKE_ACK, &data).await?;

        loop {
            match read_message(&mut reader).await {
                Ok((MSG_REQUEST, payload)) => {
                    let request: ServiceRequest = serde_json::from_slice(&payload)?;
                    let response = handler.handle(request).await;
                    let data = serde_json::to_vec(&response)?;
                    write_message(&mut writer, MSG_RESPONSE, &data).await?;
                }
                Ok((MSG_PING, _)) => {
                    write_message(&mut writer, MSG_PONG, &[]).await?;
                }
                Ok((MSG_CLOSE, _)) => break,
                Err(ProtocolError::ConnectionClosed) => break,
                _ => {}
            }
        }

        Ok(())
    }

    pub async fn run_with_shutdown<H>(
        &self,
        handler: std::sync::Arc<H>,
        mut shutdown: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<()>
    where
        H: ServiceHandler + 'static,
    {
        let listener = tokio::net::TcpListener::bind(&self.addr).await?;

        tracing::info!("TCP Server listening on {}", self.addr);

        loop {
            tokio::select! {
                result = listener.accept() => {
                    match result {
                        Ok((stream, _)) => {
                            let h = handler.clone();
                            tokio::spawn(async move {
                                if let Err(e) = Self::handle_connection(stream, h).await {
                                    tracing::error!("Connection error: {}", e);
                                }
                            });
                        }
                        Err(e) => tracing::error!("Accept error: {}", e),
                    }
                }
                _ = &mut shutdown => {
                    tracing::info!("Shutdown signal received");
                    break;
                }
            }
        }

        Ok(())
    }
}

pub struct TcpClient {
    addr: String,
    connected: bool,
    session_id: Option<String>,
    stream: tokio::sync::Mutex<Option<tokio::net::TcpStream>>,
}

impl TcpClient {
    pub fn new(addr: &str) -> Self {
        Self {
            addr: addr.to_string(),
            connected: false,
            session_id: None,
            stream: tokio::sync::Mutex::new(None),
        }
    }

    pub fn is_connected(&self) -> bool {
        self.connected
    }

    pub async fn connect(&mut self) -> Result<()> {
        let stream = tokio::net::TcpStream::connect(&self.addr).await?;

        let (reader, writer) = stream.into_split();
        let mut reader = tokio::io::BufReader::new(reader);
        let mut writer = tokio::io::BufWriter::new(writer);

        let request = HandshakeRequest {
            client_id: "protocol-tcp-client".to_string(),
            service_name: "client".to_string(),
            capabilities: vec!["json".to_string()],
        };

        let data = serde_json::to_vec(&request)?;
        write_message(&mut writer, MSG_HANDSHAKE, &data).await?;

        let (msg_type, payload) = read_message(&mut reader).await?;

        if msg_type != MSG_HANDSHAKE_ACK {
            return Err(ProtocolError::Protocol("invalid handshake response".into()));
        }

        let response: HandshakeResponse = serde_json::from_slice(&payload)?;

        if !response.success {
            return Err(ProtocolError::Protocol("handshake rejected".into()));
        }

        self.session_id = Some(response.session_id);
        self.connected = true;

        let stream = tokio::net::TcpStream::connect(&self.addr).await?;
        *self.stream.lock().await = Some(stream);

        Ok(())
    }

    pub async fn call(
        &mut self,
        service: &str,
        method: &str,
        payload: serde_json::Value,
    ) -> Result<ServiceResponse> {
        if !self.connected {
            self.connect().await?;
        }

        let mut guard = self.stream.lock().await;
        let stream = guard.as_mut().ok_or(ProtocolError::ConnectionClosed)?;

        let (reader, writer) = stream.split();
        let reader = tokio::io::BufReader::new(reader);
        let mut writer = tokio::io::BufWriter::new(writer);

        let request = HandshakeRequest {
            client_id: "protocol-tcp-client".to_string(),
            service_name: service.to_string(),
            capabilities: vec!["json".to_string()],
        };
        let data = serde_json::to_vec(&request)?;
        write_message(&mut writer, MSG_HANDSHAKE, &data).await?;

        let mut reader = reader;
        read_message(&mut reader).await?;

        let svc_request = crate::protocol::ServiceRequest::new(service, method, payload);
        let data = serde_json::to_vec(&svc_request)?;
        write_message(&mut writer, MSG_REQUEST, &data).await?;

        let (msg_type, payload) = read_message(&mut reader).await?;

        if msg_type != MSG_RESPONSE {
            self.connected = false;
            return Err(ProtocolError::Protocol("invalid response type".into()));
        }

        let response: ServiceResponse = serde_json::from_slice(&payload)?;
        Ok(response)
    }

    pub async fn close(&mut self) -> Result<()> {
        self.connected = false;
        self.session_id = None;
        *self.stream.lock().await = None;
        Ok(())
    }
}
