//! A tiny JSON backend to try apibreak against.
//!
//! ```text
//! cargo run --example demo-server            # listens on port 3000
//! cargo run --example demo-server -- 8080
//! ```

use std::convert::Infallible;
use std::net::{Ipv4Addr, SocketAddr};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::{HeaderValue, CONTENT_TYPE};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use tokio::net::TcpListener;

fn users() -> Vec<Value> {
    vec![
        json!({ "id": 1, "name": "Ada Lovelace", "email": "ada@example.com", "role": "admin" }),
        json!({ "id": 2, "name": "Alan Turing", "email": "alan@example.com", "role": "editor" }),
        json!({ "id": 3, "name": "Grace Hopper", "email": "grace@example.com", "role": "viewer" }),
    ]
}

async fn route(request: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();

    let (status, body) = match (&method, segments.as_slice()) {
        (&Method::GET, ["api", "users"]) => (StatusCode::OK, Value::Array(users())),
        (&Method::GET, ["api", "users", id]) => {
            let found = users()
                .into_iter()
                .find(|user| id.parse().ok() == user["id"].as_u64());
            match found {
                Some(user) => (StatusCode::OK, user),
                None => (StatusCode::NOT_FOUND, json!({ "error": "user not found" })),
            }
        }
        (&Method::POST, ["api", "login"]) => {
            let body = request.into_body().collect().await;
            let credentials = body
                .ok()
                .and_then(|body| serde_json::from_slice::<Value>(&body.to_bytes()).ok());
            match credentials {
                Some(value) if value.get("email").is_some() => (
                    StatusCode::OK,
                    json!({ "token": "demo-token", "user": users()[0] }),
                ),
                _ => (
                    StatusCode::BAD_REQUEST,
                    json!({ "error": "email is required" }),
                ),
            }
        }
        _ => (StatusCode::NOT_FOUND, json!({ "error": "not found" })),
    };
    println!("{method} {path} -> {}", status.as_u16());

    let mut response = Response::new(Full::new(Bytes::from(body.to_string())));
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    Ok(response)
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let port = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(3000);
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port))).await?;
    println!("demo server on http://localhost:{port}");
    loop {
        let (stream, _) = listener.accept().await?;
        tokio::spawn(async move {
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service_fn(route))
                .await;
        });
    }
}
