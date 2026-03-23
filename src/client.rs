use crate::error::{ProtocolError, Result};
use crate::protocol::{HandshakeRequest, HandshakeResponse, ServiceRequest, ServiceResponse};
use crate::schema::ServiceSchema;
use crate::streaming::{StreamMessage, StreamMode, StreamResponse};
use crate::wire::{
    read_message, write_message, MSG_ERROR, MSG_HANDSHAKE, MSG_HANDSHAKE_ACK, MSG_PING,
    MSG_REQUEST, MSG_RESPONSE, MSG_SCHEMA_REQUEST, MSG_SCHEMA_RESPONSE, MSG_STREAM_CHUNK,
    MSG_STREAM_END, MSG_STREAM_REQUEST, MSG_STREAM_START,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct UdsClient {
    path: String,
    connected: parking_lot::RwLock<bool>,
    session_id: parking_lot::RwLock<Option<String>>,
    stream: tokio::sync::Mutex<Option<tokio::net::UnixStream>>,
    connect_timeout: Duration,
    request_timeout: Duration,
}

impl UdsClient {
    pub fn new(path: &str) -> Self {
        Self {
            path: path.to_string(),
            connected: parking_lot::RwLock::new(false),
            session_id: parking_lot::RwLock::new(None),
            stream: tokio::sync::Mutex::new(None),
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(30),
        }
    }

    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    pub fn is_connected(&self) -> bool {
        *self.connected.read()
    }

    pub async fn connect(&mut self) -> Result<()> {
        let stream = tokio::time::timeout(
            self.connect_timeout,
            tokio::net::UnixStream::connect(&self.path),
        )
        .await
        .map_err(|_| ProtocolError::Timeout)??;

        let (reader, writer) = stream.into_split();
        let mut reader = tokio::io::BufReader::new(reader);
        let mut writer = tokio::io::BufWriter::new(writer);

        let request = HandshakeRequest {
            client_id: "protocol-client".to_string(),
            service_name: "client".to_string(),
            capabilities: vec!["json".to_string()],
        };

        let data = serde_json::to_vec(&request)?;
        write_message(&mut writer, MSG_HANDSHAKE, &data).await?;

        let (msg_type, payload) =
            tokio::time::timeout(self.request_timeout, read_message(&mut reader))
                .await
                .map_err(|_| ProtocolError::Timeout)??;

        if msg_type != MSG_HANDSHAKE_ACK {
            return Err(ProtocolError::Protocol("invalid handshake response".into()));
        }

        let response: HandshakeResponse = serde_json::from_slice(&payload)?;

        if !response.success {
            return Err(ProtocolError::Protocol("handshake rejected".into()));
        }

        *self.session_id.write() = Some(response.session_id);
        self.set_connected();

        let reader = reader.into_inner();
        let writer = writer.into_inner();

        let stream = reader
            .reunite(writer)
            .map_err(|_| ProtocolError::ConnectionFailed("Failed to reunite stream".into()))?;

        *self.stream.lock().await = Some(stream);

        Ok(())
    }

    fn set_disconnected(&self) {
        *self.connected.write() = false;
    }

    fn set_connected(&self) {
        *self.connected.write() = true;
    }

    pub async fn call(
        &mut self,
        service: &str,
        method: &str,
        payload: serde_json::Value,
    ) -> Result<ServiceResponse> {
        let needs_connect = {
            let stream_guard = self.stream.lock().await;
            stream_guard.is_none()
        };

        if needs_connect {
            self.connect().await?;
        }

        let result = self.do_call_internal(service, method, payload).await;

        if result.is_err() {
            self.set_disconnected();
            *self.stream.lock().await = None;
        }

        result
    }

    async fn do_call_internal(
        &self,
        service: &str,
        method: &str,
        payload: serde_json::Value,
    ) -> Result<ServiceResponse> {
        let mut guard = self.stream.lock().await;
        let stream = guard.as_mut().ok_or(ProtocolError::ConnectionClosed)?;

        let (reader, writer) = stream.split();
        let mut reader = tokio::io::BufReader::new(reader);
        let mut writer = tokio::io::BufWriter::new(writer);

        let svc_request = ServiceRequest::new(service, method, payload);
        let data = serde_json::to_vec(&svc_request)?;
        write_message(&mut writer, MSG_REQUEST, &data).await?;

        let (msg_type, payload) =
            tokio::time::timeout(self.request_timeout, read_message(&mut reader))
                .await
                .map_err(|_| ProtocolError::Timeout)??;

        if msg_type != MSG_RESPONSE {
            return Err(ProtocolError::Protocol("invalid response type".into()));
        }

        let response: ServiceResponse = serde_json::from_slice(&payload)?;
        Ok(response)
    }

    pub async fn call_streaming(
        &mut self,
        service: &str,
        method: &str,
        payload: serde_json::Value,
        mode: StreamMode,
    ) -> Result<Vec<StreamMessage>> {
        let needs_connect = {
            let stream_guard = self.stream.lock().await;
            stream_guard.is_none()
        };

        if needs_connect {
            self.connect().await?;
        }

        let result = self
            .do_call_streaming_internal(service, method, payload, mode)
            .await;

        if result.is_err() {
            self.set_disconnected();
            *self.stream.lock().await = None;
        }

        result
    }

    async fn do_call_streaming_internal(
        &self,
        service: &str,
        method: &str,
        payload: serde_json::Value,
        mode: StreamMode,
    ) -> Result<Vec<StreamMessage>> {
        let mut guard = self.stream.lock().await;
        let stream = guard.as_mut().ok_or(ProtocolError::ConnectionClosed)?;

        let (reader, writer) = stream.split();
        let mut reader = tokio::io::BufReader::new(reader);
        let mut writer = tokio::io::BufWriter::new(writer);

        let mut request_payload = payload;
        request_payload["stream_mode"] = serde_json::Value::String(mode.as_str().to_string());
        let svc_request = ServiceRequest::new(service, method, request_payload);
        let data = serde_json::to_vec(&svc_request)?;
        write_message(&mut writer, MSG_STREAM_REQUEST, &data).await?;

        let mut messages = Vec::new();

        loop {
            let (msg_type, payload) =
                tokio::time::timeout(self.request_timeout, read_message(&mut reader))
                    .await
                    .map_err(|_| ProtocolError::Timeout)??;

            match msg_type {
                MSG_STREAM_START => {}
                MSG_STREAM_CHUNK => {
                    let msg: StreamMessage = serde_json::from_slice(&payload)?;
                    messages.push(msg);
                }
                MSG_STREAM_END => {
                    let end: StreamResponse = serde_json::from_slice(&payload)?;
                    if end.status.starts_with("error") {
                        return Err(ProtocolError::Protocol(end.status));
                    }
                    break;
                }
                MSG_ERROR => {
                    let err: serde_json::Value = serde_json::from_slice(&payload)?;
                    return Err(ProtocolError::Protocol(err.to_string()));
                }
                _ => {
                    return Err(ProtocolError::Protocol(format!(
                        "unexpected message type: {}",
                        msg_type
                    )));
                }
            }
        }

        Ok(messages)
    }

    pub async fn get_schema(&mut self) -> Result<ServiceSchema> {
        let needs_connect = {
            let stream_guard = self.stream.lock().await;
            stream_guard.is_none()
        };

        if needs_connect {
            self.connect().await?;
        }

        let result = self.do_get_schema_internal().await;

        if result.is_err() {
            self.set_disconnected();
            *self.stream.lock().await = None;
        }

        result
    }

    async fn do_get_schema_internal(&self) -> Result<ServiceSchema> {
        let mut guard = self.stream.lock().await;
        let stream = guard.as_mut().ok_or(ProtocolError::ConnectionClosed)?;

        let (reader, writer) = stream.split();
        let mut reader = tokio::io::BufReader::new(reader);
        let mut writer = tokio::io::BufWriter::new(writer);

        write_message(&mut writer, MSG_SCHEMA_REQUEST, &[]).await?;

        let (msg_type, payload) =
            tokio::time::timeout(self.request_timeout, read_message(&mut reader))
                .await
                .map_err(|_| ProtocolError::Timeout)??;

        if msg_type != MSG_SCHEMA_RESPONSE {
            return Err(ProtocolError::Protocol("expected schema response".into()));
        }

        let schema: ServiceSchema = serde_json::from_slice(&payload)?;
        Ok(schema)
    }

    pub async fn close(&mut self) -> Result<()> {
        *self.connected.write() = false;
        *self.session_id.write() = None;
        *self.stream.lock().await = None;
        Ok(())
    }
}

#[derive(Clone)]
pub struct SharedUdsClient {
    inner: Arc<tokio::sync::Mutex<UdsClient>>,
}

impl SharedUdsClient {
    pub fn new(path: &str) -> Self {
        Self {
            inner: Arc::new(tokio::sync::Mutex::new(UdsClient::new(path))),
        }
    }

    pub async fn call(
        &self,
        service: &str,
        method: &str,
        payload: serde_json::Value,
    ) -> Result<ServiceResponse> {
        let mut client = self.inner.lock().await;
        client.call(service, method, payload).await
    }

    pub async fn call_streaming(
        &self,
        service: &str,
        method: &str,
        payload: serde_json::Value,
        mode: StreamMode,
    ) -> Result<Vec<StreamMessage>> {
        let mut client = self.inner.lock().await;
        client.call_streaming(service, method, payload, mode).await
    }

    pub async fn get_schema(&self) -> Result<ServiceSchema> {
        let mut client = self.inner.lock().await;
        client.get_schema().await
    }

    pub async fn close(&self) {
        let mut client = self.inner.lock().await;
        client.close().await.ok();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Reconnecting,
    Failed,
}

impl Default for ConnectionState {
    fn default() -> Self {
        ConnectionState::Disconnected
    }
}

#[derive(Debug, Clone)]
pub struct ConnectionConfig {
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub idle_timeout: Duration,
    pub heartbeat_interval: Duration,
    pub max_reconnect_attempts: u32,
    pub reconnect_delay: Duration,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(300),
            heartbeat_interval: Duration::from_secs(30),
            max_reconnect_attempts: 5,
            reconnect_delay: Duration::from_secs(1),
        }
    }
}

