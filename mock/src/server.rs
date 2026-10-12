//! A small HTTP/1.1 server on loopback: keep-alive, Content-Length bodies,
//! one thread per connection. Hand-rolled so a test can make it drop a
//! connection mid-request, which an off-the-shelf server won't do.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

pub struct HttpRequest {
    pub method: String,
    /// Path and query, as sent.
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or("")
    }

    pub fn query(&self) -> Vec<(String, String)> {
        match self.target.split_once('?') {
            Some((_, q)) => url::form_urlencoded::parse(q.as_bytes()).into_owned().collect(),
            None => Vec::new(),
        }
    }
}

pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn json(status: u16, body: &serde_json::Value) -> Self {
        HttpResponse {
            status,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: serde_json::to_vec(body).unwrap(),
        }
    }
}

/// What the handler wants done with a request.
pub enum Reply {
    Respond(HttpResponse),
    /// Close the connection without answering (a network drop).
    Drop,
}

pub type Handler = Arc<dyn Fn(HttpRequest) -> Reply + Send + Sync>;

pub struct Server {
    pub addr: SocketAddr,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl Server {
    pub fn start(handler: Handler) -> std::io::Result<Server> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let addr = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop_accept = stop.clone();
        let accept = std::thread::Builder::new().name("mock-square".into()).spawn(move || {
            while !stop_accept.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let handler = handler.clone();
                        let stop = stop_accept.clone();
                        let _ = std::thread::Builder::new()
                            .name("mock-square-conn".into())
                            .spawn(move || serve(stream, handler, stop));
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(2)),
                }
            }
        })?;
        Ok(Server { addr, stop, accept: Some(accept) })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.accept.take() {
            let _ = t.join();
        }
    }
}

fn serve(stream: TcpStream, handler: Handler, stop: Arc<AtomicBool>) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    let _ = stream.set_nodelay(true);
    let Ok(read_half) = stream.try_clone() else { return };
    let mut reader = BufReader::new(read_half);
    let mut writer = stream;
    let mut idle = 0;
    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let request = match read_request(&mut reader) {
            Ok(Some(r)) => r,
            Ok(None) => return,
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                idle += 1;
                if idle > 300 {
                    return;
                }
                continue;
            }
            Err(_) => return,
        };
        idle = 0;
        let close = request.header("Connection").is_some_and(|v| v.eq_ignore_ascii_case("close"));
        match handler(request) {
            Reply::Drop => {
                let _ = writer.shutdown(Shutdown::Both);
                return;
            }
            Reply::Respond(response) => {
                if write_response(&mut writer, &response).is_err() || close {
                    return;
                }
            }
        }
    }
}

fn read_request(reader: &mut BufReader<TcpStream>) -> std::io::Result<Option<HttpRequest>> {
    let mut head = Vec::new();
    loop {
        let mut line = Vec::new();
        let n = reader.read_until(b'\n', &mut line)?;
        if n == 0 {
            return Ok(None);
        }
        if head.is_empty() && (line == b"\r\n" || line == b"\n") {
            continue;
        }
        head.extend_from_slice(&line);
        if line == b"\r\n" || line == b"\n" {
            break;
        }
        if head.len() > 1 << 20 {
            return Ok(None);
        }
    }
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut parsed = httparse::Request::new(&mut headers);
    if parsed.parse(&head).is_err() {
        return Ok(None);
    }
    let method = parsed.method.unwrap_or("").to_string();
    let target = parsed.path.unwrap_or("").to_string();
    let headers: Vec<(String, String)> = parsed
        .headers
        .iter()
        .map(|h| (h.name.to_string(), String::from_utf8_lossy(h.value).to_string()))
        .collect();
    let length = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    // The body follows the head immediately; wait for it even if slow.
    let stream = reader.get_ref();
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    reader.read_exact(&mut body)?;
    let _ = reader.get_ref().set_read_timeout(Some(Duration::from_millis(200)));
    Ok(Some(HttpRequest { method, target, headers, body }))
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

fn write_response(w: &mut TcpStream, r: &HttpResponse) -> std::io::Result<()> {
    let mut out = format!("HTTP/1.1 {} {}\r\n", r.status, reason(r.status));
    for (k, v) in &r.headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str(&format!("Content-Length: {}\r\n\r\n", r.body.len()));
    w.write_all(out.as_bytes())?;
    w.write_all(&r.body)?;
    w.flush()
}
