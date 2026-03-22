use std::{
    collections::HashMap,
    fmt, fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Multipart, State},
    http::{Method, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use rusqlite::types::ValueRef;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tower_http::cors::{Any, CorsLayer};

use crate::{
    app::AppState,
    config::{self, ConfigError},
    registry::{ServiceEntry, ServiceRegistry},
};

const MAX_UPLOAD_SIZE_BYTES: usize = 512 * 1024 * 1024;

pub fn build_admin_router() -> Router<std::sync::Arc<AppState>> {
    Router::new()
        .route("/overview", get(overview))
        .route("/upload", post(upload_binary))
        .route("/sql", post(execute_sql))
        .layer(DefaultBodyLimit::max(MAX_UPLOAD_SIZE_BYTES))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods([Method::GET, Method::POST])
                .allow_headers(Any),
        )
}

async fn overview(
    State(state): State<std::sync::Arc<AppState>>,
) -> Result<Json<OverviewResponse>, AdminError> {
    let registry = state.registry_snapshot().await;
    let mut services = Vec::new();
    for service in registry.all_services() {
        services.push(snapshot_service(&service).await?);
    }
    services.sort_by(|left, right| left.route_prefix.cmp(&right.route_prefix));

    let binaries = read_uploaded_binaries(&state.upload_dir)?;

    Ok(Json(OverviewResponse {
        config_path: state.config_path.display().to_string(),
        upload_dir: state.upload_dir.display().to_string(),
        services,
        binaries,
    }))
}

async fn upload_binary(
    State(state): State<std::sync::Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<Json<UploadResponse>, AdminError> {
    let mut uploaded = Vec::new();
    fs::create_dir_all(&state.upload_dir).map_err(|source| AdminError::Io {
        action: "create upload directory",
        path: state.upload_dir.clone(),
        source,
    })?;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|source| AdminError::Multipart(source.to_string()))?
    {
        let Some(file_name) = field.file_name().map(ToOwned::to_owned) else {
            continue;
        };

        let bytes = field
            .bytes()
            .await
            .map_err(|source| AdminError::Multipart(source.to_string()))?;
        let sanitized_name = sanitize_filename(&file_name);
        let stored_path = unique_upload_path(&state.upload_dir, &sanitized_name);

        fs::write(&stored_path, &bytes).map_err(|source| AdminError::Io {
            action: "write uploaded binary",
            path: stored_path.clone(),
            source,
        })?;
        make_binary_executable(&stored_path).map_err(|source| AdminError::Io {
            action: "mark binary executable",
            path: stored_path.clone(),
            source,
        })?;

        let absolute_path = fs::canonicalize(&stored_path).unwrap_or(stored_path.clone());
        uploaded.push(UploadedBinaryResponse {
            original_name: file_name,
            stored_name: sanitized_name,
            stored_path: absolute_path.display().to_string(),
            size_bytes: bytes.len() as u64,
            sql_template: build_sql_template(&absolute_path),
        });
    }

    if uploaded.is_empty() {
        return Err(AdminError::BadRequest(
            "attach at least one file in the multipart form".to_owned(),
        ));
    }

    Ok(Json(UploadResponse { uploaded }))
}

async fn execute_sql(
    State(state): State<std::sync::Arc<AppState>>,
    Json(request): Json<SqlRequest>,
) -> Result<Json<SqlResponse>, AdminError> {
    let sql = request.sql.trim();
    if sql.is_empty() {
        return Err(AdminError::BadRequest(
            "SQL input is empty; provide exactly one statement".to_owned(),
        ));
    }

    let execution = run_sql_statement(&state.config_path, sql)?;
    if let Some(registry) = execution.registry {
        state.replace_registry(registry).await;
    }

    Ok(Json(execution.response))
}

async fn snapshot_service(service: &std::sync::Arc<ServiceEntry>) -> Result<ServiceStatusResponse, AdminError> {
    let mut runtime = service.runtime.lock().await;
    let running = match runtime.process.as_mut() {
        Some(child) => match child.try_wait() {
            Ok(None) => true,
            Ok(Some(_)) => {
                runtime.process = None;
                false
            }
            Err(error) => {
                return Err(AdminError::RuntimeInspection {
                    route_prefix: service.config.route_prefix.clone(),
                    details: error.to_string(),
                });
            }
        },
        None => false,
    };

    Ok(ServiceStatusResponse {
        route_prefix: service.config.route_prefix.clone(),
        backend_port: service.config.backend_port,
        strip_prefix: service.config.strip_prefix,
        command: service.config.command.clone(),
        args: service.config.args.clone(),
        environment: service.config.environment.clone(),
        working_directory: service
            .config
            .working_directory
            .as_ref()
            .map(|value| value.display().to_string()),
        startup_timeout_ms: service.config.startup_timeout_ms,
        idle_timeout_secs: service.config.idle_timeout_secs,
        health_path: service.config.health_path.clone(),
        backend_base_url: service.config.backend_base_url(),
        running,
        startup_in_progress: runtime.startup_in_progress,
        last_used_ms_ago: runtime.last_used.elapsed().as_millis() as u64,
        last_startup_error: runtime
            .last_startup_error
            .as_ref()
            .map(ToString::to_string),
    })
}

