mod support;

use apibreak_core::{Proxy, ProxyConfig};
use support::{backend, send, Backend};
use tokio::net::TcpListener;

async fn start_proxy(backend: &Backend, cors: bool) -> u16 {
    let config = ProxyConfig {
        target: format!("http://127.0.0.1:{}", backend.port),
        cors,
    };
    let proxy = Proxy::new(config).expect("proxy");
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(proxy.serve(listener));
    port
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
