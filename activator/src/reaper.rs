use std::time::Duration;

use tokio::{
    task::JoinHandle,
    time::{MissedTickBehavior, interval},
};

use std::sync::Arc;

use crate::{app::AppState, registry::ServiceEntry};

pub fn spawn_reaper(state: Arc<AppState>, sweep_interval: Duration) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = interval(sweep_interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            ticker.tick().await;
            sweep_once(&state).await;
        }
    })
}

pub async fn sweep_once(state: &Arc<AppState>) {
    let registry = state.registry_snapshot().await;
    for service in registry.all_services() {
        sweep_service(&service).await;
    }
}

async fn sweep_service(service: &ServiceEntry) {
    let idle_timeout = Duration::from_secs(service.config.idle_timeout_secs);
    let route_prefix = service.config.route_prefix.clone();
    let mut runtime = service.runtime.lock().await;

    if runtime.startup_in_progress {
        return;
    }

    let process_state = match runtime.process.as_mut() {
        Some(child) => child.try_wait(),
        None => return,
    };

    match process_state {
        Ok(Some(status)) => {
            tracing::info!(route_prefix = %route_prefix, %status, "cleaning up exited backend");
            runtime.process = None;
            return;
        }
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(route_prefix = %route_prefix, error = %error, "failed to inspect backend process");
            return;
        }
    }

    if runtime.last_used.elapsed() < idle_timeout {
        return;
    }

    tracing::info!(
        route_prefix = %route_prefix,
        idle_timeout_secs = service.config.idle_timeout_secs,
        "stopping idle backend"
    );

    let Some(child) = runtime.process.as_mut() else {
        return;
    };

    if let Err(error) = child.start_kill() {
        tracing::warn!(route_prefix = %route_prefix, error = %error, "failed to signal idle backend");
        return;
    }

    if let Err(error) = child.wait().await {
        tracing::warn!(route_prefix = %route_prefix, error = %error, "failed to wait for idle backend shutdown");
    }

    runtime.process = None;
}
