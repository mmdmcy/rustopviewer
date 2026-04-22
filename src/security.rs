use anyhow::{Context, Result};
use rand::{Rng, distr::Alphanumeric, distr::SampleString};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fmt, fs,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const SESSION_COOKIE_NAME: &str = "rov_session";
pub const TRUSTED_BROWSER_COOKIE_NAME: &str = "rov_trusted";
pub const MAX_PAIR_ATTEMPTS: u8 = 5;
pub const MAX_INPUTS_PER_SECOND: u16 = 90;
pub const PAIR_CODE_TTL: Duration = Duration::from_secs(10 * 60);
pub const SESSION_MAX_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);
pub const TRUSTED_BROWSER_MAX_LIFETIME: Duration = Duration::from_secs(5 * 365 * 24 * 60 * 60);

#[derive(Debug, Clone)]
pub struct PairCodeSnapshot {
    pub code: String,
    pub expires_in: Duration,
    pub remaining_attempts: u8,
}

#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    pub expires_in: Duration,
    pub idle_expires_in: Option<Duration>,
    pub bytes_sent: u64,
    pub frame_responses: u64,
    pub cached_frame_hits: u64,
    pub status_responses: u64,
}

#[derive(Debug, Clone)]
pub struct TrustedBrowserSnapshot {
    pub id: String,
    pub label: String,
    pub created_ago: Duration,
    pub last_seen_ago: Duration,
}

#[derive(Debug, Clone)]
pub struct SessionGrant {
    pub session_id: String,
    pub trusted_browser_token: Option<String>,
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
                f.write_str("the host is not offering an access code right now")
            }
            PairingError::InvalidCode => f.write_str("the access code was not correct"),
            PairingError::TooManyAttempts => {
                f.write_str("too many wrong attempts; generate a new access code on the host")
            }
            PairingError::CodeExpired => {
                f.write_str("the access code expired; generate a new one on the host")
            }
        }
    }
}

#[derive(Debug)]
pub enum IssuePairingError {
    Pairing(PairingError),
    Storage,
}

impl From<PairingError> for IssuePairingError {
    fn from(value: PairingError) -> Self {
        Self::Pairing(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAuthError {
    Missing,
    Invalid,
    Expired,
    RateLimited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustedBrowserAuthError {
    Missing,
    Invalid,
    Storage,
}

#[derive(Debug, Clone)]
pub struct TrustedBrowserStore {
    path: PathBuf,
}

impl TrustedBrowserStore {
    pub fn new(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create the trusted browser directory at {}",
                    parent.display()
                )
            })?;
        }

        Ok(Self { path })
    }

    fn load_or_create(&self) -> Result<Vec<TrustedBrowserRecord>> {
        if self.path.exists() {
            let content = fs::read_to_string(&self.path)
                .with_context(|| format!("failed to read {}", self.path.display()))?;
            let persisted: PersistedTrustedBrowsers = serde_json::from_str(&content)
                .with_context(|| format!("failed to parse {}", self.path.display()))?;
            Ok(persisted
                .browsers
                .into_iter()
                .map(TrustedBrowserRecord::from_persisted)
                .collect())
        } else {
            self.save(&[])?;
            Ok(Vec::new())
        }
    }

    fn save(&self, browsers: &[TrustedBrowserRecord]) -> Result<()> {
        let persisted = PersistedTrustedBrowsers {
            browsers: browsers
                .iter()
                .cloned()
                .map(TrustedBrowserRecord::into_persisted)
                .collect(),
        };
        let serialized = serde_json::to_string_pretty(&persisted)
            .context("failed to serialize trusted browser records")?;
        fs::write(&self.path, serialized)
            .with_context(|| format!("failed to write {}", self.path.display()))?;
        Ok(())
    }
}

pub struct SessionStore {
    pair_code: Option<PairCode>,
    session: Option<RemoteSession>,
    trusted_browser_store: TrustedBrowserStore,
    trusted_browsers: Vec<TrustedBrowserRecord>,
}

