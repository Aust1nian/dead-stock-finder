//! Square OAuth with PKCE, for a desktop app that can't keep a secret.
//!
//! 1. [`Pkce::new`] makes a code verifier, its SHA-256 challenge and a state.
//! 2. [`CallbackListener::bind`] listens on loopback on a free port.
//! 3. The browser opens [`authorize_url`]; the seller logs in on Square's page.
//! 4. [`CallbackListener::wait`] takes the one callback and checks the state.
//! 5. [`exchange_code`] trades the code and verifier for tokens, which the
//!    caller saves to the keychain before doing anything else.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, Ipv6Addr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::http::{Environment, Error, HttpClient, Method};
use crate::token::Tokens;

/// Read everything the app needs; write only when labels are on.
pub const READ_SCOPES: &[&str] = &["MERCHANT_PROFILE_READ", "ITEMS_READ", "ORDERS_READ", "INVENTORY_READ"];
pub const LABEL_SCOPE: &str = "ITEMS_WRITE";

pub fn scopes(labels_on: bool) -> Vec<String> {
    let mut scopes: Vec<String> = READ_SCOPES.iter().map(|s| s.to_string()).collect();
    if labels_on {
        scopes.push(LABEL_SCOPE.to_string());
    }
    scopes
}

pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
    pub state: String,
}

impl Pkce {
    pub fn new() -> Self {
        let verifier = random_token(64);
        let challenge = challenge_for(&verifier);
        Pkce { verifier, challenge, state: random_token(32) }
    }
}

impl Default for Pkce {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Pkce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Pkce { .. }")
    }
}

/// base64url(SHA-256(verifier)), RFC 7636 S256.
pub fn challenge_for(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("the operating system's random source");
    URL_SAFE_NO_PAD.encode(buf)
}

/// Square's authorize page. `session=false` makes the seller log in every
/// time, so they always connect the account they mean to.
pub fn authorize_url(
    env: &Environment,
    app_id: &str,
    scopes: &[String],
    pkce: &Pkce,
    redirect_uri: &str,
) -> String {
    let mut url = url::Url::parse(&format!("{}/oauth2/authorize", env.base_url())).expect("static URL");
    url.query_pairs_mut()
        .append_pair("client_id", app_id)
        .append_pair("scope", &scopes.join(" "))
        .append_pair("session", "false")
        .append_pair("state", &pkce.state)
        // S256 is implied; Square documents no code_challenge_method.
        .append_pair("code_challenge", &pkce.challenge)
        .append_pair("redirect_uri", redirect_uri);
    url.to_string()
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("the Square page was closed or took longer than 5 minutes")]
    TimedOut,
    #[error("connecting was canceled")]
    Canceled,
    #[error("Square didn't connect: {0}")]
    Denied(String),
    #[error("the reply from Square didn't match this request")]
    StateMismatch,
    #[error("couldn't listen for Square's reply: {0}")]
    Listener(String),
    #[error(transparent)]
    Square(#[from] Error),
}

/// The one-request loopback listener for Square's redirect.
pub struct CallbackListener {
    v4: TcpListener,
    v6: Option<TcpListener>,
    port: u16,
}

impl CallbackListener {
    /// Listens on 127.0.0.1 on a free port, and on [::1] on the same port when
    /// it can, because browsers may resolve `localhost` to either.
    pub fn bind() -> Result<Self, ConnectError> {
        let mut last_error = None;
        for _ in 0..10 {
            let v4 = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                .map_err(|e| ConnectError::Listener(e.to_string()))?;
            let port = v4.local_addr().map_err(|e| ConnectError::Listener(e.to_string()))?.port();
            match TcpListener::bind((Ipv6Addr::LOCALHOST, port)) {
                Ok(v6) => return Ok(CallbackListener { v4, v6: Some(v6), port }),
                // No IPv6 loopback on this computer: IPv4 alone is fine.
                Err(e) if e.kind() == std::io::ErrorKind::AddrNotAvailable => {
                    return Ok(CallbackListener { v4, v6: None, port });
                }
                Err(e) => last_error = Some(e),
            }
        }
        Err(ConnectError::Listener(last_error.map(|e| e.to_string()).unwrap_or_default()))
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The redirect URL sent to Square: `http://localhost:<port>`.
    pub fn redirect_uri(&self) -> String {
        format!("http://localhost:{}", self.port)
    }

    /// Waits for Square's redirect and returns the authorization code. Other
    /// requests (a browser's favicon fetch) get a 404 and waiting continues.
    pub fn wait(self, state: &str, timeout: Duration, cancel: &AtomicBool) -> Result<String, ConnectError> {
        let deadline = Instant::now() + timeout;
        let listeners: Vec<&TcpListener> = std::iter::once(&self.v4).chain(self.v6.as_ref()).collect();
        for l in &listeners {
            l.set_nonblocking(true).map_err(|e| ConnectError::Listener(e.to_string()))?;
        }
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(ConnectError::Canceled);
            }
            if Instant::now() >= deadline {
                return Err(ConnectError::TimedOut);
            }
            let mut accepted = None;
            for l in &listeners {
                if let Ok((stream, peer)) = l.accept() {
                    accepted = Some((stream, peer));
                    break;
                }
            }
            let Some((stream, peer)) = accepted else {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            };
            if !peer.ip().is_loopback() {
                continue;
            }
            match handle_callback(stream, state) {
                Some(result) => return result,
                None => continue,
            }
        }
    }
}

