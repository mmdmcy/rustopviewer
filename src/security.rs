use rand::{Rng, distr::Alphanumeric, distr::SampleString};
use std::{
    fmt,
    time::{Duration, SystemTime},
};

pub const SESSION_COOKIE_NAME: &str = "rov_session";
pub const MAX_PAIR_ATTEMPTS: u8 = 5;
pub const MAX_INPUTS_PER_SECOND: u16 = 90;
pub const PAIR_CODE_TTL: Duration = Duration::from_secs(2 * 60);
pub const SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(20 * 60);
pub const SESSION_MAX_LIFETIME: Duration = Duration::from_secs(4 * 60 * 60);

#[derive(Debug, Clone)]
pub struct PairCodeSnapshot {
    pub code: String,
    pub expires_in: Duration,
    pub remaining_attempts: u8,
}

#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    pub expires_in: Duration,
    pub idle_expires_in: Duration,
}

#[derive(Debug, Clone)]
pub struct SessionGrant {
    pub session_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingError {
    MissingCode,
    NoActiveCode,
    InvalidCode,
    TooManyAttempts,
    CodeExpired,
}

impl fmt::Display for PairingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PairingError::MissingCode => f.write_str("enter the one-time pairing code"),
            PairingError::NoActiveCode => {
                f.write_str("the Windows app is not offering a pairing code right now")
            }
            PairingError::InvalidCode => f.write_str("the pairing code was not correct"),
            PairingError::TooManyAttempts => {
                f.write_str("too many wrong attempts; generate a new pairing code on Windows")
            }
            PairingError::CodeExpired => {
                f.write_str("the pairing code expired; generate a new one on Windows")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAuthError {
    Missing,
    Invalid,
    Expired,
    RateLimited,
}

pub struct SessionStore {
    pair_code: Option<PairCode>,
    session: Option<RemoteSession>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self {
            pair_code: None,
            session: None,
        }
    }

    pub fn generate_pair_code(&mut self) -> PairCodeSnapshot {
        let now = SystemTime::now();
        let pair_code = PairCode {
            code: generate_pair_code(),
            expires_at: now + PAIR_CODE_TTL,
            remaining_attempts: MAX_PAIR_ATTEMPTS,
        };
        let snapshot = pair_code.snapshot(now);
        self.pair_code = Some(pair_code);
        snapshot
    }

    pub fn clear_pair_code(&mut self) {
        self.pair_code = None;
    }

    pub fn pair_code_snapshot(&mut self) -> Option<PairCodeSnapshot> {
        let now = SystemTime::now();
        let pair_code = self.pair_code.as_ref()?;
        if pair_code.is_expired(now) {
            self.pair_code = None;
            None
        } else {
            Some(pair_code.snapshot(now))
        }
    }

    pub fn clear_session(&mut self) {
        self.session = None;
    }

    pub fn session_snapshot(&mut self) -> Option<SessionSnapshot> {
        let now = SystemTime::now();
        let session = self.session.as_ref()?;
        if session.is_expired(now) {
            self.session = None;
            None
        } else {
            Some(session.snapshot(now))
        }
    }

    pub fn exchange_pair_code(
        &mut self,
        candidate: &str,
        user_agent: Option<String>,
    ) -> Result<SessionGrant, PairingError> {
        let now = SystemTime::now();
        let code = candidate.trim();
        if code.is_empty() {
            return Err(PairingError::MissingCode);
        }

        let pair_code = match self.pair_code.as_mut() {
            Some(pair_code) => pair_code,
            None => return Err(PairingError::NoActiveCode),
        };

        if pair_code.is_expired(now) {
            self.pair_code = None;
            return Err(PairingError::CodeExpired);
        }

        if !constant_time_eq(&pair_code.code, code) {
            pair_code.remaining_attempts = pair_code.remaining_attempts.saturating_sub(1);
            if pair_code.remaining_attempts == 0 {
                self.pair_code = None;
                return Err(PairingError::TooManyAttempts);
            }
            return Err(PairingError::InvalidCode);
        }

        self.pair_code = None;

        let session = RemoteSession {
            id: Alphanumeric.sample_string(&mut rand::rng(), 48),
            expires_at: now + SESSION_MAX_LIFETIME,
            last_seen_at: now,
            input_window_started_at: now,
            input_count_in_window: 0,
            user_agent,
        };
        let session_id = session.id.clone();
        self.session = Some(session);
        Ok(SessionGrant { session_id })
    }

    pub fn authorize_session(
        &mut self,
        session_id: &str,
    ) -> Result<SessionSnapshot, SessionAuthError> {
        let now = SystemTime::now();
        let session = self.session.as_mut().ok_or(SessionAuthError::Missing)?;
        if session.is_expired(now) {
            self.session = None;
            return Err(SessionAuthError::Expired);
        }
        if !constant_time_eq(&session.id, session_id) {
            return Err(SessionAuthError::Invalid);
        }
        session.last_seen_at = now;
        Ok(session.snapshot(now))
    }

    pub fn authorize_input_session(
        &mut self,
        session_id: &str,
    ) -> Result<SessionSnapshot, SessionAuthError> {
        let now = SystemTime::now();
        let session = self.session.as_mut().ok_or(SessionAuthError::Missing)?;
        if session.is_expired(now) {
            self.session = None;
            return Err(SessionAuthError::Expired);
        }
        if !constant_time_eq(&session.id, session_id) {
            return Err(SessionAuthError::Invalid);
        }

        if elapsed_since(session.input_window_started_at, now) >= Duration::from_secs(1) {
            session.input_window_started_at = now;
            session.input_count_in_window = 0;
        }
        if session.input_count_in_window >= MAX_INPUTS_PER_SECOND {
            return Err(SessionAuthError::RateLimited);
        }

        session.input_count_in_window = session.input_count_in_window.saturating_add(1);
        session.last_seen_at = now;
        Ok(session.snapshot(now))
    }

    pub fn current_user_agent(&self) -> Option<&str> {
        self.session.as_ref()?.user_agent.as_deref()
    }
}

#[derive(Clone)]
struct PairCode {
    code: String,
    expires_at: SystemTime,
    remaining_attempts: u8,
}

impl PairCode {
    fn is_expired(&self, now: SystemTime) -> bool {
        now >= self.expires_at
    }

