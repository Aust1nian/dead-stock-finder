//! The app's one HTTP client.
//!
//! Every request in the app goes through [`HttpClient::send`], which refuses
//! any URL whose scheme, host and port aren't on the allowlist before a socket
//! is opened. Redirects are never followed and proxies are never used, so a
//! response can't move a request to another host. `tools/depcheck.py` fails
//! CI if any other crate in the shipped tree brings its own HTTP client.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use ureq::tls::{Certificate, RootCerts, TlsConfig};

/// Square API version sent with every request. Upgrading it is a deliberate
/// change: a DECISIONS.md entry and a full test run.
pub const SQUARE_VERSION: &str = "2026-09-16";

pub const PRODUCTION_HOST: &str = "connect.squareup.com";
pub const SANDBOX_HOST: &str = "connect.squareupsandbox.com";

const USER_AGENT: &str = concat!("IdleStockFinder/", env!("CARGO_PKG_VERSION"));

/// Where requests go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Environment {
    Production,
    /// Square's Sandbox. Only builds with the `sandbox` feature (development
    /// builds) accept it.
    Sandbox,
    /// The local mock Square server, for tests and `--self-test`. Loopback
    /// only, so nothing leaves the computer.
    Mock(SocketAddr),
}

impl Environment {
    pub fn base_url(&self) -> String {
        match self {
            Environment::Production => format!("https://{PRODUCTION_HOST}"),
            Environment::Sandbox => format!("https://{SANDBOX_HOST}"),
            Environment::Mock(addr) => format!("http://{addr}"),
        }
    }

    /// A short name for logs, the keychain account and the cache.
    pub fn name(&self) -> &'static str {
        match self {
            Environment::Production => "production",
            Environment::Sandbox => "sandbox",
            Environment::Mock(_) => "mock",
        }
    }

    /// Whether this build may talk to this environment at all.
    pub fn permitted(&self) -> Result<(), Error> {
        match self {
            Environment::Production => Ok(()),
            Environment::Sandbox if cfg!(feature = "sandbox") => Ok(()),
            Environment::Sandbox => Err(Error::HostNotAllowed(SANDBOX_HOST.into())),
            Environment::Mock(addr) if addr.ip().is_loopback() => Ok(()),
            Environment::Mock(addr) => Err(Error::HostNotAllowed(addr.to_string())),
        }
    }

    /// The allowlist: is `url` exactly this environment's scheme, host and
    /// port? Anything else is refused.
    pub fn allows(&self, url: &str) -> bool {
        let Ok(parsed) = url::Url::parse(url) else { return false };
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return false;
        }
        let host = parsed.host_str().unwrap_or("");
        match self {
            Environment::Production => {
                parsed.scheme() == "https"
                    && host == PRODUCTION_HOST
                    && parsed.port_or_known_default() == Some(443)
            }
            Environment::Sandbox => {
                cfg!(feature = "sandbox")
                    && parsed.scheme() == "https"
                    && host == SANDBOX_HOST
                    && parsed.port_or_known_default() == Some(443)
            }
            Environment::Mock(addr) => {
                let ip_matches = match parsed.host() {
                    Some(url::Host::Ipv4(ip)) => std::net::IpAddr::V4(ip) == addr.ip(),
                    Some(url::Host::Ipv6(ip)) => std::net::IpAddr::V6(ip) == addr.ip(),
                    _ => false,
                };
                addr.ip().is_loopback()
                    && parsed.scheme() == "http"
                    && ip_matches
                    && parsed.port() == Some(addr.port())
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub category: String,
    pub code: String,
    pub detail: String,
    pub field: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("refused: {0} is not on the host allowlist")]
    HostNotAllowed(String),
    #[error("couldn't reach Square: {0}")]
    Network(String),
    #[error("Square is rate-limiting requests; gave up after retries")]
    RateLimited,
    #[error("Square answered {status}: {}", summary(errors))]
    Api { status: u16, errors: Vec<ApiError> },
    #[error("unexpected response from Square: {0}")]
    Decode(String),
}

