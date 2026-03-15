use std::{
    fmt, io,
    process::Stdio,
    time::{Duration, Instant},
};

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use tokio::{process::Command, time::sleep};

use crate::{
    app::ProxyClient,
    registry::{ServiceEntry, ServiceRuntime},
};

pub async fn ensure_service_ready(
    service: &ServiceEntry,
    client: &ProxyClient,
) -> Result<(), StartupError> {
    if probe_backend_ready(service, client).await? {
        return Ok(());
    }

    loop {
        let notified = service.startup_notify.notified();
        let waiter_generation = {
            let mut runtime = service.runtime.lock().await;

            if runtime.startup_in_progress {
                Some(runtime.startup_generation)
            } else {
                runtime.startup_in_progress = true;
                runtime.startup_generation += 1;
                runtime.last_startup_generation = None;
                runtime.last_startup_error = None;
                None
            }
        };

        if let Some(generation) = waiter_generation {
            notified.await;

            let runtime = service.runtime.lock().await;
            if runtime.last_startup_generation == Some(generation) {
                if let Some(error) = &runtime.last_startup_error {
                    return Err(error.clone());
                }

                return Ok(());
            }

            continue;
        }

        let result = perform_startup(service, client).await;
        finish_startup_attempt(service, &result).await;
        return result;
    }
}

async fn perform_startup(service: &ServiceEntry, client: &ProxyClient) -> Result<(), StartupError> {
    spawn_service_if_needed(service).await?;
    wait_for_ready(service, client).await
}

async fn finish_startup_attempt(service: &ServiceEntry, result: &Result<(), StartupError>) {
    {
        let mut runtime = service.runtime.lock().await;
        runtime.startup_in_progress = false;
        runtime.last_startup_generation = Some(runtime.startup_generation);
        runtime.last_startup_error = result.as_ref().err().cloned();
    }

    service.startup_notify.notify_waiters();
}

async fn spawn_service_if_needed(service: &ServiceEntry) -> Result<(), StartupError> {
    let route_prefix = service.config.route_prefix.clone();
    let mut runtime = service.runtime.lock().await;

    if process_is_running(&mut runtime, &route_prefix)? {
        return Ok(());
    }

    let mut command = Command::new(&service.config.command);
    command.args(&service.config.args);
    command.envs(&service.config.environment);

    if let Some(working_directory) = &service.config.working_directory {
        command.current_dir(working_directory);
    }

    command
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);

    let child = command
        .spawn()
        .map_err(|source| StartupError::spawn_failed(route_prefix.clone(), source))?;

    tracing::info!(
        route_prefix = %service.config.route_prefix,
        command = %service.config.command,
        args = ?service.config.args,
        "started backend process"
    );

    runtime.process = Some(child);
    Ok(())
}

async fn wait_for_ready(service: &ServiceEntry, client: &ProxyClient) -> Result<(), StartupError> {
    let timeout = Duration::from_millis(service.config.startup_timeout_ms);
    let deadline = Instant::now() + timeout;

    loop {
        if probe_backend_ready(service, client).await? {
            tracing::info!(route_prefix = %service.config.route_prefix, "backend is ready");
            return Ok(());
        }

        {
            let route_prefix = service.config.route_prefix.clone();
            let mut runtime = service.runtime.lock().await;
            if process_exited(&mut runtime, &route_prefix)? {
                return Err(StartupError::ExitedBeforeReady { route_prefix });
            }
        }

        if Instant::now() >= deadline {
            stop_process(service).await?;
            return Err(StartupError::TimedOut {
                route_prefix: service.config.route_prefix.clone(),
                timeout_ms: timeout.as_millis() as u64,
            });
        }

        sleep(Duration::from_millis(100)).await;
    }
}

async fn probe_backend_ready(
    service: &ServiceEntry,
    client: &ProxyClient,
) -> Result<bool, StartupError> {
    let request = Request::builder()
        .method(Method::GET)
        .uri(
            service
                .config
                .healthcheck_uri()
                .map_err(StartupError::invalid_health_uri)?,
        )
        .body(Body::empty())
        .expect("health check request should be valid");

    match client.request(request).await {
        Ok(response) => Ok(response.status().is_success()),
        Err(_) => Ok(false),
    }
}