/// Reads one HTTP request. Returns `None` for requests that aren't the
/// callback (wrong path), so waiting continues.
fn handle_callback(mut stream: TcpStream, state: &str) -> Option<Result<String, ConnectError>> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok()?;
    // Drain the headers; the callback carries everything in the query.
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) if line == "\r\n" || line == "\n" => break,
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    let target = request_line.split_whitespace().nth(1).unwrap_or("");
    let parsed = url::Url::parse(&format!("http://localhost{target}")).ok()?;
    if parsed.path() != "/" {
        respond(&mut stream, "404 Not Found", "Not found.");
        return None;
    }
    let pairs: Vec<(String, String)> = parsed.query_pairs().into_owned().collect();
    let get = |key: &str| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
    let result = if get("state").as_deref() != Some(state) {
        Err(ConnectError::StateMismatch)
    } else if let Some(code) = get("code") {
        Ok(code)
    } else {
        let reason =
            get("error_description").or_else(|| get("error")).unwrap_or_else(|| "no code returned".into());
        Err(ConnectError::Denied(reason))
    };
    match &result {
        Ok(_) => respond(
            &mut stream,
            "200 OK",
            "Connected. You can close this tab and go back to Idle Stock Finder.",
        ),
        Err(_) => respond(
            &mut stream,
            "200 OK",
            "Square didn't connect. Close this tab and try again in Idle Stock Finder.",
        ),
    }
    Some(result)
}

fn respond(stream: &mut TcpStream, status: &str, message: &str) {
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>Idle Stock Finder</title>\
         <body style=\"font:16px system-ui,sans-serif;margin:3em\"><p>{message}</p></body>"
    );
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.flush();
}

/// Trades the authorization code and verifier for tokens.
pub fn exchange_code(
    client: &HttpClient,
    app_id: &str,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
    scopes: &[String],
) -> Result<Tokens, Error> {
    let body = json!({
        "client_id": app_id,
        "grant_type": "authorization_code",
        "code": code,
        "code_verifier": verifier,
        "redirect_uri": redirect_uri,
    });
    let response = client.send(Method::Post, "/oauth2/token", None, Some(&body))?;
    tokens_from(&response.body, scopes.to_vec(), client.environment())
}

/// Uses the refresh token once. Square returns a new refresh token, which the
/// caller must save before anything else: the old one no longer works.
pub fn refresh(client: &HttpClient, app_id: &str, current: &Tokens) -> Result<Tokens, Error> {
    let body = json!({
        "client_id": app_id,
        "grant_type": "refresh_token",
        "refresh_token": current.refresh_token,
    });
    let response = client.send(Method::Post, "/oauth2/token", None, Some(&body))?;
    let mut tokens = tokens_from(&response.body, current.scopes.clone(), client.environment())?;
    if tokens.merchant_id.is_empty() {
        tokens.merchant_id = current.merchant_id.clone();
    }
    Ok(tokens)
}

fn tokens_from(body: &Value, scopes: Vec<String>, env: &Environment) -> Result<Tokens, Error> {
    let text = |key: &str| body.get(key).and_then(Value::as_str).map(str::to_string);
    let time = |key: &str| {
        text(key).and_then(|t| DateTime::parse_from_rfc3339(&t).ok()).map(|t| t.with_timezone(&Utc))
    };
    let access_token =
        text("access_token").ok_or_else(|| Error::Decode("token response without access_token".into()))?;
    let refresh_token =
        text("refresh_token").ok_or_else(|| Error::Decode("token response without refresh_token".into()))?;
    let now = Utc::now();
    Ok(Tokens {
        access_token,
        access_expires_at: time("expires_at").unwrap_or(now + chrono::Duration::days(30)),
        refresh_token,
        refresh_expires_at: time("refresh_token_expires_at"),
        obtained_at: now,
        merchant_id: text("merchant_id").unwrap_or_default(),
        scopes,
        environment: env.name().to_string(),
    })
}

