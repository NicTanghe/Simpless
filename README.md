# simpless

**The self-hosted activator gateway that makes local services feel serverless.**


`simpless` gives you one clean ingress point for side projects, home-lab APIs, and internal tools. It keeps backend services private on loopback, starts them only when traffic arrives, and shuts them back down when they go idle.

The result is simple: less always-on clutter, less exposed surface area, and a setup that feels far more polished than a pile of manually managed ports.

Built with `axum`, `hyper`, `tower`, and `tokio`.

## Why It Stands Out

- One public entry point instead of a different port for every service
- Loopback-only backends by default
- Automatic cold start on first request
- Shared startup coordination for concurrent traffic
- Idle shutdown to reclaim resources when a service is not being used
- Config-driven routing through SQLite
- Health endpoints and PowerShell verification scripts included

## What Ships Today

- Reverse proxying for configured prefixes such as `/api/*`
- Prefix stripping before forwarding to the backend
- Forwarding of method, headers, body, and query string
- On-demand process startup with readiness probing
- Single-start behavior under concurrent cold-start traffic
- Idle reaping and later restart on the next request
- Activator health endpoints at `/health` and `/ready`
- Startup-time config validation for duplicate route prefixes and backend ports

## How It Works

1. A request hits the activator on one exposed address.
2. The activator maps the first path segment, such as `/api`, to a configured service.
3. If the backend is asleep, the activator starts it and waits for it to become ready.
4. The request is proxied to the loopback-only backend.
5. After an idle timeout, the backend is stopped automatically.

This is the core pitch of the project: a small Rust gateway that gives self-hosted services a "wake on demand" experience.

## Quick Start

### Prerequisites

- A Rust toolchain
- A backend service you want the activator to start

The repo ships a sample SQLite config at `activator/config/services.db` that points at a sibling test backend in `../../hello_backend`. If that backend does not exist in your local workspace, update the SQLite row before running the gateway.

Run the activator:

```powershell
cd C:\Users\Nicol\dev\leptos\FXiT\simpless\activator
$env:RUST_LOG="info"
cargo run --release
```

Default local endpoints:

- `http://127.0.0.1:3000/health`
- `http://127.0.0.1:3000/ready`
- `http://127.0.0.1:3000/api/...`

Useful verification scripts:

- `activator/smoke_test.ps1`
- `activator/concurrent_startup_test.ps1`
- `activator/check_backend_shutdown.ps1`

Example:

```powershell
cd C:\Users\Nicol\dev\leptos\FXiT\simpless\activator
.\smoke_test.ps1
```

## Configuration

The activator loads services from `activator/config/services.db` by default. You can override the database path with `ACTIVATOR_CONFIG_PATH`.

SQLite schema:

```sql
CREATE TABLE services (
    route_prefix TEXT PRIMARY KEY,
    command TEXT NOT NULL,
    args_json TEXT NOT NULL,
    backend_port INTEGER NOT NULL UNIQUE,
    strip_prefix INTEGER NOT NULL,
    environment_json TEXT NOT NULL,
    working_directory TEXT,
    startup_timeout_ms INTEGER NOT NULL,
    idle_timeout_secs INTEGER NOT NULL,
    health_path TEXT NOT NULL
);
```

Supported service fields:

- `route_prefix`
- `command`
- `args_json` JSON array of strings
- `backend_port`
- `startup_timeout_ms`
- `idle_timeout_secs`
- `health_path`
- `working_directory`
- `strip_prefix` stored as `0` or `1`
- `environment_json` JSON object of string pairs

Example update:

```powershell
sqlite3 activator/config/services.db "select route_prefix, command, backend_port, working_directory from services;"
```

## Environment Variables

### Activator

| Variable | Default | Purpose |
| --- | --- | --- |
| `RUST_LOG` | `info` if unset | Tracing filter used by `tracing-subscriber`. |
| `ACTIVATOR_BIND_ADDR` | `127.0.0.1:3000` | Address the activator binds to. |
| `ACTIVATOR_CONFIG_PATH` | `config/services.db` | SQLite database that defines managed services. |
| `ACTIVATOR_REAPER_INTERVAL_MS` | `1000` | Idle-reaper sweep interval in milliseconds. |

### Test Backend

These variables are mainly for the sibling test backend used by the scripts and local verification flow.

| Variable | Default | Purpose |
| --- | --- | --- |
| `HELLO_BACKEND_BIND_ADDR` | `127.0.0.1:9001` | Address the test backend binds to. |
| `HELLO_BACKEND_START_DELAY_MS` | `0` | Artificial startup delay for startup and concurrency tests. |
| `HELLO_BACKEND_START_MARKER` | unset | Optional file path used by tests to record backend starts. |

## Project Layout

- `activator/` main crate, config, and PowerShell test scripts
- `docs/home_activator_axum_design_doc.md` longer architecture and roadmap document

## Roadmap

The current foundation is already strong: routing, startup orchestration, concurrent cold-start coordination, idle shutdown, and config loading are in place.

Next up:

- uploading binairies
- auth and request hardening hooks
- webui
- richer service introspection
- stronger observability and recovery behavior

- allow starting of processes on other servers
- transition to actual serverless by using firecracker

- load balancing

## Why This Project Exists

Most self-hosted setups drift toward one of two bad extremes: too many always-on processes, or too much manual start-stop friction. `simpless` is built to sit in the middle and give you the better tradeoff.

You keep the control of self-hosting, but you get a cleaner ingress story, lower idle overhead, and a workflow that feels much closer to a mature platform product than a collection of local scripts.
