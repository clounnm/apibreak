mod support;

use std::sync::Arc;
use std::time::Duration;

use apibreak_core::{
    Action, Exchange, Outcome, Proxy, ProxyConfig, Scenario, ScenarioHandle, ScenarioSet, Store,
};
use support::{backend, send, Backend};
use tokio::net::TcpListener;

async fn start_proxy(backend: &Backend, cors: bool) -> u16 {
    start_recording(backend, cors).await.0
}

async fn start_recording(backend: &Backend, cors: bool) -> (u16, Arc<Store>) {
    let (port, store, _) = start_with(backend, cors, ScenarioSet::default()).await;
    (port, store)
}

async fn start_with(
    backend: &Backend,
    cors: bool,
    scenarios: ScenarioSet,
) -> (u16, Arc<Store>, ScenarioHandle) {
    let scenarios = ScenarioHandle::new(scenarios);
    let store = Arc::new(Store::new(16));
    let config = ProxyConfig {
        target: format!("http://127.0.0.1:{}", backend.port),
        cors,
    };
    let proxy = Proxy::new(config, store.clone(), scenarios.clone()).expect("proxy");
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(proxy.serve(listener));
    (port, store, scenarios)
}

#[tokio::test]
async fn forwards_method_path_query_headers_and_body() {
    let backend = backend("").await;
    let port = start_proxy(&backend, true).await;

    let reply = send(
        port,
        "PATCH",
        "/api/users/1?full=true",
        "X-Custom: yes\r\n",
        "payload",
    )
    .await
    .expect("reply");

    assert_eq!(reply.status, 200);
    assert!(
        reply
            .body
            .starts_with("PATCH /api/users/1?full=true HTTP/1.1"),
        "{}",
        reply.body
    );
    assert!(reply.body.contains("x-custom: yes"), "{}", reply.body);
    assert!(reply.body.ends_with("payload"), "{}", reply.body);
    assert!(
        reply
            .body
            .contains(&format!("host: 127.0.0.1:{}", backend.port)),
        "{}",
        reply.body
    );
}

#[tokio::test]
async fn unreachable_backend_is_a_502() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let dead = listener.local_addr().expect("addr").port();
    drop(listener);
    let port = start_proxy(
        &Backend {
            port: dead,
            hits: Default::default(),
        },
        true,
    )
    .await;

    let reply = send(port, "GET", "/", "", "").await.expect("reply");

    assert_eq!(reply.status, 502);
    assert!(
        reply.body.contains("upstream request failed"),
        "{}",
        reply.body
    );
}

#[tokio::test]
async fn answers_preflight_without_touching_the_backend() {
    let backend = backend("").await;
    let port = start_proxy(&backend, true).await;
    let extra = "Origin: http://localhost:5173\r\nAccess-Control-Request-Method: POST\r\n\
                 Access-Control-Request-Headers: authorization\r\n";

    let reply = send(port, "OPTIONS", "/api/login", extra, "")
        .await
        .expect("reply");

    assert_eq!(reply.status, 204);
    assert_eq!(
        reply.header("access-control-allow-origin").as_deref(),
        Some("http://localhost:5173")
    );
    assert_eq!(
        reply.header("access-control-allow-headers").as_deref(),
        Some("authorization")
    );
    assert_eq!(backend.hit_count(), 0);
}

#[tokio::test]
async fn cors_headers_replace_upstream_ones_and_skip_other_origins() {
    let backend = support::backend("Access-Control-Allow-Origin: https://prod.example\r\n").await;
    let port = start_proxy(&backend, true).await;

    let local = send(port, "GET", "/", "Origin: http://localhost:3000\r\n", "")
        .await
        .expect("reply");
    assert_eq!(
        local.header("access-control-allow-origin").as_deref(),
        Some("http://localhost:3000")
    );

    let foreign = send(port, "GET", "/", "Origin: https://evil.example\r\n", "")
        .await
        .expect("reply");
    assert_eq!(
        foreign.header("access-control-allow-origin").as_deref(),
        Some("https://prod.example")
    );
}