fn summary(errors: &[ApiError]) -> String {
    errors.iter().map(|e| format!("{} ({})", e.code, e.detail)).collect::<Vec<_>>().join("; ")
}

impl Error {
    pub fn has_code(&self, code: &str) -> bool {
        matches!(self, Error::Api { errors, .. } if errors.iter().any(|e| e.code == code))
    }

    pub fn status(&self) -> Option<u16> {
        match self {
            Error::Api { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// The access token was refused: expired, revoked or unknown.
    pub fn is_unauthorized(&self) -> bool {
        self.status() == Some(401)
    }

    /// Could not reach Square at all (as opposed to Square answering no).
    pub fn is_network(&self) -> bool {
        matches!(self, Error::Network(_))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Delete,
}

impl Method {
    fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Delete => "DELETE",
        }
    }
}

/// What tests and the allowlist trace see of each request. Never holds the
/// token: only whether one was sent.
#[derive(Clone, Debug)]
pub struct RequestRecord {
    pub method: Method,
    pub url: String,
    pub allowed: bool,
    pub authorized: bool,
    pub body: Option<Value>,
    pub status: Option<u16>,
}

pub type Observer = Arc<dyn Fn(&RequestRecord) + Send + Sync>;

/// Backoff for rate limits, server errors and dropped connections.
#[derive(Clone, Debug)]
pub struct RetryPolicy {
    pub rate_limit_attempts: u32,
    pub server_error_attempts: u32,
    pub network_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            rate_limit_attempts: 8,
            server_error_attempts: 4,
            network_attempts: 4,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(32),
        }
    }
}

impl RetryPolicy {
    /// Millisecond delays, for tests against the mock server.
    pub fn fast() -> Self {
        RetryPolicy {
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(8),
            ..Self::default()
        }
    }

    fn delay(&self, attempt: u32) -> Duration {
        let exp = self.base_delay.saturating_mul(1u32 << attempt.min(16));
        let capped = exp.min(self.max_delay);
        // Up to 25% jitter so parallel checks don't retry in lockstep.
        let jitter = capped.mul_f64(f64::from(crate::random_u32() % 256) / 1024.0);
        capped + jitter
    }
}

pub struct HttpClient {
    agent: ureq::Agent,
    env: Environment,
    retry: RetryPolicy,
    observer: Option<Observer>,
    log: Option<Arc<Mutex<Vec<String>>>>,
}

pub struct Response {
    pub status: u16,
    pub body: Value,
}

impl HttpClient {
    pub fn new(env: Environment) -> Result<Self, Error> {
        env.permitted()?;
        let tls = TlsConfig::builder().root_certs(root_certs()).build();
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .https_only(!matches!(env, Environment::Mock(_)))
            .proxy(None)
            .max_redirects(0)
            .max_redirects_will_error(false)
            .user_agent(USER_AGENT)
            .timeout_connect(Some(Duration::from_secs(20)))
            .timeout_global(Some(Duration::from_secs(120)))
            .max_response_header_size(256 * 1024)
            .tls_config(tls)
            .build();
        Ok(HttpClient {
            agent: ureq::Agent::new_with_config(config),
            env,
            retry: RetryPolicy::default(),
            observer: None,
            log: None,
        })
    }

    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    pub fn with_observer(mut self, observer: Observer) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Collect one line per retry, for the check's log.
    pub fn with_log(mut self, log: Arc<Mutex<Vec<String>>>) -> Self {
        self.log = Some(log);
        self
    }

    pub fn environment(&self) -> &Environment {
        &self.env
    }

    fn note(&self, line: String) {
        if let Some(log) = &self.log
            && let Ok(mut log) = log.lock()
        {
            log.push(line);
        }
    }

