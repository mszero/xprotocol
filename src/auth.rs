use crate::error::ProtocolError;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub trait Authenticator: Send + Sync {
    fn authenticate(&self, credentials: &Credentials) -> Result<AuthContext, ProtocolError>;
}

#[derive(Debug, Clone)]
pub struct Credentials {
    pub client_id: String,
    pub token: Option<String>,
    pub api_key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AuthContext {
    pub client_id: String,
    pub permissions: Vec<Permission>,
    pub expires_at: Option<Instant>,
}

impl AuthContext {
    pub fn is_valid(&self) -> bool {
        if let Some(expires) = self.expires_at {
            Instant::now() < expires
        } else {
            true
        }
    }

    pub fn has_permission(&self, permission: &Permission) -> bool {
        self.permissions
            .iter()
            .any(|p| p.service == "*" || p.service == permission.service)
            && self.permissions.iter().any(|p| {
                match (p.method.as_str(), permission.method.as_str()) {
                    ("*", _) => true,
                    (_, "*") => true,
                    (a, b) => a == b,
                }
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Permission {
    pub service: String,
    pub method: String,
}

impl Permission {
    pub fn new(service: &str, method: &str) -> Self {
        Self {
            service: service.to_string(),
            method: method.to_string(),
        }
    }

    pub fn all() -> Self {
        Self {
            service: "*".to_string(),
            method: "*".to_string(),
        }
    }
}

pub struct TokenAuthenticator {
    tokens: Arc<parking_lot::RwLock<HashMap<String, AuthContext>>>,
    token_ttl: Duration,
}

impl TokenAuthenticator {
    pub fn new() -> Self {
        Self {
            tokens: Arc::new(parking_lot::RwLock::new(HashMap::new())),
            token_ttl: Duration::from_secs(3600),
        }
    }

    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.token_ttl = ttl;
        self
    }

    pub fn add_token(&self, token: &str, client_id: &str, permissions: Vec<Permission>) {
        let mut tokens = self.tokens.write();
        tokens.insert(
            token.to_string(),
            AuthContext {
                client_id: client_id.to_string(),
                permissions,
                expires_at: Some(Instant::now() + self.token_ttl),
            },
        );
    }

    pub fn remove_token(&self, token: &str) {
        let mut tokens = self.tokens.write();
        tokens.remove(token);
    }
}

impl Default for TokenAuthenticator {
    fn default() -> Self {
        Self::new()
    }
}

impl Authenticator for TokenAuthenticator {
    fn authenticate(&self, credentials: &Credentials) -> Result<AuthContext, ProtocolError> {
        let token = credentials
            .token
            .as_ref()
            .ok_or_else(|| ProtocolError::Protocol("missing authentication token".into()))?;

        let tokens = self.tokens.read();
        let ctx = tokens
            .get(token)
            .ok_or_else(|| ProtocolError::Protocol("invalid token".into()))?
            .clone();

        if !ctx.is_valid() {
            return Err(ProtocolError::Protocol("token expired".into()));
        }

        Ok(ctx)
    }
}

pub struct ApiKeyAuthenticator {
    keys: Arc<parking_lot::RwLock<HashMap<String, (String, Vec<Permission>)>>>,
}

impl ApiKeyAuthenticator {
    pub fn new() -> Self {
        Self {
            keys: Arc::new(parking_lot::RwLock::new(HashMap::new())),
        }
    }

    pub fn add_key(&self, api_key: &str, client_id: &str, permissions: Vec<Permission>) {
        let mut keys = self.keys.write();
        keys.insert(api_key.to_string(), (client_id.to_string(), permissions));
    }
}

impl Default for ApiKeyAuthenticator {
    fn default() -> Self {
        Self::new()
    }
}

impl Authenticator for ApiKeyAuthenticator {
    fn authenticate(&self, credentials: &Credentials) -> Result<AuthContext, ProtocolError> {
        let api_key = credentials
            .api_key
            .as_ref()
            .ok_or_else(|| ProtocolError::Protocol("missing API key".into()))?;

        let keys = self.keys.read();
        let (client_id, permissions) = keys
            .get(api_key)
            .ok_or_else(|| ProtocolError::Protocol("invalid API key".into()))?
            .clone();

        Ok(AuthContext {
            client_id,
            permissions,
            expires_at: None,
        })
    }
}

pub struct CompositeAuthenticator {
    authenticators: Vec<Box<dyn Authenticator>>,
}

impl CompositeAuthenticator {
    pub fn new() -> Self {
        Self {
            authenticators: Vec::new(),
        }
    }

    pub fn add<T: Authenticator + 'static>(mut self, authenticator: T) -> Self {
        self.authenticators.push(Box::new(authenticator));
        self
    }
}

impl Default for CompositeAuthenticator {
    fn default() -> Self {
        Self::new()
    }
}

impl Authenticator for CompositeAuthenticator {
    fn authenticate(&self, credentials: &Credentials) -> Result<AuthContext, ProtocolError> {
        let mut last_error = None;

        for authenticator in &self.authenticators {
            match authenticator.authenticate(credentials) {
                Ok(ctx) => return Ok(ctx),
                Err(e) => last_error = Some(e),
            }
        }

        Err(last_error.unwrap_or_else(|| ProtocolError::Protocol("authentication failed".into())))
    }
}

pub struct AuthMiddleware {
    authenticator: Arc<dyn Authenticator>,
    required_permissions: HashMap<String, Vec<Permission>>,
}

impl AuthMiddleware {
    pub fn new(authenticator: Arc<dyn Authenticator>) -> Self {
        Self {
            authenticator,
            required_permissions: HashMap::new(),
        }
    }

    pub fn require_permission(mut self, service: &str, permission: Permission) -> Self {
        self.required_permissions
            .entry(service.to_string())
            .or_default()
            .push(permission);
        self
    }

    pub fn check(
        &self,
        credentials: &Credentials,
        service: &str,
        method: &str,
    ) -> Result<AuthContext, ProtocolError> {
        let ctx = self.authenticator.authenticate(credentials)?;

        if !ctx.is_valid() {
            return Err(ProtocolError::Protocol("authentication expired".into()));
        }

        if let Some(required) = self.required_permissions.get(service) {
            let permission = Permission::new(service, method);
            if !required.contains(&permission) && !required.contains(&Permission::all()) {
                return Err(ProtocolError::Protocol("permission denied".into()));
            }
        }

        Ok(ctx)
    }
}

pub struct RateLimiter {
    max_requests: usize,
    window: Duration,
    requests: parking_lot::Mutex<HashMap<String, Vec<Instant>>>,
}

impl RateLimiter {
    pub fn new(max_requests: usize, window: Duration) -> Self {
        Self {
            max_requests,
            window,
            requests: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    pub fn check(&self, client_id: &str) -> bool {
        let now = Instant::now();
        let mut requests = self.requests.lock();

        let client_requests = requests.entry(client_id.to_string()).or_default();
        client_requests.retain(|t| now.duration_since(*t) < self.window);

        if client_requests.len() < self.max_requests {
            client_requests.push(now);
            true
        } else {
            false
        }
    }

    pub fn remaining(&self, client_id: &str) -> usize {
        let now = Instant::now();
        let requests = self.requests.lock();

        if let Some(client_requests) = requests.get(client_id) {
            let valid: Vec<_> = client_requests
                .iter()
                .filter(|t| now.duration_since(**t) < self.window)
                .collect();
            self.max_requests.saturating_sub(valid.len())
        } else {
            self.max_requests
        }
    }

    pub fn reset(&self, client_id: &str) {
        let mut requests = self.requests.lock();
        requests.remove(client_id);
    }
}
