use crate::error::ProtocolError;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

pub struct CircuitBreaker {
    failures: AtomicUsize,
    successes: AtomicUsize,
    state: parking_lot::RwLock<CircuitState>,
    threshold: usize,
    timeout: Duration,
    last_failure: parking_lot::RwLock<Option<Instant>>,
}

impl CircuitBreaker {
    pub fn new(threshold: usize) -> Self {
        Self {
            failures: AtomicUsize::new(0),
            successes: AtomicUsize::new(0),
            state: parking_lot::RwLock::new(CircuitState::Closed),
            threshold,
            timeout: Duration::from_secs(30),
            last_failure: parking_lot::RwLock::new(None),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn record_success(&self) {
        self.successes.fetch_add(1, Ordering::Relaxed);
        self.failures.store(0, Ordering::Relaxed);

        let mut state = self.state.write();
        if *state == CircuitState::HalfOpen {
            *state = CircuitState::Closed;
        }
    }

    pub fn record_failure(&self) {
        self.failures.fetch_add(1, Ordering::Relaxed);
        *self.last_failure.write() = Some(Instant::now());

        let failures = self.failures.load(Ordering::Relaxed);
        if failures >= self.threshold {
            *self.state.write() = CircuitState::Open;
        }
    }

    pub fn state(&self) -> CircuitState {
        let state = *self.state.read();

        if state == CircuitState::Open {
            if let Some(last) = *self.last_failure.read() {
                if last.elapsed() > self.timeout {
                    *self.state.write() = CircuitState::HalfOpen;
                    return CircuitState::HalfOpen;
                }
            }
        }

        state
    }

    pub fn is_available(&self) -> bool {
        self.state() != CircuitState::Open
    }

    pub fn can_execute(&self) -> Result<(), ProtocolError> {
        match self.state() {
            CircuitState::Closed | CircuitState::HalfOpen => Ok(()),
            CircuitState::Open => Err(ProtocolError::ConnectionFailed(
                "circuit breaker open".into(),
            )),
        }
    }
}