    /// Sends one request to Square, retrying rate limits, server errors and
    /// dropped connections with backoff. `path` is the part after the host,
    /// such as `/v2/locations`.
    pub fn send(
        &self,
        method: Method,
        path: &str,
        bearer: Option<&str>,
        body: Option<&Value>,
    ) -> Result<Response, Error> {
        let url = format!("{}{}", self.env.base_url(), path);
        let allowed = path.starts_with('/') && !path.starts_with("//") && self.env.allows(&url);
        if !allowed {
            self.observe(method, &url, false, bearer.is_some(), body, None);
            return Err(Error::HostNotAllowed(url));
        }
        let mut rate_limits = 0;
        let mut server_errors = 0;
        let mut network_errors = 0;
        loop {
            let attempt = self.send_once(method, &url, bearer, body);
            self.observe(
                method,
                &url,
                true,
                bearer.is_some(),
                body,
                attempt.as_ref().ok().map(|r| r.0.status),
            );
            match attempt {
                Ok((response, retry_after)) => match response.status {
                    429 => {
                        if rate_limits + 1 >= self.retry.rate_limit_attempts {
                            return Err(Error::RateLimited);
                        }
                        let wait = retry_after
                            .unwrap_or_else(|| self.retry.delay(rate_limits))
                            .min(Duration::from_secs(120));
                        self.note(format!("rate-limited on {path}; waiting {} ms", wait.as_millis()));
                        std::thread::sleep(wait);
                        rate_limits += 1;
                    }
                    500 | 502 | 503 | 504 => {
                        if server_errors + 1 >= self.retry.server_error_attempts {
                            return Err(api_error(response));
                        }
                        let wait = self.retry.delay(server_errors);
                        self.note(format!("Square returned {} on {path}; retrying", response.status));
                        std::thread::sleep(wait);
                        server_errors += 1;
                    }
                    200..=299 => return Ok(response),
                    _ => return Err(api_error(response)),
                },
                Err(Error::Network(reason)) => {
                    if network_errors + 1 >= self.retry.network_attempts {
                        return Err(Error::Network(reason));
                    }
                    self.note(format!("connection problem on {path}: {reason}; retrying"));
                    std::thread::sleep(self.retry.delay(network_errors));
                    network_errors += 1;
                }
                Err(other) => return Err(other),
            }
        }
    }

    fn send_once(
        &self,
        method: Method,
        url: &str,
        bearer: Option<&str>,
        body: Option<&Value>,
    ) -> Result<(Response, Option<Duration>), Error> {
        let mut request = ureq::http::Request::builder()
            .method(method.as_str())
            .uri(url)
            .header("Square-Version", SQUARE_VERSION)
            .header("Accept", "application/json");
        if let Some(token) = bearer {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        let result = match body {
            Some(json) => {
                let bytes = serde_json::to_vec(json).map_err(|e| Error::Decode(e.to_string()))?;
                let request = request
                    .header("Content-Type", "application/json")
                    .body(bytes)
                    .map_err(|e| Error::Decode(e.to_string()))?;
                self.agent.run(request)
            }
            None => {
                let request = request.body(()).map_err(|e| Error::Decode(e.to_string()))?;
                self.agent.run(request)
            }
        };
        let mut response = result.map_err(network_error)?;
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get("Retry-After")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let text = response
            .body_mut()
            .with_config()
            .limit(256 * 1024 * 1024)
            .read_to_string()
            .map_err(network_error)?;
        let body = if text.trim().is_empty() {
            Value::Object(Default::default())
        } else {
            match serde_json::from_str(&text) {
                Ok(v) => v,
                // A proxy or captive portal page, or a truncated body.
                Err(_) if !(200..300).contains(&status) => Value::Object(Default::default()),
                Err(e) => return Err(Error::Decode(format!("{status}: {e}"))),
            }
        };
        Ok((Response { status, body }, retry_after))
    }

    fn observe(
        &self,
        method: Method,
        url: &str,
        allowed: bool,
        authorized: bool,
        body: Option<&Value>,
        status: Option<u16>,
    ) {
        if let Some(observer) = &self.observer {
            observer(&RequestRecord {
                method,
                url: url.to_string(),
                allowed,
                authorized,
                body: body.cloned(),
                status,
            });
        }
    }
}

fn network_error(e: ureq::Error) -> Error {
    match e {
        ureq::Error::BadUri(u) => Error::HostNotAllowed(u),
        other => Error::Network(other.to_string()),
    }
}

fn api_error(response: Response) -> Error {
    let errors: Vec<ApiError> = response
        .body
        .get("errors")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|e| ApiError {
                    category: str_field(e, "category"),
                    code: str_field(e, "code"),
                    detail: str_field(e, "detail"),
                    field: e.get("field").and_then(Value::as_str).map(str::to_string),
                })
                .collect()
        })
        .unwrap_or_default();
    // OAuth endpoints answer with {"error": ..., "error_description": ...}.
    let errors = if errors.is_empty() {
        match response.body.get("error").and_then(Value::as_str) {
            Some(code) => vec![ApiError {
                category: "AUTHENTICATION_ERROR".into(),
                code: code.to_string(),
                detail: str_field(&response.body, "error_description"),
                field: None,
            }],
            None => vec![ApiError {
                category: "API_ERROR".into(),
                code: format!("HTTP_{}", response.status),
                detail: String::new(),
                field: None,
            }],
        }
    } else {
        errors
    };
    Error::Api { status: response.status, errors }
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