fn run_sql_statement(path: &Path, sql: &str) -> Result<SqlExecution, AdminError> {
    let mut connection = config::open_database_connection(path)?;
    let tx = connection
        .transaction()
        .map_err(|source| AdminError::DatabaseOperation {
            operation: "start SQL transaction",
            source,
        })?;

    let mut statement = tx
        .prepare(sql)
        .map_err(|source| AdminError::DatabaseOperation {
            operation: "prepare SQL statement",
            source,
        })?;

    if statement.readonly() {
        let response = query_statement(&mut statement)?;
        drop(statement);
        tx.commit().map_err(|source| AdminError::DatabaseOperation {
            operation: "commit read-only transaction",
            source,
        })?;

        return Ok(SqlExecution {
            response,
            registry: None,
        });
    }

    let rows_affected = statement
        .execute([])
        .map_err(|source| AdminError::DatabaseOperation {
            operation: "execute SQL statement",
            source,
        })?;
    drop(statement);

    let registry = config::load_registry_from_connection(&tx, path)?;
    tx.commit().map_err(|source| AdminError::DatabaseOperation {
        operation: "commit SQL transaction",
        source,
    })?;

    Ok(SqlExecution {
        response: SqlResponse {
            kind: "execute",
            columns: Vec::new(),
            rows: Vec::new(),
            row_count: 0,
            rows_affected,
            registry_reloaded: true,
        },
        registry: Some(registry),
    })
}

fn query_statement(statement: &mut rusqlite::Statement<'_>) -> Result<SqlResponse, AdminError> {
    let columns = statement
        .column_names()
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let column_count = statement.column_count();
    let mut query = statement
        .query([])
        .map_err(|source| AdminError::DatabaseOperation {
            operation: "run SQL query",
            source,
        })?;

    let mut rows = Vec::new();
    while let Some(row) = query.next().map_err(|source| AdminError::DatabaseOperation {
        operation: "read SQL row",
        source,
    })? {
        let mut values = Vec::with_capacity(column_count);
        for index in 0..column_count {
            let value = row
                .get_ref(index)
                .map(value_ref_to_json)
                .map_err(|source| AdminError::DatabaseOperation {
                    operation: "decode SQL value",
                    source,
                })?;
            values.push(value);
        }
        rows.push(values);
    }

    Ok(SqlResponse {
        kind: "query",
        columns,
        row_count: rows.len(),
        rows,
        rows_affected: 0,
        registry_reloaded: false,
    })
}

fn value_ref_to_json(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(value) => Value::from(value),
        ValueRef::Real(value) => Value::from(value),
        ValueRef::Text(value) => Value::from(String::from_utf8_lossy(value).into_owned()),
        ValueRef::Blob(value) => Value::from(format!("<{} bytes>", value.len())),
    }
}