async fn stop_process(service: &ServiceEntry) -> Result<(), StartupError> {
    let mut child = {
        let mut runtime = service.runtime.lock().await;
        runtime.process.take()
    };

    if let Some(child) = child.as_mut() {
        child.start_kill().map_err(|source| {
            StartupError::stop_failed(service.config.route_prefix.clone(), source)
        })?;
        let _ = child.wait().await;
    }

    Ok(())
}

fn process_is_running(
    runtime: &mut ServiceRuntime,
    route_prefix: &str,
) -> Result<bool, StartupError> {
    let Some(child) = runtime.process.as_mut() else {
        return Ok(false);
    };

    match child
        .try_wait()
        .map_err(|source| StartupError::inspect_failed(route_prefix.to_owned(), source))?
    {
        None => Ok(true),
        Some(status) => {
            tracing::warn!(route_prefix, %status, "backend exited");
            runtime.process = None;
            Ok(false)
        }
    }
}

fn process_exited(runtime: &mut ServiceRuntime, route_prefix: &str) -> Result<bool, StartupError> {
    let Some(child) = runtime.process.as_mut() else {
        return Ok(false);
    };

    match child
        .try_wait()
        .map_err(|source| StartupError::inspect_failed(route_prefix.to_owned(), source))?
    {
        None => Ok(false),
        Some(status) => {
            tracing::warn!(route_prefix, %status, "backend exited before becoming ready");
            runtime.process = None;
            Ok(true)
        }
    }
}

#[derive(Clone, Debug)]
pub enum StartupError {
    InvalidHealthUri {
        details: String,
    },
    SpawnFailed {
        route_prefix: String,
        details: String,
    },
    InspectFailed {
        route_prefix: String,
        details: String,
    },
    ExitedBeforeReady {
        route_prefix: String,
    },
    TimedOut {
        route_prefix: String,
        timeout_ms: u64,
    },
    StopFailed {
        route_prefix: String,
        details: String,
    },
}

impl StartupError {
    pub fn status_code(&self) -> StatusCode {
        match self {
            Self::TimedOut { .. } => StatusCode::GATEWAY_TIMEOUT,
            _ => StatusCode::BAD_GATEWAY,
        }
    }

    fn invalid_health_uri(source: axum::http::uri::InvalidUri) -> Self {
        Self::InvalidHealthUri {
            details: source.to_string(),
        }
    }

    fn spawn_failed(route_prefix: String, source: io::Error) -> Self {
        Self::SpawnFailed {
            route_prefix,
            details: source.to_string(),
        }
    }

    fn inspect_failed(route_prefix: String, source: io::Error) -> Self {
        Self::InspectFailed {
            route_prefix,
            details: source.to_string(),
        }
    }

    fn stop_failed(route_prefix: String, source: io::Error) -> Self {
        Self::StopFailed {
            route_prefix,
            details: source.to_string(),
        }
    }
}

impl fmt::Display for StartupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidHealthUri { details } => {
                write!(f, "health check URI is invalid: {details}")
            }
            Self::SpawnFailed {
                route_prefix,
                details,
            } => write!(f, "failed to start `{route_prefix}` backend: {details}"),
            Self::InspectFailed {
                route_prefix,
                details,
            } => write!(f, "failed to inspect `{route_prefix}` backend: {details}"),
            Self::ExitedBeforeReady { route_prefix } => {
                write!(f, "`{route_prefix}` backend exited before it became ready")
            }
            Self::TimedOut {
                route_prefix,
                timeout_ms,
            } => write!(
                f,
                "`{route_prefix}` backend did not become ready within {timeout_ms} ms"
            ),
            Self::StopFailed {
                route_prefix,
                details,
            } => write!(
                f,
                "failed to stop `{route_prefix}` backend after timeout: {details}"
            ),
        }
    }
}

impl std::error::Error for StartupError {}
