use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Instant};

use axum::http::{Uri, uri::InvalidUri};
use tokio::{
    process::Child,
    sync::{Mutex, Notify},
};

use crate::startup::StartupError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceConfig {
    pub route_prefix: String,
    pub backend_host: String,
    pub backend_port: u16,
    pub strip_prefix: bool,
    pub command: String,
    pub args: Vec<String>,
    pub environment: HashMap<String, String>,
    pub working_directory: Option<PathBuf>,
    pub startup_timeout_ms: u64,
    pub idle_timeout_secs: u64,
    pub health_path: String,
}

pub struct ServiceEntry {
    pub config: ServiceConfig,
    pub(crate) runtime: Mutex<ServiceRuntime>,
    pub(crate) startup_notify: Notify,
}

pub(crate) struct ServiceRuntime {
    pub process: Option<Child>,
    pub last_used: Instant,
    pub startup_generation: u64,
    pub startup_in_progress: bool,
    pub last_startup_generation: Option<u64>,
    pub last_startup_error: Option<StartupError>,
}

#[derive(Clone)]
pub struct ServiceRegistry {
    services: HashMap<String, Arc<ServiceEntry>>,
}

#[derive(Debug, Eq, PartialEq)]
pub struct RouteParts<'a> {
    pub prefix: &'a str,
    pub backend_path: String,
}

pub struct ResolvedRoute {
    pub service: Arc<ServiceEntry>,
    pub backend_path: String,
}

impl ServiceConfig {
    pub fn backend_base_url(&self) -> String {
        format!("http://{}:{}", self.backend_host, self.backend_port)
    }

    pub fn healthcheck_uri(&self) -> Result<Uri, InvalidUri> {
        let target = format!(
            "{}{}",
            self.backend_base_url(),
            normalize_health_path(&self.health_path)
        );

        target.parse()
    }
}

impl ServiceEntry {
    pub fn new(config: ServiceConfig) -> Self {
        Self {
            config,
            runtime: Mutex::new(ServiceRuntime {
                process: None,
                last_used: Instant::now(),
                startup_generation: 0,
                startup_in_progress: false,
                last_startup_generation: None,
                last_startup_error: None,
            }),
            startup_notify: Notify::new(),
        }
    }

    pub async fn mark_used(&self) {
        let mut runtime = self.runtime.lock().await;
        runtime.last_used = Instant::now();
    }
}

impl ServiceRegistry {
    pub fn from_services(services: impl IntoIterator<Item = ServiceConfig>) -> Self {
        let services = services
            .into_iter()
            .map(|service| {
                let route_prefix = service.route_prefix.clone();
                let service = Arc::new(ServiceEntry::new(service));
                (route_prefix, service)
            })
            .collect();

        Self { services }
    }

    pub fn resolve(&self, path: &str) -> Option<ResolvedRoute> {
        let parts = split_route_prefix(path)?;
        let service = self.services.get(parts.prefix)?.clone();

        let backend_path = if service.config.strip_prefix {
            parts.backend_path
        } else {
            path.to_owned()
        };

        Some(ResolvedRoute {
            service,
            backend_path,
        })
    }

    pub fn all_services(&self) -> Vec<Arc<ServiceEntry>> {
        self.services.values().cloned().collect()
    }
}

pub fn split_route_prefix(path: &str) -> Option<RouteParts<'_>> {
    let trimmed = path.trim_start_matches('/');
    if trimmed.is_empty() {
        return None;
    }

    let mut segments = trimmed.splitn(2, '/');
    let prefix = segments.next()?;
    if prefix.is_empty() {
        return None;
    }

    let backend_path = match segments.next() {
        Some(remainder) if !remainder.is_empty() => format!("/{remainder}"),
        _ => "/".to_owned(),
    };

    Some(RouteParts {
        prefix,
        backend_path,
    })
}

fn normalize_health_path(path: &str) -> String {
    if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{
        RouteParts, ServiceConfig, ServiceRegistry, normalize_health_path, split_route_prefix,
    };

    fn test_service_config(route_prefix: &str, backend_base_url: &str) -> ServiceConfig {
        ServiceConfig {
            route_prefix: route_prefix.to_owned(),
            backend_host: "127.0.0.1".to_owned(),
            backend_port: backend_base_url
                .rsplit(':')
                .next()
                .unwrap()
                .parse()
                .unwrap(),
            strip_prefix: true,
            command: "cargo".to_owned(),
            args: vec!["run".to_owned()],
            environment: HashMap::new(),
            working_directory: None,
            startup_timeout_ms: 1_000,
            idle_timeout_secs: 60,
            health_path: "/health".to_owned(),
        }
    }

    #[test]
    fn extracts_prefix_and_remainder() {
        assert_eq!(
            split_route_prefix("/api/orders/123"),
            Some(RouteParts {
                prefix: "api",
                backend_path: "/orders/123".to_owned(),
            })
        );
    }

    #[test]
    fn maps_prefix_only_route_to_root() {
        assert_eq!(
            split_route_prefix("/api"),
            Some(RouteParts {
                prefix: "api",
                backend_path: "/".to_owned(),
            })
        );
    }

    #[test]
    fn rejects_empty_path() {
        assert_eq!(split_route_prefix("/"), None);
    }

    #[test]
    fn resolves_registered_service() {
        let registry =
            ServiceRegistry::from_services([test_service_config("media", "http://127.0.0.1:9002")]);

        let resolved = registry.resolve("/media/uploads/a.png").unwrap();

        assert_eq!(resolved.service.config.route_prefix, "media");
        assert_eq!(resolved.backend_path, "/uploads/a.png");
    }

    #[test]
    fn normalizes_health_path() {
        assert_eq!(normalize_health_path("health"), "/health");
        assert_eq!(normalize_health_path("/ready"), "/ready");
    }
}
