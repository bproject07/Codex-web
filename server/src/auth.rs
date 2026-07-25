use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    extract::{ConnectInfo, Request, State},
    http::{StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use subtle::ConstantTimeEq;
use url::Url;

use crate::routes::AppState;

const FAILURE_WINDOW: Duration = Duration::from_secs(60);
const BLOCK_DURATION: Duration = Duration::from_secs(60);
const MAX_FAILURES: u32 = 5;
const MAX_TRACKED_ADDRESSES: usize = 1_024;

#[derive(Clone)]
pub struct AuthState {
    inner: Arc<AuthInner>,
}

struct AuthInner {
    token: String,
    failures: Mutex<HashMap<IpAddr, FailureRecord>>,
}

#[derive(Debug, Clone, Copy)]
struct FailureRecord {
    window_started: Instant,
    failures: u32,
    blocked_until: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthDecision {
    Allowed,
    Invalid,
    Blocked,
}

pub fn generate_token() -> anyhow::Result<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| anyhow::anyhow!(error.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

impl AuthState {
    pub fn new(token: String) -> Self {
        Self {
            inner: Arc::new(AuthInner {
                token,
                failures: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn authenticate(&self, address: IpAddr, candidate: Option<&str>) -> AuthDecision {
        let now = Instant::now();
        let mut failures = self
            .inner
            .failures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if let Some(record) = failures.get(&address)
            && record.blocked_until.is_some_and(|until| until > now)
        {
            return AuthDecision::Blocked;
        }

        if candidate.is_some_and(|value| token_matches(&self.inner.token, value)) {
            failures.remove(&address);
            return AuthDecision::Allowed;
        }

        prune_failure_records(&mut failures, now);
        let record = failures.entry(address).or_insert(FailureRecord {
            window_started: now,
            failures: 0,
            blocked_until: None,
        });

        if now.duration_since(record.window_started) > FAILURE_WINDOW {
            *record = FailureRecord {
                window_started: now,
                failures: 0,
                blocked_until: None,
            };
        }

        record.failures = record.failures.saturating_add(1);
        if record.failures >= MAX_FAILURES {
            record.blocked_until = Some(now + BLOCK_DURATION);
            AuthDecision::Blocked
        } else {
            AuthDecision::Invalid
        }
    }
}

fn prune_failure_records(records: &mut HashMap<IpAddr, FailureRecord>, now: Instant) {
    records.retain(|_, record| {
        record.blocked_until.is_some_and(|until| until > now)
            || now.duration_since(record.window_started) <= FAILURE_WINDOW
    });

    if records.len() >= MAX_TRACKED_ADDRESSES
        && let Some(address) = records.keys().next().copied()
    {
        records.remove(&address);
    }
}

pub fn token_matches(expected: &str, candidate: &str) -> bool {
    expected.len() == candidate.len() && bool::from(expected.as_bytes().ct_eq(candidate.as_bytes()))
}

pub fn bearer_token(request: &Request) -> Option<&str> {
    request
        .headers()
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

pub async fn require_http_auth(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    let decision = state.auth.authenticate(peer.ip(), bearer_token(&request));

    match decision {
        AuthDecision::Allowed => next.run(request).await,
        AuthDecision::Invalid => StatusCode::UNAUTHORIZED.into_response(),
        AuthDecision::Blocked => StatusCode::TOO_MANY_REQUESTS.into_response(),
    }
}

pub fn origin_is_allowed(
    origin_header: Option<&str>,
    host_header: Option<&str>,
    bind_host: IpAddr,
) -> bool {
    let Some(origin_header) = origin_header else {
        return false;
    };
    let Ok(origin) = Url::parse(origin_header) else {
        return false;
    };

    if !matches!(origin.scheme(), "http" | "https") {
        return false;
    }

    let Some(origin_host) = origin.host_str() else {
        return false;
    };

    if bind_host.is_loopback() {
        return origin_host.eq_ignore_ascii_case("localhost")
            || origin_host
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback());
    }

    let Some(host_header) = host_header else {
        return false;
    };
    let Ok(host_url) = Url::parse(&format!("http://{host_header}")) else {
        return false;
    };
    let Some(request_host) = host_url.host_str() else {
        return false;
    };

    if !origin_host.eq_ignore_ascii_case(request_host) {
        return false;
    }

    match host_url.port() {
        Some(port) => origin.port_or_known_default() == Some(port),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_tokens_exactly() {
        assert!(token_matches("0123456789abcdef", "0123456789abcdef"));
        assert!(!token_matches("0123456789abcdef", "0123456789abcdeg"));
        assert!(!token_matches("0123456789abcdef", "short"));
    }

    #[test]
    fn generated_tokens_are_strong_and_url_safe() {
        let first = generate_token().expect("generate token");
        let second = generate_token().expect("generate token");

        assert_ne!(first, second);
        assert!(first.len() >= 43);
        assert!(
            first.chars().all(
                |character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            )
        );
    }

    #[test]
    fn rate_limits_repeated_failures() {
        let auth = AuthState::new("0123456789abcdef".to_owned());
        let address = "127.0.0.1".parse().expect("valid IP");

        for _ in 0..(MAX_FAILURES - 1) {
            assert_eq!(
                auth.authenticate(address, Some("wrong-token-value")),
                AuthDecision::Invalid
            );
        }
        assert_eq!(
            auth.authenticate(address, Some("wrong-token-value")),
            AuthDecision::Blocked
        );
    }

    #[test]
    fn allows_loopback_development_origins_on_other_ports() {
        let bind_host = "127.0.0.1".parse().expect("valid IP");
        assert!(origin_is_allowed(
            Some("http://localhost:5173"),
            Some("127.0.0.1:8787"),
            bind_host
        ));
        assert!(!origin_is_allowed(
            Some("https://evil.example"),
            Some("127.0.0.1:8787"),
            bind_host
        ));
    }

    #[test]
    fn requires_remote_origin_to_match_host() {
        let bind_host = "0.0.0.0".parse().expect("valid IP");
        assert!(origin_is_allowed(
            Some("https://terminal.example"),
            Some("terminal.example"),
            bind_host
        ));
        assert!(!origin_is_allowed(
            Some("https://evil.example"),
            Some("terminal.example"),
            bind_host
        ));
    }
}