impl ConnectionConfig {
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    pub fn with_idle_timeout(mut self, timeout: Duration) -> Self {
        self.idle_timeout = timeout;
        self
    }

    pub fn with_heartbeat_interval(mut self, interval: Duration) -> Self {
        self.heartbeat_interval = interval;
        self
    }

    pub fn with_max_reconnect_attempts(mut self, attempts: u32) -> Self {
        self.max_reconnect_attempts = attempts;
        self
    }

    pub fn with_reconnect_delay(mut self, delay: Duration) -> Self {
        self.reconnect_delay = delay;
        self
    }

    pub fn production() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(60),
            idle_timeout: Duration::from_secs(600),
            heartbeat_interval: Duration::from_secs(30),
            max_reconnect_attempts: 10,
            reconnect_delay: Duration::from_secs(2),
        }
    }
}

#[derive(Debug, Default)]
pub struct ConnectionStats {
    pub total_requests: AtomicU64,
    pub successful_requests: AtomicU64,
    pub failed_requests: AtomicU64,
    pub reconnection_attempts: AtomicU64,
    pub reconnection_successes: AtomicU64,
    pub last_connected_at: AtomicU64,
    pub last_request_at: AtomicU64,
}

impl ConnectionStats {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_request(&self, success: bool) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        if success {
            self.successful_requests.fetch_add(1, Ordering::Relaxed);
        } else {
            self.failed_requests.fetch_add(1, Ordering::Relaxed);
        }
        self.last_request_at.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            Ordering::Relaxed,
        );
    }

    pub fn record_reconnect(&self, success: bool) {
        self.reconnection_attempts.fetch_add(1, Ordering::Relaxed);
        if success {
            self.reconnection_successes.fetch_add(1, Ordering::Relaxed);
            self.last_connected_at.store(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
                Ordering::Relaxed,
            );
        }
    }

    pub fn success_rate(&self) -> f64 {
        let total = self.total_requests.load(Ordering::Relaxed) as f64;
        if total == 0.0 {
            return 1.0;
        }
        let successful = self.successful_requests.load(Ordering::Relaxed) as f64;
        successful / total
    }

    pub fn reconnection_rate(&self) -> f64 {
        let attempts = self.reconnection_attempts.load(Ordering::Relaxed) as f64;
        if attempts == 0.0 {
            return 1.0;
        }
        let successes = self.reconnection_successes.load(Ordering::Relaxed) as f64;
        successes / attempts
    }
}

