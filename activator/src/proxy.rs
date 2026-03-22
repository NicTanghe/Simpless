use std::{fmt, sync::Arc};

use axum::{
    body::Body,
    extract::State,
    http::{
        HeaderValue, Request, Response, StatusCode, Uri,
        header::{HOST, HeaderName},
    },
    response::IntoResponse,
};
use hyper::body::Incoming;

use crate::{app::AppState, startup};

const X_FORWARDED_HOST: HeaderName = HeaderName::from_static("x-forwarded-host");
const X_FORWARDED_PROTO: HeaderName = HeaderName::from_static("x-forwarded-proto");

pub async fn proxy_request(
    State(state): State<Arc<AppState>>,
    request: Request<Body>,
) -> Result<Response<Body>, ProxyError> {
    let request_path = request.uri().path().to_owned();
    let registry = state.registry_snapshot().await;
    let resolved = registry
        .resolve(&request_path)
        .ok_or_else(|| ProxyError::UnknownRoute(request_path.clone()))?;

    startup::ensure_service_ready(&resolved.service, &state.client)
        .await
        .map_err(ProxyError::ServiceStartup)?;

    let backend_base_url = resolved.service.config.backend_base_url();
    let target_uri = rewrite_uri(request.uri(), &backend_base_url, &resolved.backend_path)?;

    tracing::info!(
        route_prefix = %resolved.service.config.route_prefix,
        target = %target_uri,
        "proxying request"
    );

    let response = forward_request(&state, request, target_uri).await?;
    resolved.service.mark_used().await;
    Ok(response.map(Body::new))
}

async fn forward_request(
    state: &AppState,
    request: Request<Body>,
    target_uri: Uri,
) -> Result<Response<Incoming>, ProxyError> {
    let upstream_host = target_uri
        .authority()
        .map(|authority| authority.as_str().to_owned())
        .ok_or(ProxyError::MissingAuthority)?;

    let original_host = request.headers().get(HOST).cloned();
    let (mut parts, body) = request.into_parts();
    parts.uri = target_uri;
    parts.headers.insert(
        HOST,
        HeaderValue::from_str(&upstream_host).map_err(ProxyError::InvalidHeaderValue)?,
    );

    if let Some(host) = original_host {
        parts.headers.insert(X_FORWARDED_HOST, host);
    }

    parts
        .headers
        .insert(X_FORWARDED_PROTO, HeaderValue::from_static("http"));

    let outbound = Request::from_parts(parts, body);
    state
        .client
        .request(outbound)
        .await
        .map_err(ProxyError::Upstream)
}

pub fn rewrite_uri(
    original_uri: &Uri,
    backend_base_url: &str,
    backend_path: &str,
) -> Result<Uri, ProxyError> {
    let query_suffix = original_uri
        .query()
        .map(|query| format!("?{query}"))
        .unwrap_or_default();

    let target = format!(
        "{}{}{}",
        backend_base_url.trim_end_matches('/'),
        backend_path,
        query_suffix
    );

    target.parse::<Uri>().map_err(ProxyError::InvalidTargetUri)
}

#[derive(Debug)]
pub enum ProxyError {
    UnknownRoute(String),
    MissingAuthority,
    InvalidTargetUri(axum::http::uri::InvalidUri),
    InvalidHeaderValue(axum::http::header::InvalidHeaderValue),
    ServiceStartup(startup::StartupError),
    Upstream(hyper_util::client::legacy::Error),
}

impl fmt::Display for ProxyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownRoute(path) => write!(f, "no service is configured for path `{path}`"),
            Self::MissingAuthority => write!(f, "rewritten upstream URI is missing an authority"),
            Self::InvalidTargetUri(error) => write!(f, "target URI is invalid: {error}"),
            Self::InvalidHeaderValue(error) => write!(f, "failed to build proxied header: {error}"),
            Self::ServiceStartup(error) => write!(f, "{error}"),
            Self::Upstream(error) => write!(f, "upstream request failed: {error}"),
        }
    }
}

impl std::error::Error for ProxyError {}

