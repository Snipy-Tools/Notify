use crate::store;
use crate::tray::UserEvent;
use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tao::event_loop::EventLoopProxy;

pub type Pending = Arc<Mutex<HashMap<u64, Sender<String>>>>;

const ADDR: &str = "127.0.0.1:47615";
const MAX_BODY: usize = 1 << 20;
const HOLD_FOR: Duration = Duration::from_secs(110);
const POLL: Duration = Duration::from_millis(150);

struct Request {
    path: String,
    host: String,
    origin: bool,
    body: Vec<u8>,
}

struct Context {
    proxy: EventLoopProxy<UserEvent>,
    pending: Pending,
    token: String,
    next_id: AtomicU64,
}

pub fn start(proxy: EventLoopProxy<UserEvent>) -> Pending {
    let listener = TcpListener::bind(ADDR).unwrap_or_else(|_| {
        eprintln!("notify is already running");
        std::process::exit(1);
    });
    let pending = Pending::default();
    let ctx = Arc::new(Context {
        proxy,
        pending: Arc::clone(&pending),
        token: store::token(),
        next_id: AtomicU64::new(1),
    });

    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let ctx = Arc::clone(&ctx);
            std::thread::spawn(move || handle(stream, &ctx));
        }
    });

    pending
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];

    let head_end = loop {
        if let Some(i) = find(&buf, b"\r\n\r\n") {
            break i;
        }
        if buf.len() > 16384 {
            return None;
        }
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };

    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split(' ');
    if request_line.next()? != "POST" {
        return None;
    }
    let path = request_line.next()?.to_string();

    let (mut length, mut host, mut origin, mut expect) = (0usize, String::new(), false, false);
    for line in lines {
        let (name, value) = line.split_once(':')?;
        let value = value.trim();
        match name.to_ascii_lowercase().as_str() {
            "content-length" => length = value.parse().ok()?,
            "host" => host = value.to_string(),
            "origin" => origin = true,
            "expect" => expect = value.eq_ignore_ascii_case("100-continue"),
            _ => {}
        }
    }
    if length > MAX_BODY {
        return None;
    }

    let mut body = buf[head_end + 4..].to_vec();
    if expect && body.len() < length {
        stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").ok()?;
    }
    while body.len() < length {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(length);

    Some(Request { path, host, origin, body })
}

fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).ok();
}

fn closed(stream: &TcpStream) -> bool {
    let mut probe = [0u8; 1];
    match stream.peek(&mut probe) {
        Ok(0) => true,
        Ok(_) => false,
        Err(e) => e.kind() != ErrorKind::WouldBlock,
    }
}

fn handle(mut stream: TcpStream, ctx: &Context) {
    let Some(req) = read_request(&mut stream) else {
        respond(&mut stream, "400 Bad Request", "");
        return;
    };

    let local_host = req.host == ADDR || req.host == "localhost:47615";
    let route = req.path.strip_prefix('/').and_then(|p| p.split_once('/'));
    let Some((token, kind)) = route else {
        respond(&mut stream, "404 Not Found", "");
        return;
    };
    if req.origin || !local_host || token != ctx.token {
        respond(&mut stream, "403 Forbidden", "");
        return;
    }

    let body = String::from_utf8_lossy(&req.body).into_owned();
    match kind {
        "status" => {
            ctx.proxy.send_event(UserEvent::Status(body)).ok();
            respond(&mut stream, "200 OK", "");
        }
        "hook" => hold(stream, ctx, body),
        _ => respond(&mut stream, "404 Not Found", ""),
    }
}

fn hold(mut stream: TcpStream, ctx: &Context, body: String) {
    let id = ctx.next_id.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::channel();
    ctx.pending.lock().unwrap().insert(id, tx);
    if ctx.proxy.send_event(UserEvent::Hook(id, body)).is_err() {
        ctx.pending.lock().unwrap().remove(&id);
        respond(&mut stream, "200 OK", "");
        return;
    }

    stream.set_nonblocking(true).ok();
    let started = Instant::now();
    let reply = loop {
        match rx.recv_timeout(POLL) {
            Ok(text) => break Some(text),
            Err(RecvTimeoutError::Timeout) => {
                if started.elapsed() >= HOLD_FOR || closed(&stream) {
                    break None;
                }
            }
            Err(RecvTimeoutError::Disconnected) => break None,
        }
    };

    ctx.pending.lock().unwrap().remove(&id);
    if reply.is_none() {
        ctx.proxy.send_event(UserEvent::Gone(id)).ok();
    }
    stream.set_nonblocking(false).ok();
    respond(&mut stream, "200 OK", reply.as_deref().unwrap_or(""));
}
