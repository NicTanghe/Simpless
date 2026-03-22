use std::{
    collections::{HashMap, HashSet},
    fmt, fs,
    path::{Path, PathBuf},
};

use rusqlite::Connection;

use crate::registry::{ServiceConfig, ServiceRegistry};

const DEFAULT_BACKEND_HOST: &str = "127.0.0.1";
const CREATE_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS services (
    route_prefix TEXT PRIMARY KEY
        CHECK (trim(route_prefix) <> '' AND instr(route_prefix, '/') = 0),
    command TEXT NOT NULL
        CHECK (trim(command) <> ''),
    args_json TEXT NOT NULL DEFAULT '[]',
    backend_port INTEGER NOT NULL UNIQUE
        CHECK (backend_port > 0 AND backend_port <= 65535),
    strip_prefix INTEGER NOT NULL DEFAULT 1
        CHECK (strip_prefix IN (0, 1)),
    environment_json TEXT NOT NULL DEFAULT '{}',
    working_directory TEXT,
    startup_timeout_ms INTEGER NOT NULL
        CHECK (startup_timeout_ms > 0),
    idle_timeout_secs INTEGER NOT NULL
        CHECK (idle_timeout_secs > 0),
    health_path TEXT NOT NULL
        CHECK (trim(health_path) <> '')
);
"#;

#[derive(Debug, Clone)]
struct RawServiceConfig {
    route_prefix: String,
    command: String,
    args: Vec<String>,
    port: u16,
    strip_prefix: bool,
    environment: HashMap<String, String>,
    working_directory: Option<PathBuf>,
    startup_timeout_ms: u64,
    idle_timeout_secs: u64,
    health_path: String,
}

#[derive(Debug)]
struct DbServiceRow {
    route_prefix: String,
    command: String,
    args_json: String,
    port: u16,
    strip_prefix: bool,
    environment_json: String,
    working_directory: Option<String>,
    startup_timeout_ms: u64,
    idle_timeout_secs: u64,
    health_path: String,
}

pub fn load_registry_from_path(path: &Path) -> Result<ServiceRegistry, ConfigError> {
    reject_legacy_toml_path(path)?;
    let connection = open_database_connection(path)?;
    load_registry_from_connection(&connection, path)
}

pub fn default_config_path() -> PathBuf {
    PathBuf::from("config/services.db")
}

pub fn open_database_connection(path: &Path) -> Result<Connection, ConfigError> {
    reject_legacy_toml_path(path)?;
    ensure_parent_dir(path)?;

    Connection::open(path).map_err(|source| ConfigError::OpenFailed {
        path: path.to_path_buf(),
        source,
    })
}

pub fn load_registry_from_connection(
    connection: &Connection,
    path: &Path,
) -> Result<ServiceRegistry, ConfigError> {
    initialize_schema(connection, path)?;

    let services = load_raw_services_from_db(connection, path)?;
    let services = build_service_configs(services, path)?;
    Ok(ServiceRegistry::from_services(services))
}

fn reject_legacy_toml_path(path: &Path) -> Result<(), ConfigError> {
    let is_toml = path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("toml"));

    if is_toml {
        return Err(ConfigError::UnsupportedConfigPath {
            path: path.to_path_buf(),
        });
    }

    Ok(())
}

fn ensure_parent_dir(path: &Path) -> Result<(), ConfigError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| ConfigError::CreateDirectoryFailed {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    Ok(())
}

fn initialize_schema(connection: &Connection, path: &Path) -> Result<(), ConfigError> {
    connection
        .execute_batch(CREATE_SCHEMA_SQL)
        .map_err(|source| ConfigError::DatabaseFailed {
            path: path.to_path_buf(),
            operation: "initialize schema",
            source,
        })
}

fn load_raw_services_from_db(
    connection: &Connection,
    path: &Path,
) -> Result<Vec<RawServiceConfig>, ConfigError> {
    let mut statement = connection
        .prepare(
            "SELECT
                route_prefix,
                command,
                args_json,
                backend_port,
                strip_prefix,
                environment_json,
                working_directory,
                startup_timeout_ms,
                idle_timeout_secs,
                health_path
             FROM services
             ORDER BY route_prefix",
        )
        .map_err(|source| ConfigError::DatabaseFailed {
            path: path.to_path_buf(),
            operation: "prepare service query",
            source,
        })?;

    let rows = statement
        .query_map([], |row| {
            Ok(DbServiceRow {
                route_prefix: row.get(0)?,
                command: row.get(1)?,
                args_json: row.get(2)?,
                port: row.get(3)?,
                strip_prefix: row.get(4)?,
                environment_json: row.get(5)?,
                working_directory: row.get(6)?,
                startup_timeout_ms: row.get(7)?,
                idle_timeout_secs: row.get(8)?,
                health_path: row.get(9)?,
            })
        })
        .map_err(|source| ConfigError::DatabaseFailed {
            path: path.to_path_buf(),
            operation: "query configured services",
            source,
        })?;

    let mut services = Vec::new();
    for row in rows {
        let row = row.map_err(|source| ConfigError::DatabaseFailed {
            path: path.to_path_buf(),
            operation: "read configured service",
            source,
        })?;

        let args = serde_json::from_str(&row.args_json).map_err(|source| {
            ConfigError::InvalidStoredJson {
                path: path.to_path_buf(),
                route_prefix: row.route_prefix.clone(),
                field: "args_json",
                source,
            }
        })?;
        let environment = serde_json::from_str(&row.environment_json).map_err(|source| {
            ConfigError::InvalidStoredJson {
                path: path.to_path_buf(),
                route_prefix: row.route_prefix.clone(),
                field: "environment_json",
                source,
            }
        })?;

        services.push(RawServiceConfig {
            route_prefix: row.route_prefix,
            command: row.command,
            args,
            port: row.port,
            strip_prefix: row.strip_prefix,
            environment,
            working_directory: row.working_directory.map(PathBuf::from),
            startup_timeout_ms: row.startup_timeout_ms,
            idle_timeout_secs: row.idle_timeout_secs,
            health_path: row.health_path,
        });
    }

    Ok(services)
}