/// Whether a check should refresh: the access token is more than 7 days old,
/// or expires within 7 days, whichever comes first.
pub fn needs_refresh(tokens: &Tokens, now: DateTime<Utc>) -> bool {
    now - tokens.obtained_at > chrono::Duration::days(7)
        || tokens.access_expires_at - now < chrono::Duration::days(7)
}

/// A refresh failure that means the connection is gone (as opposed to a
/// network problem, which the next check simply retries).
pub fn refresh_rejected(e: &Error) -> bool {
    match e {
        Error::Api { status, .. } => matches!(status, 400 | 401 | 403),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn rfc7636_example() {
        // RFC 7636 appendix B.
        assert_eq!(
            challenge_for("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn verifier_shape() {
        let p = Pkce::new();
        assert!((43..=128).contains(&p.verifier.len()));
        assert!(p.verifier.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_ne!(Pkce::new().state, p.state);
    }

    #[test]
    fn authorize_url_has_everything() {
        let p = Pkce::new();
        let url =
            authorize_url(&Environment::Production, "sq0idp-abc", &scopes(true), &p, "http://localhost:5555");
        assert!(url.starts_with("https://connect.squareup.com/oauth2/authorize?"));
        let parsed = url::Url::parse(&url).unwrap();
        let q: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(q["client_id"], "sq0idp-abc");
        assert_eq!(q["session"], "false");
        assert_eq!(q["state"], p.state);
        assert_eq!(q["code_challenge"], p.challenge);
        assert_eq!(q["redirect_uri"], "http://localhost:5555");
        assert_eq!(q["scope"], "MERCHANT_PROFILE_READ ITEMS_READ ORDERS_READ INVENTORY_READ ITEMS_WRITE");
        assert!(!scopes(false).contains(&"ITEMS_WRITE".to_string()));
    }

    fn get(port: u16, target: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(s, "GET {target} HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n").unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    }

    #[test]
    fn callback_returns_code_and_ignores_favicon() {
        let listener = CallbackListener::bind().unwrap();
        let port = listener.port();
        let cancel = AtomicBool::new(false);
        let t = std::thread::spawn(move || {
            assert!(get(port, "/favicon.ico").starts_with("HTTP/1.1 404"));
            assert!(get(port, "/?code=abc%2B1&state=s123").starts_with("HTTP/1.1 200"));
        });
        let code = listener.wait("s123", Duration::from_secs(10), &cancel).unwrap();
        t.join().unwrap();
        assert_eq!(code, "abc+1");
    }

    #[test]
    fn callback_rejects_wrong_state_and_reports_denial() {
        let cancel = AtomicBool::new(false);
        let listener = CallbackListener::bind().unwrap();
        let port = listener.port();
        let t = std::thread::spawn(move || get(port, "/?code=abc&state=other"));
        assert!(matches!(
            listener.wait("s123", Duration::from_secs(10), &cancel),
            Err(ConnectError::StateMismatch)
        ));
        t.join().unwrap();

        let listener = CallbackListener::bind().unwrap();
        let port = listener.port();
        let t = std::thread::spawn(move || {
            get(port, "/?error=access_denied&error_description=user+denied&state=s123")
        });
        match listener.wait("s123", Duration::from_secs(10), &cancel) {
            Err(ConnectError::Denied(reason)) => assert_eq!(reason, "user denied"),
            other => panic!("{other:?}"),
        }
        t.join().unwrap();
    }

    #[test]
    fn callback_times_out_and_cancels() {
        let cancel = AtomicBool::new(false);
        let listener = CallbackListener::bind().unwrap();
        assert!(matches!(
            listener.wait("s", Duration::from_millis(100), &cancel),
            Err(ConnectError::TimedOut)
        ));
        let listener = CallbackListener::bind().unwrap();
        cancel.store(true, Ordering::Relaxed);
        assert!(matches!(listener.wait("s", Duration::from_secs(10), &cancel), Err(ConnectError::Canceled)));
    }
}
