//! CORS handling so a dev server on any localhost port can call the proxy.
//!
//! Only localhost origins are answered. The proxy reflects the origin and
//! allows credentials, so doing that for every origin would let any website
//! open in the developer's browser read the proxied API.

use http::header::{self, HeaderMap, HeaderName, HeaderValue};
use url::Url;

const ALLOW_METHODS: &str = "GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS";
const ALLOW_PRIVATE_NETWORK: HeaderName =
    HeaderName::from_static("access-control-allow-private-network");
const REQUEST_PRIVATE_NETWORK: HeaderName =
    HeaderName::from_static("access-control-request-private-network");

/// Returns the request's `Origin` if it is a localhost origin.
pub fn localhost_origin(headers: &HeaderMap) -> Option<HeaderValue> {
    let origin = headers.get(header::ORIGIN)?;
    let url = Url::parse(origin.to_str().ok()?).ok()?;
    let host = url.host_str()?;
    let local = matches!(host, "localhost" | "127.0.0.1" | "[::1]") || host.ends_with(".localhost");
    local.then(|| origin.clone())
}

/// True for a browser CORS preflight, as opposed to a plain `OPTIONS` call.
pub fn is_preflight(method: &http::Method, headers: &HeaderMap) -> bool {
    method == http::Method::OPTIONS && headers.contains_key(header::ACCESS_CONTROL_REQUEST_METHOD)
}

/// Replaces any upstream CORS headers with ones allowing `origin`.
pub fn apply(headers: &mut HeaderMap, origin: &HeaderValue) {
    let upstream: Vec<HeaderName> = headers
        .keys()
        .filter(|name| name.as_str().starts_with("access-control-"))
        .cloned()
        .collect();
    for name in upstream {
        headers.remove(name);
    }
    // `*` is not a wildcard on credentialed requests, so list the names.
    let exposed = headers
        .keys()
        .map(HeaderName::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    if let Ok(exposed) = HeaderValue::from_str(&exposed) {
        headers.insert(header::ACCESS_CONTROL_EXPOSE_HEADERS, exposed);
    }
    headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
        HeaderValue::from_static("true"),
    );
    headers.append(header::VARY, HeaderValue::from_static("Origin"));
}

/// Headers answering the preflight described by `request`.
pub fn preflight_headers(request: &HeaderMap, origin: &HeaderValue) -> HeaderMap {
    let mut headers = HeaderMap::new();
    apply(&mut headers, origin);
    headers.remove(header::ACCESS_CONTROL_EXPOSE_HEADERS);
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static(ALLOW_METHODS),
    );
    if let Some(requested) = request.get(header::ACCESS_CONTROL_REQUEST_HEADERS) {
        headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, requested.clone());
    }
    if request.contains_key(REQUEST_PRIVATE_NETWORK) {
        headers.insert(ALLOW_PRIVATE_NETWORK, HeaderValue::from_static("true"));
    }
    headers.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static("600"),
    );
    headers
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_origin(origin: &'static str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::ORIGIN, HeaderValue::from_static(origin));
        headers
    }

    #[test]
    fn accepts_localhost_origins_only() {
        for origin in [
            "http://localhost:5173",
            "https://localhost",
            "http://127.0.0.1:3000",
            "http://[::1]:3000",
            "http://app.localhost:3000",
        ] {
            assert!(localhost_origin(&with_origin(origin)).is_some(), "{origin}");
        }
        for origin in [
            "https://evil.example",
            "http://localhost.evil.example",
            "null",
        ] {
            assert!(localhost_origin(&with_origin(origin)).is_none(), "{origin}");
        }
    }

    #[test]
    fn apply_replaces_upstream_cors_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static("https://app.example"),
        );
        headers.insert("x-request-id", HeaderValue::from_static("1"));
        apply(
            &mut headers,
            &HeaderValue::from_static("http://localhost:1"),
        );

        assert_eq!(
            headers[header::ACCESS_CONTROL_ALLOW_ORIGIN],
            "http://localhost:1"
        );
        assert_eq!(headers[header::ACCESS_CONTROL_ALLOW_CREDENTIALS], "true");
        assert_eq!(
            headers[header::ACCESS_CONTROL_EXPOSE_HEADERS],
            "x-request-id"
        );
    }
}
