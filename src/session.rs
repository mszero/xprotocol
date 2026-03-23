use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub client_id: String,
    pub service_name: String,
    pub created_at: Instant,
    pub last_active: Instant,
    pub metadata: HashMap<String, String>,
}

impl Session {
    pub fn new(id: String, client_id: String, service_name: String) -> Self {
        let now = Instant::now();
        Self {
            id,
            client_id,
            service_name,
            created_at: now,
            last_active: now,
            metadata: HashMap::new(),
        }
    }

    pub fn touch(&mut self) {
        self.last_active = Instant::now();
    }

    pub fn age(&self) -> Duration {
        self.created_at.elapsed()
    }

    pub fn idle_time(&self) -> Duration {
        self.last_active.elapsed()
    }
}

pub struct SessionManager {
    sessions: Arc<RwLock<HashMap<String, Session>>>,
    max_idle_time: Duration,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            max_idle_time: Duration::from_secs(300),
        }
    }

    pub fn with_max_idle_time(mut self, duration: Duration) -> Self {
        self.max_idle_time = duration;
        self
    }

    pub async fn create_session(&self, client_id: String, service_name: String) -> Session {
        let id = uuid::Uuid::new_v4().to_string();
        let session = Session::new(id.clone(), client_id, service_name);
        self.sessions.write().await.insert(id, session.clone());
        session
    }

    pub async fn get_session(&self, id: &str) -> Option<Session> {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get_mut(id) {
            session.touch();
            Some(session.clone())
        } else {
            None
        }
    }

    pub async fn remove_session(&self, id: &str) -> Option<Session> {
        self.sessions.write().await.remove(id)
    }

    pub async fn cleanup_idle(&self) -> usize {
        let mut sessions = self.sessions.write().await;
        let before = sessions.len();
        sessions.retain(|_, s| s.idle_time() < self.max_idle_time);
        before - sessions.len()
    }

    pub async fn session_count(&self) -> usize {
        self.sessions.read().await.len()
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}