fn read_uploaded_binaries(upload_dir: &Path) -> Result<Vec<BinaryAssetResponse>, AdminError> {
    fs::create_dir_all(upload_dir).map_err(|source| AdminError::Io {
        action: "create upload directory",
        path: upload_dir.to_path_buf(),
        source,
    })?;

    let mut binaries = Vec::new();
    for entry in fs::read_dir(upload_dir).map_err(|source| AdminError::Io {
        action: "read upload directory",
        path: upload_dir.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| AdminError::Io {
            action: "read upload entry",
            path: upload_dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let metadata = entry.metadata().map_err(|source| AdminError::Io {
            action: "read upload metadata",
            path: path.clone(),
            source,
        })?;
        let modified_unix_ms = metadata
            .modified()
            .ok()
            .and_then(system_time_to_unix_ms);

        binaries.push(BinaryAssetResponse {
            name: entry.file_name().to_string_lossy().into_owned(),
            stored_path: fs::canonicalize(&path)
                .unwrap_or(path.clone())
                .display()
                .to_string(),
            size_bytes: metadata.len(),
            modified_unix_ms,
        });
    }

    binaries.sort_by(|left, right| right.modified_unix_ms.cmp(&left.modified_unix_ms));
    Ok(binaries)
}

fn system_time_to_unix_ms(value: SystemTime) -> Option<u64> {
    value
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis() as u64)
}

fn sanitize_filename(original: &str) -> String {
    let file_name = original.rsplit(['/', '\\']).next().unwrap_or("binary.bin");
    let sanitized = file_name
        .chars()
        .map(|value| {
            if value.is_ascii_alphanumeric() || matches!(value, '.' | '-' | '_') {
                value
            } else {
                '_'
            }
        })
        .collect::<String>();

    let sanitized = sanitized.trim_matches('_').to_owned();
    if sanitized.is_empty() {
        "binary.bin".to_owned()
    } else {
        sanitized
    }
}

fn unique_upload_path(upload_dir: &Path, file_name: &str) -> PathBuf {
    let candidate = upload_dir.join(file_name);
    if !candidate.exists() {
        return candidate;
    }

    let stem = Path::new(file_name)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("binary");
    let extension = Path::new(file_name)
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{value}"))
        .unwrap_or_default();

    for index in 1.. {
        let candidate = upload_dir.join(format!("{stem}-{index}{extension}"));
        if !candidate.exists() {
            return candidate;
        }
    }

    unreachable!("upload filename search should eventually find a free name");
}

#[cfg(unix)]
fn make_binary_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
}