fn build_service_configs(
    parsed_services: Vec<RawServiceConfig>,
    path: &Path,
) -> Result<Vec<ServiceConfig>, ConfigError> {
    if parsed_services.is_empty() {
        return Err(ConfigError::NoServices {
            path: path.to_path_buf(),
        });
    }

    let mut seen_route_prefixes = HashSet::new();
    let mut seen_ports = HashSet::new();
    let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut services = Vec::with_capacity(parsed_services.len());

    for raw_service in parsed_services {
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

#[derive(Debug)]
pub enum ConfigError {
    UnsupportedConfigPath {
        path: PathBuf,
    },
    CreateDirectoryFailed {
        path: PathBuf,
        source: std::io::Error,
    },
    OpenFailed {
        path: PathBuf,
        source: rusqlite::Error,
    },
    DatabaseFailed {
        path: PathBuf,
        operation: &'static str,
        source: rusqlite::Error,
    },
    InvalidStoredJson {
        path: PathBuf,
        route_prefix: String,
        field: &'static str,
        source: serde_json::Error,
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
            Self::UnsupportedConfigPath { path } => write!(
                f,
                "config path `{}` is not supported; use a SQLite database file instead of TOML",
                path.display()
            ),
            Self::CreateDirectoryFailed { path, source } => {
                write!(
                    f,
                    "failed to create config directory `{}`: {source}",
                    path.display()
                )
            }
            Self::OpenFailed { path, source } => {
                write!(
                    f,
                    "failed to open config database `{}`: {source}",
                    path.display()
                )
            }
            Self::DatabaseFailed {
                path,
                operation,
                source,
            } => {
                write!(
                    f,
                    "failed to {operation} in config database `{}`: {source}",
                    path.display()
                )
            }
            Self::InvalidStoredJson {
                path,
                route_prefix,
                field,
                source,
            } => {
                write!(
                    f,
                    "config database `{}` contains invalid `{field}` for service `{route_prefix}`: {source}",
                    path.display()
                )
            }
            Self::NoServices { path } => {
                write!(
                    f,
                    "config source `{}` does not define any services",
                    path.display()
                )
            }
            Self::DuplicateRoutePrefix { path, route_prefix } => write!(
                f,
                "config source `{}` defines route_prefix `{route_prefix}` more than once",
                path.display()
            ),
            Self::DuplicatePort { path, port } => write!(
                f,
                "config source `{}` defines backend port `{port}` more than once",
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
        collections::HashMap,
        fs,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    use rusqlite::{Connection, params};

    use super::{RawServiceConfig, initialize_schema, load_registry_from_path};

    #[test]
    fn loads_services_from_sqlite_and_resolves_relative_paths() {
        let config_dir = unique_temp_dir("activator_config_load");
        let db_path = config_dir.join("nested/services.db");
        write_database_file(
            &db_path,
            vec![RawServiceConfig {
                route_prefix: "api".to_owned(),
                command: "cargo".to_owned(),
                args: vec!["run".to_owned()],
                port: 9001,
                strip_prefix: true,
                environment: HashMap::new(),
                working_directory: Some(PathBuf::from("../backend")),
                startup_timeout_ms: 4000,
                idle_timeout_secs: 120,
                health_path: "/health".to_owned(),
            }],
        );

        let registry = load_registry_from_path(&db_path).unwrap();
        let resolved = registry.resolve("/api/demo").unwrap();

        assert_eq!(resolved.service.config.backend_port, 9001);
        assert_eq!(
            resolved.service.config.working_directory.as_deref(),
            Some(config_dir.join("nested/../backend").as_path())
        );

        cleanup_path(&db_path);
    }

    #[test]
    fn rejects_empty_database() {
        let db_path = unique_temp_dir("activator_empty_db").join("services.db");

        let error = load_registry_from_path(&db_path).err().unwrap();

        assert!(error.to_string().contains("does not define any services"));
        cleanup_path(&db_path);
    }

    #[test]
    fn rejects_toml_config_paths() {
        let toml_path = unique_temp_dir("activator_toml_path").join("services.toml");

        let error = load_registry_from_path(&toml_path).err().unwrap();

        assert!(error.to_string().contains("instead of TOML"));
        cleanup_path(&toml_path);
    }

    #[test]
    fn rejects_duplicate_route_prefixes_from_database_rows() {
        let db_path = unique_temp_dir("activator_duplicate_route").join("services.db");
        let connection = initialize_non_unique_database(&db_path);

        insert_raw_row(&connection, "api", 9001);
        insert_raw_row(&connection, "api", 9002);

        let error = load_registry_from_path(&db_path).err().unwrap();
        assert!(error.to_string().contains("route_prefix `api`"));

        cleanup_path(&db_path);
    }

    #[test]
    fn rejects_duplicate_ports_from_database_rows() {
        let db_path = unique_temp_dir("activator_duplicate_port").join("services.db");
        let connection = initialize_non_unique_database(&db_path);

        insert_raw_row(&connection, "api", 9001);
        insert_raw_row(&connection, "media", 9001);

        let error = load_registry_from_path(&db_path).err().unwrap();
        assert!(error.to_string().contains("backend port `9001`"));

        cleanup_path(&db_path);
    }

    #[test]
    fn rejects_invalid_json_stored_in_database() {
        let db_path = unique_temp_dir("activator_invalid_json").join("services.db");
        fs::create_dir_all(db_path.parent().unwrap()).unwrap();

        let connection = Connection::open(&db_path).unwrap();
        initialize_schema(&connection, &db_path).unwrap();
        connection
            .execute(
                "INSERT INTO services (
                    route_prefix,
                    command,
                    args_json,
                    backend_port,
                    strip_prefix,
                    environment_json,
                    working_directory,
                    startup_timeout_ms,
                    idle_timeout_secs,
                    health_path
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    "api",
                    "cargo",
                    "not json",
                    9001,
                    true,
                    "{}",
                    Option::<String>::None,
                    4000,
                    120,
                    "/health",
                ],
            )
            .unwrap();

        let error = load_registry_from_path(&db_path).err().unwrap();
        assert!(error.to_string().contains("args_json"));

        cleanup_path(&db_path);
    }

    fn write_database_file(path: &Path, services: Vec<RawServiceConfig>) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut connection = Connection::open(path).unwrap();
        initialize_schema(&connection, path).unwrap();
        insert_services(&mut connection, services);
    }

    fn initialize_non_unique_database(path: &Path) -> Connection {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE services (
                    route_prefix TEXT NOT NULL,
                    command TEXT NOT NULL,
                    args_json TEXT NOT NULL DEFAULT '[]',
                    backend_port INTEGER NOT NULL,
                    strip_prefix INTEGER NOT NULL DEFAULT 1,
                    environment_json TEXT NOT NULL DEFAULT '{}',
                    working_directory TEXT,
                    startup_timeout_ms INTEGER NOT NULL,
                    idle_timeout_secs INTEGER NOT NULL,
                    health_path TEXT NOT NULL
                )",
            )
            .unwrap();
        connection
    }

    fn insert_raw_row(connection: &Connection, route_prefix: &str, backend_port: u16) {
        connection
            .execute(
                "INSERT INTO services (
                    route_prefix,
                    command,
                    args_json,
                    backend_port,
                    strip_prefix,
                    environment_json,
                    working_directory,
                    startup_timeout_ms,
                    idle_timeout_secs,
                    health_path
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    route_prefix,
                    "cargo",
                    "[]",
                    backend_port,
                    true,
                    "{}",
                    Option::<String>::None,
                    4000,
                    120,
                    "/health",
                ],
            )
            .unwrap();
    }

    fn insert_services(connection: &mut Connection, services: Vec<RawServiceConfig>) {
        let tx = connection.transaction().unwrap();
        let mut statement = tx
            .prepare(
                "INSERT INTO services (
                    route_prefix,
                    command,
                    args_json,
                    backend_port,
                    strip_prefix,
                    environment_json,
                    working_directory,
                    startup_timeout_ms,
                    idle_timeout_secs,
                    health_path
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .unwrap();

        for service in services {
            let working_directory = service
                .working_directory
                .as_ref()
                .map(|value| value.to_string_lossy().into_owned());

            statement
                .execute(params![
                    service.route_prefix,
                    service.command,
                    serde_json::to_string(&service.args).unwrap(),
                    service.port,
                    service.strip_prefix,
                    serde_json::to_string(&service.environment).unwrap(),
                    working_directory,
                    service.startup_timeout_ms,
                    service.idle_timeout_secs,
                    service.health_path,
                ])
                .unwrap();
        }

        drop(statement);
        tx.commit().unwrap();
    }

    fn cleanup_path(path: &Path) {
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
