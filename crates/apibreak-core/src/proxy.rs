//! The reverse proxy.
//!
//! Built on hyper directly rather than axum: there is no routing to do, and
//! returning an error from the service closes the socket without a response,
//! which is what the offline scenario needs. reqwest is the upstream client
//! because it brings TLS, HTTP/2 and connection pooling; with its decompression
//! features off, bodies reach the browser exactly as the backend sent them.

use std::convert::Infallible;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{ready, Context, Poll};
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use http::header::{self, HeaderMap, HeaderName, HeaderValue};
use http::{Request, Response, StatusCode};
use http_body::{Body, Frame};
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use url::Url;

use crate::error::{Error, Result};
use crate::mock::MockStore;
use crate::scenario::{Action, Plan, Scenario, ScenarioHandle};
use crate::store::{Captured, Headers, NewExchange, Outcome, ResponseRecord, Store, CAPTURE_LIMIT};
use crate::{body as body_view, cors};

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
    /// Directory that relative mock paths start from.
    pub config_dir: PathBuf,
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
    store: Arc<Store>,
    scenarios: ScenarioHandle,
    mocks: MockStore,
}

/// Makes hyper close the connection without writing a response.
#[derive(Debug, thiserror::Error)]
#[error("connection dropped on purpose")]
struct DropConnection;

impl Proxy {
    pub fn new(config: ProxyConfig, store: Arc<Store>, scenarios: ScenarioHandle) -> Result<Self> {
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
                store,
                scenarios,
                mocks: MockStore::new(config.config_dir),
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
        let body = Limited::new(body, MAX_REQUEST_BODY).collect().await;
        let too_large = body.is_err();
        let body = body.map(|body| body.to_bytes()).unwrap_or_default();

        let started = Instant::now();
        // A preflight has to succeed for the real request to be sent at all,
        // so scenarios apply to the real request only.
        let preflight = origin.is_some() && cors::is_preflight(&parts.method, &parts.headers);
        let plan = if preflight {
            None
        } else {
            let scenarios = self.inner.scenarios.current();
            scenarios.plan(parts.method.as_str(), parts.uri.path())
        };
        let id = self.inner.store.begin(NewExchange {
            method: parts.method.to_string(),
            path: parts.uri.path().to_owned(),
            uri: path_and_query(&parts.uri).to_owned(),
            request_headers: header_list(&parts.headers),
            request_body: Captured::new(body.clone()),
            scenario: plan.as_ref().map(|plan| plan.label.clone()),
        });
        let mut recording = Recording::new(self.inner.store.clone(), id, started);

        if too_large {
            let response = error_response(StatusCode::PAYLOAD_TOO_LARGE, "request body too large");
            return Ok(finish(response, recording, origin.as_ref()));
        }
        if let Some(origin) = &origin {
            if cors::is_preflight(&parts.method, &parts.headers) {
                let headers = cors::preflight_headers(&parts.headers, origin);
                let response = respond(StatusCode::NO_CONTENT, headers, Bytes::new());
                return Ok(finish(response, recording, None));
            }
        }
        if let Some(Plan {
            delay, response, ..
        }) = plan
        {
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            if let Some(scenario) = response {
                let response = match self.scenario_response(&scenario).await {
                    Some(response) => response,
                    None => {
                        recording.failure = Some("connection dropped by scenario".to_owned());
                        return Err(DropConnection);
                    }
                };
                return Ok(finish(response, recording, origin.as_ref()));
            }
        }
        let response = self
            .forward(parts.method, &parts.uri, &parts.headers, body)
            .await;
        Ok(finish(response, recording, origin.as_ref()))
    }

    /// Builds the response a scenario calls for; `None` drops the connection.
    async fn scenario_response(&self, scenario: &Scenario) -> Option<Response<ProxyBody>> {
        Some(match &scenario.action {
            Action::Status(status) => status_response(*status),
            Action::Empty => self.empty_response(scenario),
            Action::Offline => return None,
            Action::Mock(file) => self.mock_response(file).await,
            // Delays are applied before this point and never decide a response.
            Action::Delay(_) => error_response(StatusCode::INTERNAL_SERVER_ERROR, "invalid plan"),
        })
    }

    /// A broken mock is reported as a 502 rather than silently forwarded, so
    /// the developer sees it instead of testing against the wrong data.
    async fn mock_response(&self, file: &str) -> Response<ProxyBody> {
        let mock = match self.inner.mocks.load(file).await {
            Ok(mock) => mock,
            Err(error) => return error_response(StatusCode::BAD_GATEWAY, &error.to_string()),
        };
        let Ok(status) = StatusCode::from_u16(mock.status) else {
            let message = format!("mock {file} has invalid status {}", mock.status);
            return error_response(StatusCode::BAD_GATEWAY, &message);
        };
        let mut headers = HeaderMap::new();
        for (name, value) in &mock.headers {
            if let (Ok(name), Ok(value)) = (
                HeaderName::from_bytes(name.as_bytes()),
                HeaderValue::from_str(value),
            ) {
                headers.append(name, value);
            }
        }
        respond(status, headers, mock.body_bytes())
    }

    /// `[]` if the endpoint last answered with an array, otherwise 204.
    fn empty_response(&self, scenario: &Scenario) -> Response<ProxyBody> {
        let last_real = self.inner.store.latest(|entry| {
            entry.scenario.is_none()
                && matches!(entry.outcome, Outcome::Response(_))
                && scenario.endpoint.matches(&entry.method, &entry.path)
        });
        let was_array = last_real.is_some_and(|entry| match &entry.outcome {
            Outcome::Response(response) => {
                let decoded = body_view::decode(&response.headers, &response.body);
                body_view::is_json_array(&decoded.bytes)
            }
            _ => false,
        });
        if was_array {
            json_response(StatusCode::OK, Bytes::from_static(b"[]"))
        } else {
            respond(StatusCode::NO_CONTENT, HeaderMap::new(), Bytes::new())
        }
    }

