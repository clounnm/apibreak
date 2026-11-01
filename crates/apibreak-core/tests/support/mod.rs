//! A raw-socket HTTP backend and client, so the proxy is tested against real
//! connections on random ports with nothing hiding what is on the wire.
#![allow(dead_code)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Parsed HTTP/1 response. `None` from [`send`] means the socket closed first.
#[derive(Debug)]
pub struct Reply {
    pub status: u16,
    pub head: String,
    pub body: String,
}

impl Reply {
    pub fn header(&self, name: &str) -> Option<String> {
        self.head.lines().skip(1).find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name)
                .then(|| value.trim().to_owned())
        })
    }
}

/// Sends raw request text with `Connection: close` and reads the reply.
pub async fn send(port: u16, method: &str, path: &str, extra: &str, body: &str) -> Option<Reply> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\
         Content-Length: {}\r\n{extra}\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.expect("write");
    let mut raw = Vec::new();
    let _ = stream.read_to_end(&mut raw).await;
    let raw = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = raw.split_once("\r\n\r\n")?;
    let status = head.split_whitespace().nth(1)?.parse().ok()?;
    Some(Reply {
        status,
        head: head.to_owned(),
        body: body.to_owned(),
    })
}

pub struct Backend {
    pub port: u16,
    pub hits: Arc<AtomicUsize>,
}

impl Backend {
    pub fn hit_count(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

/// Starts a backend echoing the request back as its body, with `headers`
/// appended to every reply.
pub async fn backend(headers: &'static str) -> Backend {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let counter = counter.clone();
            tokio::spawn(async move {
                let request = read_request(&mut stream).await;
                counter.fetch_add(1, Ordering::SeqCst);
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n{headers}\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{request}",
                    request.len()
                );
                let _ = stream.write_all(reply.as_bytes()).await;
            });
        }
    });
    Backend { port, hits }
}

/// Starts a backend answering every request with `body` as JSON.
pub async fn json_backend(body: &'static str) -> Backend {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let counter = counter.clone();
            tokio::spawn(async move {
                read_request(&mut stream).await;
                counter.fetch_add(1, Ordering::SeqCst);
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes()).await;
            });
        }
    });
    Backend { port, hits }
}

async fn read_request(stream: &mut TcpStream) -> String {
    let mut raw = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = stream.read(&mut chunk).await.unwrap_or(0);
        if read == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..read]);
        let text = String::from_utf8_lossy(&raw);
        if let Some((head, body)) = text.split_once("\r\n\r\n") {
            let length = head
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|value| value.trim().parse::<usize>().unwrap_or(0))
                })
                .unwrap_or(0);
            if body.len() >= length {
                break;
            }
        }
    }
    String::from_utf8_lossy(&raw).into_owned()
}
