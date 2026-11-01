//! The reverse proxy.
//!
//! Built on hyper directly rather than axum: there is no routing to do, and
//! returning an error from the service closes the socket without a response,
//! which is what the offline scenario needs. reqwest is the upstream client
//! because it brings TLS, HTTP/2 and connection pooling; with its decompression
//! features off, bodies reach the browser exactly as the backend sent them.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::header::{self, HeaderMap, HeaderName, HeaderValue};
use http::{Request, Response, StatusCode};
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use url::Url;

use crate::cors;
use crate::error::{Error, Result};

/// Request bodies are buffered before forwarding, so they need a ceiling.
const MAX_REQUEST_BODY: usize = 256 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type ProxyBody = UnsyncBoxBody<Bytes, BoxError>;

/// Settings for [`Proxy`].
#[derive(Debug, Clone)]
pub struct ProxyConfig {
    /// Backend base URL, optionally with a path prefix.
    pub target: String,
    /// Answer preflights and add CORS headers for localhost origins.
    pub cors: bool,
}

/// A cheaply cloneable handle to the running proxy state.
#[derive(Clone)]
pub struct Proxy {
    inner: Arc<Inner>,
}

struct Inner {
    /// Target URL without a trailing slash.
    base: String,
    cors: bool,
    client: reqwest::Client,
}

/// Makes hyper close the connection without writing a response.
#[derive(Debug, thiserror::Error)]
#[error("connection dropped on purpose")]
struct DropConnection;

impl Proxy {
    pub fn new(config: ProxyConfig) -> Result<Self> {
        let url =
            Url::parse(&config.target).map_err(|_| Error::InvalidTarget(config.target.clone()))?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err(Error::InvalidTarget(config.target));
        }
        // Fails only if a provider is already installed, which is just as good.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()?;
        let base = url.as_str().trim_end_matches('/').to_owned();
        Ok(Self {
            inner: Arc::new(Inner {
                base,
                cors: config.cors,
                client,
            }),
        })
    }

    /// Accepts connections until the task is cancelled.
    pub async fn serve(self, listener: TcpListener) -> Result<()> {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(accepted) => accepted,
                // A connection reset while queued must not stop the proxy.
                Err(error) if is_transient(&error) => continue,
                Err(error) => return Err(Error::Accept(error)),
            };
            let proxy = self.clone();
            tokio::spawn(async move {
                let service = service_fn(move |request| {
                    let proxy = proxy.clone();
                    async move { proxy.handle(request).await }
                });
                // Errors here are client disconnects or dropped connections.
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
    }

    async fn handle(
        &self,
        request: Request<Incoming>,
    ) -> std::result::Result<Response<ProxyBody>, DropConnection> {
        let origin = self
            .inner
            .cors
            .then(|| cors::localhost_origin(request.headers()))
            .flatten();

        let (parts, body) = request.into_parts();
        if let Some(origin) = &origin {
            if cors::is_preflight(&parts.method, &parts.headers) {
                let headers = cors::preflight_headers(&parts.headers, origin);
                return Ok(respond(StatusCode::NO_CONTENT, headers, Bytes::new()));
            }
        }

        let mut response = match Limited::new(body, MAX_REQUEST_BODY).collect().await {
            Ok(collected) => {
                self.forward(
                    parts.method,
                    &parts.uri,
                    &parts.headers,
                    collected.to_bytes(),
                )
                .await
            }
            Err(_) => error_response(StatusCode::PAYLOAD_TOO_LARGE, "request body too large"),
        };
        if let Some(origin) = &origin {
            cors::apply(response.headers_mut(), origin);
        }
        Ok(response)
    }

    async fn forward(
        &self,
        method: http::Method,
        uri: &http::Uri,
        headers: &HeaderMap,
        body: Bytes,
    ) -> Response<ProxyBody> {
        let path = uri.path_and_query().map_or("/", |value| value.as_str());
        let url = format!("{}{path}", self.inner.base);
        let mut upstream_headers = strip_hop_by_hop(headers);
        // reqwest derives Host from the URL, which is the rewrite we want.
        upstream_headers.remove(header::HOST);

        let result = self
            .inner
            .client
            .request(method, url)
            .headers(upstream_headers)
            .body(body)
            .send()
            .await;
        let upstream = match result {
            Ok(upstream) => upstream,
            Err(error) => {
                let message = format!("upstream request failed: {}", root_cause(&error));
                return error_response(StatusCode::BAD_GATEWAY, &message);
            }
        };

        let status = upstream.status();
        let mut headers = strip_hop_by_hop(upstream.headers());
        self.rewrite_location(&mut headers);
        let body = reqwest::Body::from(upstream).map_err(BoxError::from);
        let mut response = Response::new(UnsyncBoxBody::new(body));
        *response.status_mut() = status;
        *response.headers_mut() = headers;
        response
    }

    /// Keeps the browser on the proxy when the backend redirects to itself.
    fn rewrite_location(&self, headers: &mut HeaderMap) {
        let rewritten = headers
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|location| location.strip_prefix(self.inner.base.as_str()))
            .filter(|rest| rest.is_empty() || rest.starts_with(['/', '?']))
            .map(|rest| if rest.is_empty() { "/" } else { rest })
            .and_then(|rest| HeaderValue::from_str(rest).ok());
        if let Some(value) = rewritten {
            headers.insert(header::LOCATION, value);
        }
    }
}