impl SessionStore {
    pub fn new(trusted_browser_store: TrustedBrowserStore) -> Result<Self> {
        let trusted_browsers = trusted_browser_store.load_or_create()?;
        Ok(Self {
            pair_code: None,
            session: None,
            trusted_browser_store,
            trusted_browsers,
        })
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

    pub fn clear_trusted_browsers(&mut self) -> Result<usize> {
        let count = self.trusted_browsers.len();
        self.trusted_browsers.clear();
        self.session = None;
        self.persist_trusted_browsers()?;
        Ok(count)
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

    pub fn trusted_browser_snapshots(&self) -> Vec<TrustedBrowserSnapshot> {
        let now = SystemTime::now();
        let mut snapshots = self
            .trusted_browsers
            .iter()
            .map(|browser| browser.snapshot(now))
            .collect::<Vec<_>>();
        snapshots.sort_by_key(|browser| browser.last_seen_ago);
        snapshots
    }

    pub fn trusted_browser_count(&self) -> usize {
        self.trusted_browsers.len()
    }

    pub fn issue_pairing_session(
        &mut self,
        candidate: &str,
        user_agent: Option<String>,
        remember_browser: bool,
    ) -> Result<SessionGrant, IssuePairingError> {
        let now = SystemTime::now();
        let code = candidate.trim();
        if code.is_empty() {
            return Err(PairingError::MissingCode.into());
        }

        {
            let pair_code = match self.pair_code.as_mut() {
                Some(pair_code) => pair_code,
                None => return Err(PairingError::NoActiveCode.into()),
            };

            if pair_code.is_expired(now) {
                self.pair_code = None;
                return Err(PairingError::CodeExpired.into());
            }

            if !constant_time_eq(&pair_code.code, code) {
                pair_code.remaining_attempts = pair_code.remaining_attempts.saturating_sub(1);
                if pair_code.remaining_attempts == 0 {
                    self.pair_code = None;
                    return Err(PairingError::TooManyAttempts.into());
                }
                return Err(PairingError::InvalidCode.into());
            }
        }

        let user_agent = normalize_user_agent(user_agent);
        let mut trusted_browser_token = None;
        if remember_browser {
            let (browser, token) = TrustedBrowserRecord::issue(now, user_agent.clone());
            self.trusted_browsers.push(browser);
            if self.persist_trusted_browsers().is_err() {
                self.trusted_browsers.pop();
                return Err(IssuePairingError::Storage);
            }
            trusted_browser_token = Some(token);
        }

        self.pair_code = None;
        let session = RemoteSession::issue(now, user_agent);
        let session_id = session.id.clone();
        self.session = Some(session);
        Ok(SessionGrant {
            session_id,
            trusted_browser_token,
        })
    }

    pub fn restore_trusted_browser_session(
        &mut self,
        trusted_token: &str,
        user_agent: Option<String>,
    ) -> Result<SessionGrant, TrustedBrowserAuthError> {
        let token = trusted_token.trim();
        if token.is_empty() {
            return Err(TrustedBrowserAuthError::Missing);
        }

        let token_hash = hash_trusted_browser_token(token);
        let index = self
            .trusted_browsers
            .iter()
            .position(|browser| browser.matches_token_hash(&token_hash))
            .ok_or(TrustedBrowserAuthError::Invalid)?;

        let now = SystemTime::now();
        let user_agent = normalize_user_agent(user_agent);
        let previous = self.trusted_browsers[index].clone();
        self.trusted_browsers[index].last_seen_at = now;
        if user_agent.is_some() {
            self.trusted_browsers[index].user_agent = user_agent.clone();
        }

        if self.persist_trusted_browsers().is_err() {
            self.trusted_browsers[index] = previous;
            return Err(TrustedBrowserAuthError::Storage);
        }

        let session_user_agent = user_agent.or_else(|| previous.user_agent.clone());
        let session = RemoteSession::issue(now, session_user_agent);
        let session_id = session.id.clone();
        self.session = Some(session);
        Ok(SessionGrant {
            session_id,
            trusted_browser_token: None,
        })
    }

    pub fn current_user_agent(&self) -> Option<&str> {
        self.session.as_ref()?.user_agent.as_deref()
    }

    pub fn record_status_response(
        &mut self,
        session_id: &str,
        bytes_sent: usize,
    ) -> Result<(), SessionAuthError> {
        let session = self.active_session_mut(session_id)?;
        session.status_responses = session.status_responses.saturating_add(1);
        session.bytes_sent = session.bytes_sent.saturating_add(bytes_sent as u64);
        Ok(())
    }

    pub fn record_frame_response(
        &mut self,
        session_id: &str,
        bytes_sent: usize,
        reused_cached_frame: bool,
    ) -> Result<(), SessionAuthError> {
        let session = self.active_session_mut(session_id)?;
        if reused_cached_frame {
            session.cached_frame_hits = session.cached_frame_hits.saturating_add(1);
        } else {
            session.frame_responses = session.frame_responses.saturating_add(1);
            session.bytes_sent = session.bytes_sent.saturating_add(bytes_sent as u64);
        }
        Ok(())
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

    fn active_session_mut(
        &mut self,
        session_id: &str,
    ) -> Result<&mut RemoteSession, SessionAuthError> {
        let now = SystemTime::now();
        let is_expired = self
            .session
            .as_ref()
            .ok_or(SessionAuthError::Missing)?
            .is_expired(now);
        if is_expired {
            self.session = None;
            return Err(SessionAuthError::Expired);
        }
        let session = self.session.as_mut().ok_or(SessionAuthError::Missing)?;
        if !constant_time_eq(&session.id, session_id) {
            return Err(SessionAuthError::Invalid);
        }
        Ok(session)
    }

    fn persist_trusted_browsers(&self) -> Result<()> {
        self.trusted_browser_store.save(&self.trusted_browsers)
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

#[derive(Clone)]
struct TrustedBrowserRecord {
    id: String,
    token_hash: String,
    user_agent: Option<String>,
    created_at: SystemTime,
    last_seen_at: SystemTime,
}

impl TrustedBrowserRecord {
    fn issue(now: SystemTime, user_agent: Option<String>) -> (Self, String) {
        let token = Alphanumeric.sample_string(&mut rand::rng(), 72);
        let token_hash = hash_trusted_browser_token(&token);
        let id = Alphanumeric
            .sample_string(&mut rand::rng(), 12)
            .to_lowercase();
        (
            Self {
                id,
                token_hash,
                user_agent,
                created_at: now,
                last_seen_at: now,
            },
            token,
        )
    }

    fn matches_token_hash(&self, token_hash: &str) -> bool {
        constant_time_eq(&self.token_hash, token_hash)
    }

    fn snapshot(&self, now: SystemTime) -> TrustedBrowserSnapshot {
        TrustedBrowserSnapshot {
            id: self.id.clone(),
            label: trusted_browser_label(&self.id, self.user_agent.as_deref()),
            created_ago: elapsed_since(self.created_at, now),
            last_seen_ago: elapsed_since(self.last_seen_at, now),
        }
    }

    fn into_persisted(self) -> PersistedTrustedBrowser {
        PersistedTrustedBrowser {
            id: self.id,
            token_hash: self.token_hash,
            user_agent: self.user_agent,
            created_at_unix: system_time_to_unix(self.created_at),
            last_seen_at_unix: system_time_to_unix(self.last_seen_at),
        }
    }

    fn from_persisted(persisted: PersistedTrustedBrowser) -> Self {
        Self {
            id: persisted.id,
            token_hash: persisted.token_hash,
            user_agent: normalize_user_agent(persisted.user_agent),
            created_at: unix_to_system_time(persisted.created_at_unix),
            last_seen_at: unix_to_system_time(persisted.last_seen_at_unix),
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
    bytes_sent: u64,
    frame_responses: u64,
    cached_frame_hits: u64,
    status_responses: u64,
}

impl RemoteSession {
    fn issue(now: SystemTime, user_agent: Option<String>) -> Self {
        Self {
            id: Alphanumeric.sample_string(&mut rand::rng(), 48),
            expires_at: now + SESSION_MAX_LIFETIME,
            last_seen_at: now,
            input_window_started_at: now,
            input_count_in_window: 0,
            user_agent,
            bytes_sent: 0,
            frame_responses: 0,
            cached_frame_hits: 0,
            status_responses: 0,
        }
    }

    fn is_expired(&self, now: SystemTime) -> bool {
        now >= self.expires_at
    }

    fn snapshot(&self, now: SystemTime) -> SessionSnapshot {
        SessionSnapshot {
            expires_in: duration_until(self.expires_at, now),
            idle_expires_in: None,
            bytes_sent: self.bytes_sent,
            frame_responses: self.frame_responses,
            cached_frame_hits: self.cached_frame_hits,
            status_responses: self.status_responses,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PersistedTrustedBrowsers {
    browsers: Vec<PersistedTrustedBrowser>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedTrustedBrowser {
    id: String,
    token_hash: String,
    user_agent: Option<String>,
    created_at_unix: u64,
    last_seen_at_unix: u64,
}

fn generate_pair_code() -> String {
    let mut rng = rand::rng();
    (0..8)
        .map(|_| char::from(b'0' + rng.random_range(0..10) as u8))
        .collect()
}

fn trusted_browser_label(id: &str, user_agent: Option<&str>) -> String {
    if let Some(user_agent) = user_agent {
        let trimmed = user_agent.trim();
        if !trimmed.is_empty() {
            return truncate_text(trimmed, 54);
        }
    }

    format!("Browser {}", truncate_text(id, 12))
}

fn truncate_text(value: &str, limit: usize) -> String {
    let char_count = value.chars().count();
    if char_count <= limit {
        return value.to_string();
    }

    let head = value
        .chars()
        .take(limit.saturating_sub(1))
        .collect::<String>();
    format!("{head}…")
}

fn normalize_user_agent(user_agent: Option<String>) -> Option<String> {
    user_agent
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|value| truncate_text(&value, 160))
}

fn hash_trusted_browser_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    format!("{digest:x}")
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

fn system_time_to_unix(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

fn unix_to_system_time(seconds: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(seconds)
}

#[cfg(test)]
mod session_tests {
    use super::{
        MAX_PAIR_ATTEMPTS, RemoteSession, SESSION_MAX_LIFETIME, SessionStore,
        TrustedBrowserAuthError, TrustedBrowserStore,
    };
    use std::{
        fs,
        path::PathBuf,
        time::{Duration, SystemTime},
    };

    fn sample_session(now: SystemTime) -> RemoteSession {
        RemoteSession {
            id: "test-session".to_string(),
            expires_at: now + SESSION_MAX_LIFETIME,
            last_seen_at: now - Duration::from_secs(9 * 60 * 60),
            input_window_started_at: now,
            input_count_in_window: 0,
            user_agent: None,
            bytes_sent: 0,
            frame_responses: 0,
            cached_frame_hits: 0,
            status_responses: 0,
        }
    }

    fn temp_store_path(name: &str) -> PathBuf {
        let unique = format!(
            "rustopviewer-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or(Duration::ZERO)
                .as_nanos()
        );
        std::env::temp_dir()
            .join(unique)
            .join("trusted-browsers.json")
    }

    #[test]
    fn remembered_session_does_not_idle_out() {
        let now = SystemTime::now();
        let session = sample_session(now);
        let later = now + Duration::from_secs(10 * 60 * 60);
        assert!(!session.is_expired(later));
        assert!(session.snapshot(later).idle_expires_in.is_none());
    }

    #[test]
    fn remembered_session_still_expires_at_max_lifetime() {
        let now = SystemTime::now();
        let mut session = sample_session(now);
        session.expires_at = now + Duration::from_secs(5);
        assert!(session.is_expired(now + Duration::from_secs(6)));
    }

    #[test]
    fn trusted_browser_restore_survives_restart() {
        let path = temp_store_path("restore");
        let store = TrustedBrowserStore::new(path.clone()).expect("store should initialize");
        let mut sessions = SessionStore::new(store).expect("session store should initialize");
        let pair = sessions.generate_pair_code().code;
        let grant = sessions
            .issue_pairing_session(&pair, Some("TestBrowser/1.0".to_string()), true)
            .expect("pairing should succeed");
        let trusted_token = grant
            .trusted_browser_token
            .expect("pairing should remember the browser");

        let store = TrustedBrowserStore::new(path.clone()).expect("store should reopen");
        let mut restarted = SessionStore::new(store).expect("session store should reload");
        let restored = restarted
            .restore_trusted_browser_session(&trusted_token, Some("TestBrowser/1.0".to_string()))
            .expect("trusted browser should restore a fresh session");

        assert!(!restored.session_id.is_empty());
        assert_eq!(restarted.trusted_browser_count(), 1);
        let browsers = restarted.trusted_browser_snapshots();
        assert_eq!(browsers.len(), 1);

        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn invalid_trusted_browser_is_rejected() {
        let path = temp_store_path("invalid");
        let store = TrustedBrowserStore::new(path.clone()).expect("store should initialize");
        let mut sessions = SessionStore::new(store).expect("session store should initialize");
        let error = sessions
            .restore_trusted_browser_session("not-a-real-token", None)
            .expect_err("unknown trusted browser tokens must be rejected");
        assert_eq!(error, TrustedBrowserAuthError::Invalid);

        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn successful_pairing_consumes_the_pair_code_even_when_browser_is_remembered() {
        let path = temp_store_path("pair");
        let store = TrustedBrowserStore::new(path.clone()).expect("store should initialize");
        let mut sessions = SessionStore::new(store).expect("session store should initialize");
        let snapshot = sessions.generate_pair_code();
        let pair_code = snapshot.code.clone();
        let grant = sessions
            .issue_pairing_session(&pair_code, Some("TestBrowser/1.0".to_string()), true)
            .expect("pairing should succeed");

        assert!(grant.trusted_browser_token.is_some());
        assert!(sessions.pair_code_snapshot().is_none());
        assert_eq!(sessions.trusted_browser_count(), 1);

        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn wrong_pair_code_still_counts_down_attempts() {
        let path = temp_store_path("attempts");
        let store = TrustedBrowserStore::new(path.clone()).expect("store should initialize");
        let mut sessions = SessionStore::new(store).expect("session store should initialize");
        let _ = sessions.generate_pair_code();

        for expected_remaining in (1..MAX_PAIR_ATTEMPTS).rev() {
            let error = sessions
                .issue_pairing_session("00000000", None, false)
                .expect_err("wrong code should not pair");
            assert!(matches!(
                error,
                super::IssuePairingError::Pairing(super::PairingError::InvalidCode)
            ));
            assert_eq!(
                sessions
                    .pair_code_snapshot()
                    .expect("pair code should still exist before lockout")
                    .remaining_attempts,
                expected_remaining
            );
        }

        let error = sessions
            .issue_pairing_session("00000000", None, false)
            .expect_err("too many wrong attempts should revoke the pair code");
        assert!(matches!(
            error,
            super::IssuePairingError::Pairing(super::PairingError::TooManyAttempts)
        ));
        assert!(sessions.pair_code_snapshot().is_none());

        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }
}
