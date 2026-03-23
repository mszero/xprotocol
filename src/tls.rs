use crate::error::{ProtocolError, Result};

pub struct TlsConfig {
    identity: Option<native_tls::Identity>,
    accept_invalid_certs: bool,
}

impl TlsConfig {
    pub fn new() -> Self {
        Self {
            identity: None,
            accept_invalid_certs: false,
        }
    }

    pub fn with_identity(mut self, identity: native_tls::Identity) -> Self {
        self.identity = Some(identity);
        self
    }

    pub fn accept_invalid_certs(mut self) -> Self {
        self.accept_invalid_certs = true;
        self
    }

    pub fn build(&self) -> Result<native_tls::TlsConnector> {
        let mut builder = native_tls::TlsConnector::builder();

        if self.accept_invalid_certs {
            builder.danger_accept_invalid_certs(true);
        }

        if let Some(identity) = &self.identity {
            builder.identity(identity.clone());
        }

        builder
            .build()
            .map_err(|e| ProtocolError::ConnectionFailed(e.to_string()))
    }
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self::new()
    }
}

pub struct TlsClient {
    connector: tokio_native_tls::TlsConnector,
    addr: String,
}

impl TlsClient {
    pub fn new(config: &TlsConfig, addr: &str) -> Result<Self> {
        let connector = config
            .build()
            .map_err(|e| ProtocolError::ConnectionFailed(e.to_string()))?
            .into();
        Ok(Self {
            connector,
            addr: addr.to_string(),
        })
    }

    pub async fn connect(&self) -> Result<tokio_native_tls::TlsStream<tokio::net::TcpStream>> {
        let stream = tokio::net::TcpStream::connect(&self.addr)
            .await
            .map_err(|e| ProtocolError::ConnectionFailed(e.to_string()))?;
        self.connector
            .connect(&self.addr, stream)
            .await
            .map_err(|e| ProtocolError::ConnectionFailed(e.to_string()))
    }
}