/// The operating system's trusted roots (so antivirus or company TLS
/// inspection that installs its own root keeps working), plus Mozilla's
/// bundled roots as a fallback when the system store can't be read. No
/// online revocation fetches: the system verifier would contact certificate
/// authorities, which are hosts outside the allowlist.
fn root_certs() -> RootCerts {
    let mut certs: Vec<Certificate<'static>> = Vec::new();
    let native = rustls_native_certs::load_native_certs();
    for cert in native.certs {
        certs.push(Certificate::from_der(cert.as_ref()).to_owned());
    }
    if certs.is_empty() {
        return RootCerts::WebPki;
    }
    RootCerts::new_with_certs(&certs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_accepts_only_square() {
        let p = Environment::Production;
        assert!(p.allows("https://connect.squareup.com/v2/locations"));
        assert!(p.allows("https://connect.squareup.com:443/oauth2/token"));
        assert!(!p.allows("http://connect.squareup.com/v2/locations"));
        assert!(!p.allows("https://connect.squareup.com:8443/v2/locations"));
        assert!(!p.allows("https://connect.squareup.com.evil.example/v2"));
        assert!(!p.allows("https://user@connect.squareup.com/v2"));
        assert!(!p.allows("https://squareup.com/v2/locations"));
        assert!(!p.allows("https://connect.squareupsandbox.com/v2/locations"));
        assert!(!p.allows("https://example.com/"));
        assert!(!p.allows("not a url"));
    }

    #[test]
    fn sandbox_only_with_feature() {
        let s = Environment::Sandbox;
        assert_eq!(s.allows("https://connect.squareupsandbox.com/v2/locations"), cfg!(feature = "sandbox"));
        assert_eq!(s.permitted().is_ok(), cfg!(feature = "sandbox"));
        assert!(!s.allows("https://connect.squareup.com/v2/locations"));
    }

    #[test]
    fn mock_is_loopback_only() {
        let m = Environment::Mock("127.0.0.1:4567".parse().unwrap());
        assert!(m.permitted().is_ok());
        assert!(m.allows("http://127.0.0.1:4567/v2/locations"));
        assert!(!m.allows("http://127.0.0.1:4568/v2/locations"));
        assert!(!m.allows("http://localhost:4567/v2/locations"));
        assert!(!m.allows("https://connect.squareup.com/v2/locations"));
        let remote = Environment::Mock("10.0.0.5:80".parse().unwrap());
        assert!(remote.permitted().is_err());
        assert!(HttpClient::new(remote).is_err());
    }

    #[test]
    fn path_must_be_a_path() {
        let client = HttpClient::new(Environment::Mock("127.0.0.1:9".parse().unwrap())).unwrap();
        for path in ["@evil.example/", ".evil.example/", "//evil.example/x"] {
            let err = client.send(Method::Get, path, None, None).err().unwrap();
            assert!(matches!(err, Error::HostNotAllowed(_)), "{path}: {err}");
        }
    }
}