#[tokio::test]
async fn cors_can_be_disabled() {
    let backend = backend("").await;
    let port = start_proxy(&backend, false).await;
    let extra = "Origin: http://localhost:5173\r\nAccess-Control-Request-Method: POST\r\n";

    let reply = send(port, "OPTIONS", "/", extra, "").await.expect("reply");

    assert_eq!(reply.status, 200, "preflight goes to the backend");
    assert_eq!(backend.hit_count(), 1);
}

#[tokio::test]
async fn handles_concurrent_requests() {
    let backend = backend("").await;
    let port = start_proxy(&backend, true).await;

    let mut tasks = tokio::task::JoinSet::new();
    for index in 0..32 {
        tasks.spawn(async move {
            let path = format!("/item/{index}");
            let reply = send(port, "GET", &path, "", "").await.expect("reply");
            assert!(
                reply.body.starts_with(&format!("GET {path} ")),
                "{}",
                reply.body
            );
        });
    }
    while let Some(result) = tasks.join_next().await {
        result.expect("request task");
    }
    assert_eq!(backend.hit_count(), 32);
}

/// The exchange is finished just after the client has the last byte.
async fn finished(store: &Store) -> Arc<Exchange> {
    for _ in 0..200 {
        match store.snapshot().last() {
            Some(entry) if entry.outcome != Outcome::Pending => return entry.clone(),
            _ => tokio::time::sleep(Duration::from_millis(5)).await,
        }
    }
    panic!("exchange never finished");
}

#[tokio::test]
async fn records_request_and_response() {
    let backend = backend("").await;
    let (port, store) = start_recording(&backend, true).await;

    send(port, "POST", "/api/login?next=1", "", "secret")
        .await
        .expect("reply");

    let entry = finished(&store).await;
    assert_eq!(entry.method, "POST");
    assert_eq!(entry.path, "/api/login");
    assert_eq!(entry.uri, "/api/login?next=1");
    assert_eq!(&entry.request_body.bytes[..], b"secret");
    let Outcome::Response(response) = &entry.outcome else {
        panic!("expected response, got {:?}", entry.outcome);
    };
    assert_eq!(response.status, 200);
    assert!(!response.body.truncated);
    assert!(String::from_utf8_lossy(&response.body.bytes).contains("POST /api/login"));
    assert!(entry.duration.is_some());
}

#[tokio::test]
async fn offers_backend_only_gzip_when_browser_accepts_encodings() {
    let backend = backend("").await;
    let port = start_proxy(&backend, true).await;

    let extra = "Accept-Encoding: gzip, deflate, br, zstd\r\n";
    let reply = send(port, "GET", "/", extra, "").await.expect("reply");

    assert!(
        reply.body.contains("accept-encoding: gzip\r\n"),
        "{}",
        reply.body
    );
}

fn scenarios(match_text: &str, action: Action) -> ScenarioSet {
    let endpoint = match_text.parse().expect("endpoint");
    ScenarioSet::new(vec![Scenario { endpoint, action }])
}

#[tokio::test]
async fn status_scenario_skips_the_backend_and_is_labelled() {
    let backend = backend("").await;
    let set = scenarios("GET /api/users/:id", Action::Status(500));
    let (port, store, _) = start_with(&backend, true, set).await;

    let reply = send(port, "GET", "/api/users/7", "", "")
        .await
        .expect("reply");

    assert_eq!(reply.status, 500);
    assert!(
        reply.body.contains("Internal Server Error"),
        "{}",
        reply.body
    );
    assert_eq!(backend.hit_count(), 0);
    assert_eq!(finished(&store).await.scenario.as_deref(), Some("500"));

    let other = send(port, "GET", "/api/users", "", "")
        .await
        .expect("reply");
    assert_eq!(other.status, 200);
}