#[cfg(not(unix))]
fn make_binary_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn build_sql_template(binary_path: &Path) -> String {
    let file_stem = binary_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("service");
    let suggested_prefix = file_stem
        .chars()
        .map(|value| {
            if value.is_ascii_alphanumeric() {
                value.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_owned();
    let suggested_prefix = if suggested_prefix.is_empty() {
        "service".to_owned()
    } else {
        suggested_prefix
    };
    let binary_path = binary_path.display().to_string().replace('\'', "''");

    format!(
        "-- edit route_prefix and backend_port before running\n\
INSERT INTO services (\n\
    route_prefix,\n\
    command,\n\
    args_json,\n\
    backend_port,\n\
    strip_prefix,\n\
    environment_json,\n\
    working_directory,\n\
    startup_timeout_ms,\n\
    idle_timeout_secs,\n\
    health_path\n\
) VALUES (\n\
    '{suggested_prefix}',\n\
    '{binary_path}',\n\
    '[]',\n\
    9100,\n\
    1,\n\
    '{{}}',\n\
    NULL,\n\
    15000,\n\
    120,\n\
    '/health'\n\
);"
    )
}

#[derive(Serialize)]
struct OverviewResponse {
    config_path: String,
    upload_dir: String,
    services: Vec<ServiceStatusResponse>,
    binaries: Vec<BinaryAssetResponse>,
}

#[derive(Serialize)]
struct ServiceStatusResponse {
    route_prefix: String,
    backend_port: u16,
    strip_prefix: bool,
    command: String,
    args: Vec<String>,
    environment: HashMap<String, String>,
    working_directory: Option<String>,
    startup_timeout_ms: u64,
    idle_timeout_secs: u64,
    health_path: String,
    backend_base_url: String,
    running: bool,
    startup_in_progress: bool,
    last_used_ms_ago: u64,
    last_startup_error: Option<String>,
}

#[derive(Serialize)]
struct BinaryAssetResponse {
    name: String,
    stored_path: String,
    size_bytes: u64,
    modified_unix_ms: Option<u64>,
}

#[derive(Serialize)]
struct UploadResponse {
    uploaded: Vec<UploadedBinaryResponse>,
}

#[derive(Serialize)]
struct UploadedBinaryResponse {
    original_name: String,
    stored_name: String,
    stored_path: String,
    size_bytes: u64,
    sql_template: String,
}

#[derive(Deserialize)]
struct SqlRequest {
    sql: String,
}

#[derive(Serialize)]
struct SqlResponse {
    kind: &'static str,
    columns: Vec<String>,
    rows: Vec<Vec<Value>>,
    row_count: usize,
    rows_affected: usize,
    registry_reloaded: bool,
}

struct SqlExecution {
    response: SqlResponse,
    registry: Option<ServiceRegistry>,
}

#[derive(Debug)]
enum AdminError {
    BadRequest(String),
    Config(ConfigError),
    DatabaseOperation {
        operation: &'static str,
        source: rusqlite::Error,
    },
    Io {
        action: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    Multipart(String),
    RuntimeInspection {
        route_prefix: String,
        details: String,
    },
}

impl From<ConfigError> for AdminError {
    fn from(value: ConfigError) -> Self {
        Self::Config(value)
    }
}

impl fmt::Display for AdminError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadRequest(message) => write!(f, "{message}"),
            Self::Config(error) => write!(f, "{error}"),
            Self::DatabaseOperation { operation, source } => {
                write!(f, "failed to {operation}: {source}")
            }
            Self::Io {
                action,
                path,
                source,
            } => {
                write!(f, "failed to {action} at `{}`: {source}", path.display())
            }
            Self::Multipart(details) => write!(f, "invalid multipart payload: {details}"),
            Self::RuntimeInspection {
                route_prefix,
                details,
            } => {
                write!(f, "failed to inspect runtime for `{route_prefix}`: {details}")
            }
        }
    }
}

impl std::error::Error for AdminError {}

impl IntoResponse for AdminError {
    fn into_response(self) -> axum::response::Response {
        let status = match self {
            Self::BadRequest(_) | Self::Config(_) | Self::Multipart(_) => StatusCode::BAD_REQUEST,
            Self::DatabaseOperation { .. }
            | Self::Io { .. }
            | Self::RuntimeInspection { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        };

        tracing::warn!(status = %status, error = %self, "admin request failed");
        (
            status,
            Json(ErrorResponse {
                error: self.to_string(),
            }),
        )
            .into_response()
    }
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        fs,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    use rusqlite::params;
    use serde_json::Value;

    use super::{build_sql_template, read_uploaded_binaries, run_sql_statement};
    use crate::config;

    #[test]
    fn query_sql_returns_rows_from_services_table() {
        let dir = unique_temp_dir("activator_admin_query");
        let db_path = dir.join("services.db");
        write_service_db(&db_path, &[("api", 9001)]);

        let result = run_sql_statement(
            &db_path,
            "SELECT route_prefix, backend_port FROM services ORDER BY route_prefix;",
        )
        .unwrap();

        assert_eq!(result.response.kind, "query");
        assert_eq!(result.response.row_count, 1);
        assert_eq!(result.response.rows[0][0], Value::String("api".to_owned()));
        assert_eq!(result.response.rows[0][1], Value::from(9001));

        cleanup_dir(&dir);
    }

    #[test]
    fn write_sql_commits_and_returns_reloaded_registry() {
        let dir = unique_temp_dir("activator_admin_reload");
        let db_path = dir.join("services.db");
        write_service_db(&db_path, &[("api", 9001)]);

        let result = run_sql_statement(
            &db_path,
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
            ) VALUES (
                'admin',
                'cargo',
                '[\"run\"]',
                9002,
                1,
                '{}',
                NULL,
                15000,
                120,
                '/health'
            )",
        )
        .unwrap();

        assert_eq!(result.response.kind, "execute");
        assert!(result.response.registry_reloaded);

        let registry = result.registry.expect("write statements should return a registry");
        assert!(registry.resolve("/admin/health").is_some());
        assert!(config::load_registry_from_path(&db_path)
            .unwrap()
            .resolve("/admin/health")
            .is_some());

        cleanup_dir(&dir);
    }

    #[test]
    fn uploaded_binaries_are_listed_and_template_uses_absolute_path() {
        let dir = unique_temp_dir("activator_admin_uploads");
        let uploads_dir = dir.join("uploads");
        fs::create_dir_all(&uploads_dir).unwrap();
        let binary_path = uploads_dir.join("demo-tool.exe");
        fs::write(&binary_path, b"demo").unwrap();

        let binaries = read_uploaded_binaries(&uploads_dir).unwrap();
        assert_eq!(binaries.len(), 1);
        assert_eq!(binaries[0].name, "demo-tool.exe");
        assert!(Path::new(&binaries[0].stored_path).is_absolute());

        let template = build_sql_template(Path::new(&binaries[0].stored_path));
        assert!(template.contains("demo_tool"));
        assert!(template.contains(&binaries[0].stored_path.replace('\'', "''")));

        cleanup_dir(&dir);
    }

    fn write_service_db(path: &Path, services: &[(&str, u16)]) {
        let _ = config::load_registry_from_path(path);
        let connection = config::open_database_connection(path).unwrap();
        for (route_prefix, port) in services {
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
                        serde_json::to_string(&vec!["run"]).unwrap(),
                        port,
                        true,
                        serde_json::to_string(&HashMap::<String, String>::new()).unwrap(),
                        Option::<String>::None,
                        15000,
                        120,
                        "/health",
                    ],
                )
                .unwrap();
        }

        config::load_registry_from_connection(&connection, path).unwrap();
    }

    fn cleanup_dir(path: &Path) {
        let _ = fs::remove_dir_all(path);
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}_{timestamp}"))
    }
}
