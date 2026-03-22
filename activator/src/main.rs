mod admin;
mod app;
mod auth;
mod config;
mod health;
mod proxy;
mod reaper;
mod registry;
mod startup;

use std::{env, error::Error, net::SocketAddr, sync::Arc, time::Duration};
use std::path::PathBuf;

use tokio::net::TcpListener;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

use crate::{
    app::AppState,
    config::{default_config_path, load_registry_from_path},
    reaper::spawn_reaper,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    init_tracing();

    let bind_addr = env::var("ACTIVATOR_BIND_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:3000".to_owned())
        .parse::<SocketAddr>()?;
    let config_path = env::var("ACTIVATOR_CONFIG_PATH")
        .map(Into::into)
        .unwrap_or_else(|_| default_config_path());
    let upload_dir: PathBuf = env::var("ACTIVATOR_UPLOAD_DIR")
        .map(Into::into)
        .unwrap_or_else(|_| "uploads".into());
    let reaper_interval = Duration::from_millis(
        env::var("ACTIVATOR_REAPER_INTERVAL_MS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(1_000),
    );

    let registry = load_registry_from_path(&config_path)?;
    let state = Arc::new(AppState::new(registry, config_path.clone(), upload_dir.clone())?);
    let _reaper = spawn_reaper(state.clone(), reaper_interval);
    let app = app::build_router(state);
    let listener = TcpListener::bind(bind_addr).await?;

    tracing::info!(
        %bind_addr,
        config_path = %config_path.display(),
        upload_dir = %upload_dir.display(),
        ?reaper_interval,
        "activator listening"
    );

    axum::serve(listener, app).await?;
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer())
        .init();
}
