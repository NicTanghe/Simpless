use std::sync::Arc;

use axum::{Router, body::Body, routing::get};
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::TokioExecutor,
};

use crate::{health, proxy, registry::ServiceRegistry};

pub type ProxyClient = Client<HttpConnector, Body>;

#[derive(Clone)]
pub struct AppState {
    pub registry: ServiceRegistry,
    pub client: ProxyClient,
}

impl AppState {
    pub fn new(registry: ServiceRegistry) -> Self {
        let connector = HttpConnector::new();
        let client = Client::builder(TokioExecutor::new()).build(connector);

        Self { registry, client }
    }
}

pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health::health))
        .route("/ready", get(health::ready))
        .fallback(proxy::proxy_request)
        .with_state(state)
}