fn is_transient(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::Interrupted
    )
}

/// Copies `headers` without the ones that describe a single connection.
fn strip_hop_by_hop(headers: &HeaderMap) -> HeaderMap {
    const HOP_BY_HOP: [&str; 8] = [
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "proxy-connection",
        "te",
        "trailer",
        "transfer-encoding",
    ];
    let named_by_connection: Vec<String> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|name| name.trim().to_ascii_lowercase())
        .collect();
    let mut kept = HeaderMap::with_capacity(headers.len());
    for (name, value) in headers {
        let name_str = name.as_str();
        let dropped = HOP_BY_HOP.contains(&name_str)
            || *name == header::UPGRADE
            || named_by_connection.iter().any(|named| named == name_str);
        if !dropped {
            kept.append(HeaderName::clone(name), value.clone());
        }
    }
    kept
}

fn root_cause(error: &(dyn std::error::Error + 'static)) -> String {
    let mut current = error;
    while let Some(source) = current.source() {
        current = source;
    }
    current.to_string()
}

fn respond(status: StatusCode, headers: HeaderMap, body: Bytes) -> Response<ProxyBody> {
    let body = Full::new(body).map_err(|never: Infallible| match never {});
    let mut response = Response::new(UnsyncBoxBody::new(body));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    response
}

fn error_response(status: StatusCode, message: &str) -> Response<ProxyBody> {
    let body = serde_json::json!({ "error": message }).to_string();
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    respond(status, headers, Bytes::from(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_hop_by_hop_and_connection_named_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONNECTION,
            HeaderValue::from_static("close, x-private"),
        );
        headers.insert("x-private", HeaderValue::from_static("1"));
        headers.insert(
            header::TRANSFER_ENCODING,
            HeaderValue::from_static("chunked"),
        );
        headers.append(header::SET_COOKIE, HeaderValue::from_static("a=1"));
        headers.append(header::SET_COOKIE, HeaderValue::from_static("b=2"));

        let kept = strip_hop_by_hop(&headers);

        assert_eq!(kept.len(), 2);
        assert_eq!(kept.get_all(header::SET_COOKIE).iter().count(), 2);
    }

    #[test]
    fn rewrites_only_redirects_into_the_target() {
        let proxy = Proxy::new(ProxyConfig {
            target: "https://api.example.com/v1/".into(),
            cors: true,
        })
        .expect("proxy");
        for (location, expected) in [
            ("https://api.example.com/v1/users?a=1", "/users?a=1"),
            ("https://api.example.com/v1", "/"),
            (
                "https://api.example.com/v10/users",
                "https://api.example.com/v10/users",
            ),
            (
                "https://other.example/v1/users",
                "https://other.example/v1/users",
            ),
            ("/relative", "/relative"),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::LOCATION, HeaderValue::from_static(location));
            proxy.rewrite_location(&mut headers);
            assert_eq!(headers[header::LOCATION], expected);
        }
    }

    #[test]
    fn rejects_targets_that_are_not_http() {
        for target in ["ftp://example.com", "example.com", ""] {
            let config = ProxyConfig {
                target: target.into(),
                cors: true,
            };
            assert!(matches!(Proxy::new(config), Err(Error::InvalidTarget(_))));
        }
    }
}