    fn snapshot(&self, now: SystemTime) -> PairCodeSnapshot {
        PairCodeSnapshot {
            code: self.code.clone(),
            expires_in: duration_until(self.expires_at, now),
            remaining_attempts: self.remaining_attempts,
        }
    }
}

struct RemoteSession {
    id: String,
    expires_at: SystemTime,
    last_seen_at: SystemTime,
    input_window_started_at: SystemTime,
    input_count_in_window: u16,
    user_agent: Option<String>,
}

impl RemoteSession {
    fn is_expired(&self, now: SystemTime) -> bool {
        now >= self.expires_at || elapsed_since(self.last_seen_at, now) >= SESSION_IDLE_TIMEOUT
    }

    fn snapshot(&self, now: SystemTime) -> SessionSnapshot {
        let idle_expires_at = self.last_seen_at + SESSION_IDLE_TIMEOUT;
        SessionSnapshot {
            expires_in: duration_until(self.expires_at, now),
            idle_expires_in: duration_until(idle_expires_at, now),
        }
    }
}

fn generate_pair_code() -> String {
    let mut rng = rand::rng();
    (0..8)
        .map(|_| char::from(b'0' + rng.random_range(0..10) as u8))
        .collect()
}

fn constant_time_eq(expected: &str, provided: &str) -> bool {
    let expected_bytes = expected.as_bytes();
    let provided_bytes = provided.as_bytes();
    if expected_bytes.len() != provided_bytes.len() {
        return false;
    }

    let mut diff = 0u8;
    for (left, right) in expected_bytes.iter().zip(provided_bytes.iter()) {
        diff |= left ^ right;
    }
    diff == 0
}

fn duration_until(target: SystemTime, now: SystemTime) -> Duration {
    target.duration_since(now).unwrap_or(Duration::ZERO)
}

fn elapsed_since(start: SystemTime, end: SystemTime) -> Duration {
    end.duration_since(start).unwrap_or(Duration::ZERO)
}
