use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub client: ClientConfig,
    pub pool: PoolConfig,
    pub security: SecurityConfig,
    pub logging: LoggingConfig,
    pub monitoring: MonitoringConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            client: ClientConfig::default(),
            pool: PoolConfig::default(),
            security: SecurityConfig::default(),
            logging: LoggingConfig::default(),
            monitoring: MonitoringConfig::default(),
        }
    }
}

impl Config {
    pub fn from_file<P: AsRef<Path>>(path: P) -> std::result::Result<Self, ConfigError> {
        let content = fs::read_to_string(path).map_err(ConfigError::IoError)?;
        Self::from_toml(&content)
    }

    pub fn from_toml(content: &str) -> std::result::Result<Self, ConfigError> {
        toml::from_str(content).map_err(|e| ConfigError::ParseError(e.to_string()))
    }

    pub fn from_yaml(content: &str) -> std::result::Result<Self, ConfigError> {
        serde_yaml::from_str(content).map_err(|e| ConfigError::ParseError(e.to_string()))
    }

    pub fn to_toml(&self) -> std::result::Result<String, ConfigError> {
        toml::to_string_pretty(self).map_err(|e| ConfigError::SerializeError(e.to_string()))
    }

    pub fn to_yaml(&self) -> std::result::Result<String, ConfigError> {
        serde_yaml::to_string(self).map_err(|e| ConfigError::SerializeError(e.to_string()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub path: Option<String>,
    pub addr: Option<String>,
    pub request_timeout_secs: u64,
    pub idle_timeout_secs: u64,
    pub max_request_size: usize,
    pub max_connections: usize,
    pub enable_streaming: bool,
    pub enable_schema_introspection: bool,
    pub webhook_url: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            path: None,
            addr: None,
            request_timeout_secs: 30,
            idle_timeout_secs: 300,
            max_request_size: 16 * 1024 * 1024,
            max_connections: 10000,
            enable_streaming: true,
            enable_schema_introspection: true,
            webhook_url: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientConfig {
    pub connect_timeout_secs: u64,
    pub request_timeout_secs: u64,
    pub idle_timeout_secs: u64,
    pub heartbeat_interval_secs: u64,
    pub max_reconnect_attempts: u32,
    pub reconnect_delay_secs: u64,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            connect_timeout_secs: 5,
            request_timeout_secs: 30,
            idle_timeout_secs: 300,
            heartbeat_interval_secs: 30,
            max_reconnect_attempts: 5,
            reconnect_delay_secs: 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolConfig {
    pub max_concurrent: usize,
    pub max_pool_size: usize,
    pub connection_timeout_secs: u64,
    pub request_timeout_secs: u64,
    pub circuit_breaker_threshold: usize,
    pub circuit_breaker_timeout_secs: u64,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 100,
            max_pool_size: 100,
            connection_timeout_secs: 5,
            request_timeout_secs: 30,
            circuit_breaker_threshold: 5,
            circuit_breaker_timeout_secs: 30,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    pub auth_type: Option<String>,
    pub api_keys: Vec<ApiKeyEntry>,
    pub tokens: Vec<TokenEntry>,
    pub rate_limit: RateLimitConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyEntry {
    pub key: String,
    pub client_id: String,
    pub permissions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenEntry {
    pub token: String,
    pub client_id: String,
    pub permissions: Vec<String>,
    pub ttl_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    pub enabled: bool,
    pub max_requests: usize,
    pub window_secs: u64,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            auth_type: None,
            api_keys: Vec::new(),
            tokens: Vec::new(),
            rate_limit: RateLimitConfig::default(),
        }
    }
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_requests: 1000,
            window_secs: 60,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    pub level: String,
    pub format: String,
    pub output: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            format: "json".to_string(),
            output: "stdout".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitoringConfig {
    pub enabled: bool,
    pub metrics_port: Option<u16>,
    pub alert_rules: Vec<AlertRuleConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertRuleConfig {
    pub name: String,
    pub metric: String,
    pub condition: String,
    pub threshold: f64,
}

impl Default for MonitoringConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            metrics_port: Some(9090),
            alert_rules: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub enum ConfigError {
    IoError(std::io::Error),
    ParseError(String),
    SerializeError(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::IoError(e) => write!(f, "IO error: {}", e),
            ConfigError::ParseError(e) => write!(f, "Parse error: {}", e),
            ConfigError::SerializeError(e) => write!(f, "Serialize error: {}", e),
        }
    }
}

impl std::error::Error for ConfigError {}

pub struct ConfigBuilder {
    config: Config,
}

impl ConfigBuilder {
    pub fn new() -> Self {
        Self {
            config: Config::default(),
        }
    }

    pub fn with_server_config(mut self, server: ServerConfig) -> Self {
        self.config.server = server;
        self
    }

    pub fn with_client_config(mut self, client: ClientConfig) -> Self {
        self.config.client = client;
        self
    }

    pub fn with_pool_config(mut self, pool: PoolConfig) -> Self {
        self.config.pool = pool;
        self
    }

    pub fn with_security_config(mut self, security: SecurityConfig) -> Self {
        self.config.security = security;
        self
    }

    pub fn with_logging_config(mut self, logging: LoggingConfig) -> Self {
        self.config.logging = logging;
        self
    }

    pub fn with_monitoring_config(mut self, monitoring: MonitoringConfig) -> Self {
        self.config.monitoring = monitoring;
        self
    }

    pub fn build(self) -> Config {
        self.config
    }
}

impl Default for ConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}
