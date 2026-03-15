use std::{
    collections::{HashMap, HashSet},
    fmt, fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;

use crate::registry::{ServiceConfig, ServiceRegistry};

const DEFAULT_BACKEND_HOST: &str = "127.0.0.1";

#[derive(Debug, Deserialize)]
struct RawConfig {
    #[serde(rename = "service", default)]
    services: Vec<RawServiceConfig>,
}

#[derive(Debug, Deserialize)]
struct RawServiceConfig {
    route_prefix: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    port: u16,
    #[serde(default = "default_strip_prefix")]
    strip_prefix: bool,
    #[serde(default)]
    environment: HashMap<String, String>,
    working_directory: Option<PathBuf>,
    startup_timeout_ms: u64,
    idle_timeout_secs: u64,
    health_path: String,
}

pub fn load_registry_from_path(path: &Path) -> Result<ServiceRegistry, ConfigError> {
    let raw = fs::read_to_string(path).map_err(|source| ConfigError::ReadFailed {
        path: path.to_path_buf(),
        source,
    })?;

    let parsed: RawConfig = toml::from_str(&raw).map_err(|source| ConfigError::ParseFailed {
        path: path.to_path_buf(),
        source,
    })?;

    let services = build_service_configs(parsed, path)?;
    Ok(ServiceRegistry::from_services(services))
}

pub fn default_config_path() -> PathBuf {
    PathBuf::from("config/services.toml")
}

fn build_service_configs(
    parsed: RawConfig,
    path: &Path,
) -> Result<Vec<ServiceConfig>, ConfigError> {
    if parsed.services.is_empty() {
        return Err(ConfigError::NoServices {
            path: path.to_path_buf(),
        });
    }

    let mut seen_route_prefixes = HashSet::new();
    let mut seen_ports = HashSet::new();
    let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut services = Vec::with_capacity(parsed.services.len());

    for raw_service in parsed.services {
        validate_route_prefix(path, &raw_service.route_prefix, &mut seen_route_prefixes)?;
        validate_port(path, raw_service.port, &mut seen_ports)?;
        validate_command(path, &raw_service.command)?;
        validate_timeouts(
            path,
            &raw_service.route_prefix,
            raw_service.startup_timeout_ms,
            raw_service.idle_timeout_secs,
        )?;

        let working_directory = raw_service
            .working_directory
            .map(|value| resolve_path(base_dir, value));

        services.push(ServiceConfig {
            route_prefix: raw_service.route_prefix,
            backend_host: DEFAULT_BACKEND_HOST.to_owned(),
            backend_port: raw_service.port,
            strip_prefix: raw_service.strip_prefix,
            command: raw_service.command,
            args: raw_service.args,
            environment: raw_service.environment,
            working_directory,
            startup_timeout_ms: raw_service.startup_timeout_ms,
            idle_timeout_secs: raw_service.idle_timeout_secs,
            health_path: raw_service.health_path,
        });
    }

    Ok(services)
}

fn validate_route_prefix(
    path: &Path,
    route_prefix: &str,
    seen_route_prefixes: &mut HashSet<String>,
) -> Result<(), ConfigError> {
    let trimmed = route_prefix.trim();
    if trimmed.is_empty() {
        return Err(ConfigError::InvalidService {
            path: path.to_path_buf(),
            message: "route_prefix must not be empty".to_owned(),
        });
    }

    if trimmed.contains('/') {
        return Err(ConfigError::InvalidService {
            path: path.to_path_buf(),
            message: format!("route_prefix `{trimmed}` must not contain `/`"),
        });
    }

    if !seen_route_prefixes.insert(trimmed.to_owned()) {
        return Err(ConfigError::DuplicateRoutePrefix {
            path: path.to_path_buf(),
            route_prefix: trimmed.to_owned(),
        });
    }

    Ok(())
}

fn validate_port(path: &Path, port: u16, seen_ports: &mut HashSet<u16>) -> Result<(), ConfigError> {
    if port == 0 {
        return Err(ConfigError::InvalidService {
            path: path.to_path_buf(),
            message: "service port must be greater than 0".to_owned(),
        });
    }

    if !seen_ports.insert(port) {
        return Err(ConfigError::DuplicatePort {
            path: path.to_path_buf(),
            port,
        });
    }

    Ok(())
}

fn validate_command(path: &Path, command: &str) -> Result<(), ConfigError> {
    if command.trim().is_empty() {
        return Err(ConfigError::InvalidService {
            path: path.to_path_buf(),
            message: "command must not be empty".to_owned(),
        });
    }

    Ok(())
}

fn validate_timeouts(
    path: &Path,
    route_prefix: &str,
    startup_timeout_ms: u64,
    idle_timeout_secs: u64,
) -> Result<(), ConfigError> {
    if startup_timeout_ms == 0 {
        return Err(ConfigError::InvalidService {
            path: path.to_path_buf(),
            message: format!("service `{route_prefix}` must have startup_timeout_ms > 0"),
        });
    }

    if idle_timeout_secs == 0 {
        return Err(ConfigError::InvalidService {
            path: path.to_path_buf(),
            message: format!("service `{route_prefix}` must have idle_timeout_secs > 0"),
        });
    }

    Ok(())
}

fn resolve_path(base_dir: &Path, candidate: PathBuf) -> PathBuf {
    if candidate.is_absolute() {
        candidate
    } else {
        base_dir.join(candidate)
    }
}

fn default_strip_prefix() -> bool {
    true
}

#[derive(Debug)]
pub enum ConfigError {
    ReadFailed {
        path: PathBuf,
        source: std::io::Error,
    },
    ParseFailed {
        path: PathBuf,
        source: toml::de::Error,
    },
    NoServices {
        path: PathBuf,
    },
    DuplicateRoutePrefix {
        path: PathBuf,
        route_prefix: String,
    },
    DuplicatePort {
        path: PathBuf,
        port: u16,
    },
    InvalidService {
        path: PathBuf,
        message: String,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadFailed { path, source } => {
                write!(
                    f,
                    "failed to read config file `{}`: {source}",
                    path.display()
                )
            }
            Self::ParseFailed { path, source } => {
                write!(
                    f,
                    "failed to parse config file `{}`: {source}",
                    path.display()
                )
            }
            Self::NoServices { path } => {
                write!(
                    f,
                    "config file `{}` does not define any services",
                    path.display()
                )
            }
            Self::DuplicateRoutePrefix { path, route_prefix } => write!(
                f,
                "config file `{}` defines route_prefix `{route_prefix}` more than once",
                path.display()
            ),
            Self::DuplicatePort { path, port } => write!(
                f,
                "config file `{}` defines backend port `{port}` more than once",
                path.display()
            ),
            Self::InvalidService { path, message } => {
                write!(f, "invalid service in `{}`: {message}", path.display())
            }
        }
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::load_registry_from_path;

    #[test]
    fn loads_services_from_toml_and_resolves_relative_paths() {
        let config_dir = unique_temp_dir("activator_config_load");
        fs::create_dir_all(config_dir.join("nested")).unwrap();

        let config_path = config_dir.join("nested/services.toml");
        fs::write(
            &config_path,
            r#"
[[service]]
route_prefix = "api"
command = "cargo"
args = ["run"]
port = 9001
startup_timeout_ms = 4000
idle_timeout_secs = 120
health_path = "/health"
working_directory = "../backend"
"#,
        )
        .unwrap();

        let registry = load_registry_from_path(&config_path).unwrap();
        let resolved = registry.resolve("/api/demo").unwrap();

        assert_eq!(resolved.service.config.backend_port, 9001);
        assert_eq!(
            resolved.service.config.working_directory.as_deref(),
            Some(config_dir.join("nested/../backend").as_path())
        );

        let _ = fs::remove_dir_all(config_dir);
    }

    #[test]
    fn rejects_duplicate_route_prefixes() {
        let config_path = write_config_file(
            "activator_duplicate_route",
            r#"
[[service]]
route_prefix = "api"
command = "cargo"
port = 9001
startup_timeout_ms = 4000
idle_timeout_secs = 120
health_path = "/health"

[[service]]
route_prefix = "api"
command = "cargo"
port = 9002
startup_timeout_ms = 4000
idle_timeout_secs = 120
health_path = "/health"
"#,
        );

        let error = load_registry_from_path(&config_path).err().unwrap();
        assert!(error.to_string().contains("route_prefix `api`"));

        cleanup_config_file(&config_path);
    }

    #[test]
    fn rejects_duplicate_ports() {
        let config_path = write_config_file(
            "activator_duplicate_port",
            r#"
[[service]]
route_prefix = "api"
command = "cargo"
port = 9001
startup_timeout_ms = 4000
idle_timeout_secs = 120
health_path = "/health"

[[service]]
route_prefix = "media"
command = "cargo"
port = 9001
startup_timeout_ms = 4000
idle_timeout_secs = 120
health_path = "/health"
"#,
        );

        let error = load_registry_from_path(&config_path).err().unwrap();
        assert!(error.to_string().contains("backend port `9001`"));

        cleanup_config_file(&config_path);
    }

    fn write_config_file(prefix: &str, content: &str) -> PathBuf {
        let dir = unique_temp_dir(prefix);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("services.toml");
        fs::write(&path, content).unwrap();
        path
    }

    fn cleanup_config_file(path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}_{timestamp}"))
    }
}