impl IntoResponse for ProxyError {
    fn into_response(self) -> Response<Body> {
        match self {
            Self::UnknownRoute(path) => {
                tracing::warn!(%path, "rejecting unknown route prefix");
                (StatusCode::NOT_FOUND, "unknown service route").into_response()
            }
            Self::MissingAuthority | Self::InvalidTargetUri(_) | Self::InvalidHeaderValue(_) => {
                tracing::error!(error = %self, "proxy request could not be prepared");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "proxy configuration error",
                )
                    .into_response()
            }
            Self::ServiceStartup(error) => {
                let status_code = error.status_code();
                tracing::warn!(error = %error, "backend startup failed");
                (status_code, "backend unavailable").into_response()
            }
            Self::Upstream(_) => {
                tracing::warn!(error = %self, "upstream request failed");
                (StatusCode::BAD_GATEWAY, "upstream unavailable").into_response()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        fs,
        net::SocketAddr,
        path::{Path, PathBuf},
        sync::Arc,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{Method, Request, StatusCode, Uri},
        response::IntoResponse,
        routing::any,
    };
    use tokio::{net::TcpListener, task::JoinSet};
    use tower::ServiceExt;

    use super::rewrite_uri;
    use crate::{
        app::{AppState, build_router},
        reaper::spawn_reaper,
        registry::{ServiceConfig, ServiceRegistry},
    };

    fn test_service_config(route_prefix: &str, backend_addr: SocketAddr) -> ServiceConfig {
        ServiceConfig {
            route_prefix: route_prefix.to_owned(),
            backend_host: "127.0.0.1".to_owned(),
            backend_port: backend_addr.port(),
            strip_prefix: true,
            command: "cargo".to_owned(),
            args: vec!["run".to_owned()],
            environment: HashMap::new(),
            working_directory: None,
            startup_timeout_ms: 5_000,
            idle_timeout_secs: 60,
            health_path: "/health".to_owned(),
        }
    }

    fn hello_backend_service_config(
        route_prefix: &str,
        backend_addr: SocketAddr,
        environment: HashMap<String, String>,
        idle_timeout_secs: u64,
    ) -> ServiceConfig {
        ServiceConfig {
            route_prefix: route_prefix.to_owned(),
            backend_host: "127.0.0.1".to_owned(),
            backend_port: backend_addr.port(),
            strip_prefix: true,
            command: "cargo".to_owned(),
            args: vec!["run".to_owned(), "--quiet".to_owned()],
            environment,
            working_directory: Some(hello_backend_dir()),
            startup_timeout_ms: 30_000,
            idle_timeout_secs,
            health_path: "/health".to_owned(),
        }
    }

    #[test]
    fn rewrites_uri_and_preserves_query() {
        let original = Uri::from_static("/api/orders/123?expand=true");
        let rewritten = rewrite_uri(&original, "http://127.0.0.1:9001", "/orders/123").unwrap();

        assert_eq!(
            rewritten,
            Uri::from_static("http://127.0.0.1:9001/orders/123?expand=true")
        );
    }

    #[tokio::test]
    async fn proxies_to_registered_backend() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend_addr = listener.local_addr().unwrap();

        let backend = Router::new()
            .route("/health", any(|| async { "ok" }))
            .fallback(any(|request: Request<Body>| async move {
                let method = request.method().clone();
                let uri = request.uri().clone();
                let forwarded_host = request
                    .headers()
                    .get("x-forwarded-host")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or("-")
                    .to_owned();
                let body = to_bytes(request.into_body(), usize::MAX).await.unwrap();
                let body = String::from_utf8(body.to_vec()).unwrap();

                (
                    StatusCode::CREATED,
                    format!("{method} {uri} host={forwarded_host} body={body}"),
                )
                    .into_response()
            }));

        tokio::spawn(async move {
            axum::serve(listener, backend).await.unwrap();
        });

        let registry = ServiceRegistry::from_services([test_service_config("api", backend_addr)]);
        let app = build_router(test_app_state(registry));

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/orders/123?expand=true")
                    .header("host", "gateway.test")
                    .body(Body::from("ping"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();

        assert_eq!(
            body,
            "POST /orders/123?expand=true host=gateway.test body=ping"
        );
    }

    #[tokio::test]
    async fn starts_backend_on_first_request() {
        let backend_addr = reserve_local_addr().await;
        let registry = ServiceRegistry::from_services([hello_backend_service_config(
            "api",
            backend_addr,
            HashMap::from([(
                "HELLO_BACKEND_BIND_ADDR".to_owned(),
                backend_addr.to_string(),
            )]),
            120,
        )]);
        let app = build_router(test_app_state(registry));

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/demo")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();

        assert_eq!(body, "GET /demo x-forwarded-host=- body=");
    }

    #[tokio::test]
    async fn concurrent_requests_share_single_startup() {
        let backend_addr = reserve_local_addr().await;
        let marker_path = unique_marker_path("hello_backend_start");
        let _ = fs::remove_file(&marker_path);

        let registry = ServiceRegistry::from_services([hello_backend_service_config(
            "api",
            backend_addr,
            HashMap::from([
                (
                    "HELLO_BACKEND_BIND_ADDR".to_owned(),
                    backend_addr.to_string(),
                ),
                ("HELLO_BACKEND_START_DELAY_MS".to_owned(), "750".to_owned()),
                (
                    "HELLO_BACKEND_START_MARKER".to_owned(),
                    marker_path.to_string_lossy().into_owned(),
                ),
            ]),
            120,
        )]);
        let app = build_router(test_app_state(registry));

        let mut requests = JoinSet::new();
        for _ in 0..10 {
            let app = app.clone();
            requests.spawn(async move {
                app.oneshot(
                    Request::builder()
                        .method(Method::GET)
                        .uri("/api/demo")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
            });
        }

        let mut response_bodies = Vec::new();
        while let Some(result) = requests.join_next().await {
            let response = result.unwrap();
            assert_eq!(response.status(), StatusCode::OK);

            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            response_bodies.push(String::from_utf8(body.to_vec()).unwrap());
        }

        assert_eq!(response_bodies.len(), 10);
        assert!(
            response_bodies
                .iter()
                .all(|body| body == "GET /demo x-forwarded-host=- body=")
        );

        let marker_contents = fs::read_to_string(&marker_path).unwrap();
        assert_eq!(marker_contents.lines().count(), 1);

        let _ = fs::remove_file(marker_path);
    }

    #[tokio::test]
    async fn idle_reaper_stops_backend_and_later_request_restarts_it() {
        let backend_addr = reserve_local_addr().await;
        let marker_path = unique_marker_path("hello_backend_restarts");
        let _ = fs::remove_file(&marker_path);

        let registry = ServiceRegistry::from_services([hello_backend_service_config(
            "api",
            backend_addr,
            HashMap::from([
                (
                    "HELLO_BACKEND_BIND_ADDR".to_owned(),
                    backend_addr.to_string(),
                ),
                (
                    "HELLO_BACKEND_START_MARKER".to_owned(),
                    marker_path.to_string_lossy().into_owned(),
                ),
            ]),
            1,
        )]);
        let state = test_app_state(registry.clone());
        let reaper = spawn_reaper(state.clone(), Duration::from_millis(100));
        let app = build_router(state);

        let first_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/demo")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(first_response.status(), StatusCode::OK);
        assert!(wait_for_port_state(backend_addr, true, Duration::from_secs(5)).await);
        assert!(wait_for_port_state(backend_addr, false, Duration::from_secs(5)).await);

        let second_response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/demo")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(second_response.status(), StatusCode::OK);
        let body = to_bytes(second_response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8(body.to_vec()).unwrap(),
            "GET /demo x-forwarded-host=- body="
        );

        let marker_contents = fs::read_to_string(&marker_path).unwrap();
        assert_eq!(marker_contents.lines().count(), 2);

        assert!(wait_for_port_state(backend_addr, false, Duration::from_secs(5)).await);
        reaper.abort();
        let _ = fs::remove_file(marker_path);
    }

    async fn reserve_local_addr() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        addr
    }

    async fn wait_for_port_state(addr: SocketAddr, expected_open: bool, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            if port_is_open(addr).await == expected_open {
                return true;
            }

            if tokio::time::Instant::now() >= deadline {
                return false;
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn port_is_open(addr: SocketAddr) -> bool {
        tokio::net::TcpStream::connect(addr).await.is_ok()
    }

    fn hello_backend_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("hello_backend")
    }

    fn unique_marker_path(prefix: &str) -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}_{timestamp}.log"))
    }

    fn test_app_state(registry: ServiceRegistry) -> Arc<AppState> {
        let base = unique_test_dir("activator_proxy_state");
        Arc::new(
            AppState::new(registry, base.join("services.db"), base.join("uploads")).unwrap(),
        )
    }

    fn unique_test_dir(prefix: &str) -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}_{timestamp}"))
    }
}
