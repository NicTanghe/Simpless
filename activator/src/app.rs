use std::{fs, io, path::PathBuf, sync::Arc};

use axum::{Router, body::Body, routing::get};
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::TokioExecutor,
};
use tokio::sync::RwLock;

use crate::{admin, health, proxy, registry::ServiceRegistry, startup};

pub type ProxyClient = Client<HttpConnector, Body>;

pub struct AppState {
    pub registry: RwLock<ServiceRegistry>,
    pub client: ProxyClient,
    pub config_path: PathBuf,
    pub upload_dir: PathBuf,
}

impl AppState {
    pub fn new(registry: ServiceRegistry, config_path: PathBuf, upload_dir: PathBuf) -> Result<Self, io::Error> {
        fs::create_dir_all(&upload_dir)?;
        let connector = HttpConnector::new();
        let client = Client::builder(TokioExecutor::new()).build(connector);

        Ok(Self {
            registry: RwLock::new(registry),
            client,
            config_path,
            upload_dir,
        })
    }

    pub async fn registry_snapshot(&self) -> ServiceRegistry {
        self.registry.read().await.clone()
    }

    pub async fn replace_registry(&self, registry: ServiceRegistry) {
        let previous = {
            let mut current = self.registry.write().await;
            std::mem::replace(&mut *current, registry)
        };

        for service in previous.all_services() {
            if let Err(error) = startup::stop_service_process(&service).await {
                tracing::warn!(
                    route_prefix = %service.config.route_prefix,
                    error = %error,
                    "failed to stop backend while reloading registry"
                );
            }
        }
    }
}

pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health::health))
        .route("/ready", get(health::ready))
        .nest("/_admin/api", admin::build_admin_router())
        .fallback(proxy::proxy_request)
        .with_state(state)
}