    async fn forward(
        &self,
        method: http::Method,
        uri: &http::Uri,
        headers: &HeaderMap,
        body: Bytes,
    ) -> Response<ProxyBody> {
        let url = format!("{}{}", self.inner.base, path_and_query(uri));
        let mut upstream_headers = strip_hop_by_hop(headers);
        // reqwest derives Host from the URL, which is the rewrite we want.
        upstream_headers.remove(header::HOST);
        // gzip is the one encoding the details panel can read back, so don't
        // let the backend pick brotli or zstd when the browser offers them.
        if upstream_headers.contains_key(header::ACCEPT_ENCODING) {
            upstream_headers.insert(header::ACCEPT_ENCODING, HeaderValue::from_static("gzip"));
        }

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

/// Applies CORS and wires the body up so the exchange is recorded as it ends.
fn finish(
    mut response: Response<ProxyBody>,
    mut recording: Recording,
    origin: Option<&HeaderValue>,
) -> Response<ProxyBody> {
    if let Some(origin) = origin {
        cors::apply(response.headers_mut(), origin);
    }
    recording.response = Some((response.status().as_u16(), header_list(response.headers())));
    response.map(|inner| UnsyncBoxBody::new(RecordedBody { inner, recording }))
}

/// Tracks an exchange until it is over. Finishing in `Drop` covers every way
/// out, including a client that disconnects while a response is streaming.
struct Recording {
    store: Arc<Store>,
    id: u64,
    started: Instant,
    response: Option<(u16, Headers)>,
    failure: Option<String>,
    captured: BytesMut,
    truncated: bool,
    ended: bool,
}

impl Recording {
    fn new(store: Arc<Store>, id: u64, started: Instant) -> Self {
        Self {
            store,
            id,
            started,
            response: None,
            failure: None,
            captured: BytesMut::new(),
            truncated: false,
            ended: false,
        }
    }

    fn capture(&mut self, data: &[u8]) {
        let room = CAPTURE_LIMIT.saturating_sub(self.captured.len());
        self.truncated |= data.len() > room;
        self.captured
            .extend_from_slice(&data[..data.len().min(room)]);
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        let outcome = match self.response.take() {
            Some((status, headers)) => Outcome::Response(ResponseRecord {
                status,
                headers,
                body: Captured {
                    bytes: self.captured.split().freeze(),
                    truncated: self.truncated || !self.ended,
                },
            }),
            None => Outcome::Failed(
                self.failure
                    .take()
                    .unwrap_or_else(|| "client disconnected".to_owned()),
            ),
        };
        self.store.finish(self.id, outcome, self.started.elapsed());
    }
}

/// Passes a response body through, copying what it sees into a [`Recording`].
/// Streaming keeps long-lived responses such as server-sent events working.
struct RecordedBody {
    inner: ProxyBody,
    recording: Recording,
}

impl Drop for RecordedBody {
    fn drop(&mut self) {
        // hyper stops polling at the content length, so the end of the stream
        // is not always observed as a final `None` frame.
        self.recording.ended |= self.inner.is_end_stream();
    }
}

impl Body for RecordedBody {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<std::result::Result<Frame<Bytes>, BoxError>>> {
        let this = self.get_mut();
        let frame = ready!(Pin::new(&mut this.inner).poll_frame(cx));
        match &frame {
            Some(Ok(frame)) => {
                if let Some(data) = frame.data_ref() {
                    this.recording.capture(data);
                }
            }
            Some(Err(_)) => {}
            None => this.recording.ended = true,
        }
        Poll::Ready(frame)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

fn path_and_query(uri: &http::Uri) -> &str {
    uri.path_and_query().map_or("/", |value| value.as_str())
}

fn header_list(headers: &HeaderMap) -> Headers {
    headers
        .iter()
        .map(|(name, value)| {
            let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
            (name.to_string(), value)
        })
        .collect()
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

fn json_response(status: StatusCode, body: Bytes) -> Response<ProxyBody> {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    respond(status, headers, body)
}

fn error_response(status: StatusCode, message: &str) -> Response<ProxyBody> {
    let body = serde_json::json!({ "error": message }).to_string();
    json_response(status, Bytes::from(body))
}

/// The answer to a status scenario.
fn status_response(status: u16) -> Response<ProxyBody> {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let message = match status {
        StatusCode::UNAUTHORIZED => "token_expired",
        other => other.canonical_reason().unwrap_or("error"),
    };
    let body = serde_json::json!({ "error": message, "source": "apibreak" });
    json_response(status, Bytes::from(body.to_string()))
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
        let config = ProxyConfig {
            target: "https://api.example.com/v1/".into(),
            cors: true,
            config_dir: PathBuf::new(),
        };
        let scenarios = ScenarioHandle::new(Default::default());
        let proxy = Proxy::new(config, Arc::new(Store::new(1)), scenarios).expect("proxy");
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
                config_dir: PathBuf::new(),
            };
            let result = Proxy::new(
                config,
                Arc::new(Store::new(1)),
                ScenarioHandle::new(Default::default()),
            );
            assert!(matches!(result, Err(Error::InvalidTarget(_))));
        }
    }
}