#[tokio::test]
async fn status_401_says_the_token_expired() {
    let backend = backend("").await;
    let set = scenarios("GET /me", Action::Status(401));
    let (port, _, _) = start_with(&backend, true, set).await;
    let reply = send(port, "GET", "/me", "", "").await.expect("reply");
    assert_eq!(reply.status, 401);
    assert!(reply.body.contains("token_expired"), "{}", reply.body);
}

#[tokio::test]
async fn scenario_responses_still_carry_cors_headers() {
    let backend = backend("").await;
    let set = scenarios("GET /a", Action::Status(500));
    let (port, _, _) = start_with(&backend, true, set).await;
    let extra = "Origin: http://localhost:5173\r\n";
    let reply = send(port, "GET", "/a", extra, "").await.expect("reply");
    assert!(reply.header("access-control-allow-origin").is_some());
}

#[tokio::test]
async fn preflight_passes_even_when_the_endpoint_is_broken() {
    let backend = backend("").await;
    let set = scenarios("OPTIONS /a", Action::Offline);
    let (port, _, _) = start_with(&backend, true, set).await;
    let extra = "Origin: http://localhost:5173\r\nAccess-Control-Request-Method: GET\r\n";
    let reply = send(port, "OPTIONS", "/a", extra, "").await.expect("reply");
    assert_eq!(reply.status, 204);
}

#[tokio::test]
async fn delay_scenario_holds_the_response_then_forwards() {
    let backend = backend("").await;
    let set = scenarios("GET /slow", Action::Delay(Duration::from_millis(200)));
    let (port, _, _) = start_with(&backend, true, set).await;

    let started = std::time::Instant::now();
    let reply = send(port, "GET", "/slow", "", "").await.expect("reply");

    assert!(started.elapsed() >= Duration::from_millis(200));
    assert_eq!(reply.status, 200);
    assert_eq!(backend.hit_count(), 1);
}

#[tokio::test]
async fn offline_scenario_closes_the_socket_without_a_response() {
    let backend = backend("").await;
    let set = scenarios("GET /down", Action::Offline);
    let (port, store, _) = start_with(&backend, true, set).await;

    assert!(send(port, "GET", "/down", "", "").await.is_none());

    assert_eq!(backend.hit_count(), 0);
    let entry = finished(&store).await;
    assert_eq!(
        entry.outcome,
        Outcome::Failed("connection dropped by scenario".into())
    );
}

#[tokio::test]
async fn empty_scenario_follows_the_shape_of_the_real_response() {
    let backend = support::json_backend("[1,2]").await;
    let (port, store, handle) = start_with(&backend, true, ScenarioSet::default()).await;
    let action = || Scenario {
        endpoint: "GET /list".parse().expect("endpoint"),
        action: Action::Empty,
    };

    send(port, "GET", "/list", "", "").await.expect("reply");
    finished(&store).await;
    handle.update(|set| set.toggle(action()));

    let reply = send(port, "GET", "/list", "", "").await.expect("reply");
    assert_eq!((reply.status, reply.body.as_str()), (200, "[]"));

    store.clear();
    let reply = send(port, "GET", "/list", "", "").await.expect("reply");
    assert_eq!((reply.status, reply.body.as_str()), (204, ""));
}

#[tokio::test]
async fn scenarios_take_effect_without_a_restart() {
    let backend = backend("").await;
    let (port, _, handle) = start_with(&backend, true, ScenarioSet::default()).await;
    let action = || Scenario {
        endpoint: "GET /a".parse().expect("endpoint"),
        action: Action::Status(404),
    };

    handle.update(|set| set.toggle(action()));
    assert_eq!(
        send(port, "GET", "/a", "", "").await.expect("reply").status,
        404
    );

    handle.update(|set| set.toggle(action()));
    assert_eq!(
        send(port, "GET", "/a", "", "").await.expect("reply").status,
        200
    );
}