pub struct ReliableUdsClient {
    path: String,
    config: ConnectionConfig,
    state: parking_lot::RwLock<ConnectionState>,
    stream: tokio::sync::Mutex<Option<tokio::net::UnixStream>>,
    stats: Arc<ConnectionStats>,
    connected_at: parking_lot::RwLock<Option<Instant>>,
    last_request_at: parking_lot::RwLock<Option<Instant>>,
    lock: tokio::sync::Mutex<()>,
}

impl ReliableUdsClient {
    pub fn new(path: &str) -> Self {
        Self::with_config(path, ConnectionConfig::default())
    }

    pub fn with_config(path: &str, config: ConnectionConfig) -> Self {
        Self {
            path: path.to_string(),
            config,
            state: parking_lot::RwLock::new(ConnectionState::Disconnected),
            stream: tokio::sync::Mutex::new(None),
            stats: Arc::new(ConnectionStats::new()),
            connected_at: parking_lot::RwLock::new(None),
            last_request_at: parking_lot::RwLock::new(None),
            lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn state(&self) -> ConnectionState {
        *self.state.read()
    }

    pub fn stats(&self) -> &ConnectionStats {
        &self.stats
    }

    pub fn is_connected(&self) -> bool {
        *self.state.read() == ConnectionState::Connected
    }

    pub async fn connect(&self) -> std::result::Result<(), ProtocolError> {
        {
            let state = self.state.read();
            if *state == ConnectionState::Connecting {
                tokio::time::sleep(Duration::from_millis(100)).await;
                if *self.state.read() == ConnectionState::Connected {
                    return Ok(());
                }
            }
        }

        let _guard = self.lock.lock().await;

        {
            let state = self.state.read();
            if *state == ConnectionState::Connected {
                return Ok(());
            }
            if *state == ConnectionState::Connecting {
                return Ok(());
            }
        }

        *self.state.write() = ConnectionState::Connecting;

        let result = tokio::time::timeout(
            self.config.connect_timeout,
            tokio::net::UnixStream::connect(&self.path),
        )
        .await;

        match result {
            Ok(Ok(stream)) => {
                *self.stream.lock().await = Some(stream);
                *self.state.write() = ConnectionState::Connected;
                *self.connected_at.write() = Some(Instant::now());
                self.stats.record_reconnect(true);
                tracing::debug!("Connected to {}", self.path);
                Ok(())
            }
            Ok(Err(e)) => {
                *self.state.write() = ConnectionState::Failed;
                self.stats.record_reconnect(false);
                tracing::warn!("Failed to connect to {}: {}", self.path, e);
                Err(ProtocolError::ConnectionFailed(e.to_string()))
            }
            Err(_) => {
                *self.state.write() = ConnectionState::Failed;
                self.stats.record_reconnect(false);
                tracing::warn!("Connection to {} timed out", self.path);
                Err(ProtocolError::Timeout)
            }
        }
    }

    pub async fn disconnect(&self) {
        *self.stream.lock().await = None;
        *self.state.write() = ConnectionState::Disconnected;
        *self.connected_at.write() = None;
        tracing::debug!("Disconnected from {}", self.path);
    }

    pub async fn call(
        &self,
        service: &str,
        method: &str,
        payload: serde_json::Value,
    ) -> Result<ServiceResponse> {
        if !self.is_connected() {
            self.connect().await?;
        }

        let _guard = self.lock.lock().await;

        let mut stream_guard = self.stream.lock().await;
        let stream = match stream_guard.as_mut() {
            Some(s) => s,
            None => {
                *self.state.write() = ConnectionState::Disconnected;
                return Err(ProtocolError::ConnectionClosed);
            }
        };

        let (reader, writer) = stream.split();
        let mut reader = tokio::io::BufReader::new(reader);
        let mut writer = tokio::io::BufWriter::new(writer);

        let request = ServiceRequest::new(service, method, payload);

        let request_data =
            serde_json::to_vec(&request).map_err(|e| ProtocolError::Encoding(e.to_string()))?;

        write_message(&mut writer, MSG_REQUEST, &request_data).await?;

        let (msg_type, response_data) =
            tokio::time::timeout(self.config.request_timeout, read_message(&mut reader))
                .await
                .map_err(|_| {
                    self.stats.record_request(false);
                    ProtocolError::Timeout
                })??;

        if msg_type == MSG_ERROR {
            let error: serde_json::Value = serde_json::from_slice(&response_data)?;
            self.stats.record_request(false);
            *self.stream.lock().await = None;
            *self.state.write() = ConnectionState::Disconnected;
            return Err(ProtocolError::Server(
                error
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("Unknown error")
                    .to_string(),
            ));
        }

        if msg_type != MSG_RESPONSE {
            self.stats.record_request(false);
            *self.stream.lock().await = None;
            *self.state.write() = ConnectionState::Disconnected;
            return Err(ProtocolError::Protocol(format!(
                "Unexpected message type: {}",
                msg_type
            )));
        }

        let response: ServiceResponse = serde_json::from_slice(&response_data)
            .map_err(|e| ProtocolError::Encoding(e.to_string()))?;

        self.stats.record_request(true);
        *self.last_request_at.write() = Some(Instant::now());

        Ok(response)
    }

    pub async fn ping(&self) -> std::result::Result<(), ProtocolError> {
        if !self.is_connected() {
            return Err(ProtocolError::ConnectionClosed);
        }

        let _guard = self.lock.lock().await;

        let mut stream_guard = self.stream.lock().await;
        let stream = match stream_guard.as_mut() {
            Some(s) => s,
            None => return Err(ProtocolError::ConnectionClosed),
        };

        let (_, writer) = stream.split();
        let mut writer = tokio::io::BufWriter::new(writer);

        write_message(&mut writer, MSG_PING, &[]).await?;

        Ok(())
    }

    pub fn needs_reconnect(&self) -> bool {
        if let Some(last_request) = *self.last_request_at.read() {
            if last_request.elapsed() > self.config.idle_timeout {
                return true;
            }
        }
        false
    }

    pub async fn reconnect(&self) -> std::result::Result<(), ProtocolError> {
        tracing::info!("Attempting to reconnect to {}", self.path);

        let _guard = self.lock.lock().await;

        for attempt in 0..self.config.max_reconnect_attempts {
            *self.state.write() = ConnectionState::Reconnecting;
            *self.stream.lock().await = None;

            match tokio::time::timeout(
                self.config.connect_timeout,
                tokio::net::UnixStream::connect(&self.path),
            )
            .await
            {
                Ok(Ok(stream)) => {
                    *self.stream.lock().await = Some(stream);
                    *self.state.write() = ConnectionState::Connected;
                    *self.connected_at.write() = Some(Instant::now());
                    self.stats.record_reconnect(true);
                    tracing::info!(
                        "Reconnected to {} after {} attempts",
                        self.path,
                        attempt + 1
                    );
                    return Ok(());
                }
                Ok(Err(e)) => {
                    tracing::warn!("Reconnection attempt {} failed: {}", attempt + 1, e);
                }
                Err(_) => {
                    tracing::warn!("Reconnection attempt {} timed out", attempt + 1);
                }
            }

            let delay = self.config.reconnect_delay * (attempt + 1) as u32;
            tokio::time::sleep(delay.min(Duration::from_secs(30))).await;
        }

        *self.state.write() = ConnectionState::Failed;
        self.stats.record_reconnect(false);
        Err(ProtocolError::ConnectionFailed(format!(
            "Failed to reconnect after {} attempts",
            self.config.max_reconnect_attempts
        )))
    }

    pub async fn ensure_connected(&self) -> std::result::Result<(), ProtocolError> {
        if !self.is_connected() {
            self.connect().await?;
        }
        Ok(())
    }
}
