# Home Activator Workspace

## Quick Overview

This workspace contains a small Rust gateway that exposes one public port and starts a loopback-only backend on demand.

- `activator/`: the Axum gateway
- `hello_backend/`: a simple Axum backend used for local testing
- `docs/home_activator_axum_design_doc.md`: the design doc the implementation follows

Current default behavior:

- the activator listens on `127.0.0.1:3000`
- `/api/*` is routed to a backend on `127.0.0.1:9001`
- the `/api` prefix is stripped before forwarding
- if the backend is not running, the activator starts it
- concurrent cold-start requests share one startup path
- idle backends are stopped by the reaper after the configured timeout

## Current Status

Implemented:

- Phase 0: proxy to a running loopback backend
- Phase 1: start backend on first request
- Phase 2: coordinate concurrent startup
- Phase 3: idle shutdown and restart on later request
- Phase 4: TOML config file support

Not implemented yet:

- Phase 5: auth and request hardening
- Phase 6: richer observability and service introspection
- Phase 7: hardening and crash-loop recovery

## Quick Start

Run the activator:

```powershell
cd C:\Users\Nicol\dev\leptos\FXiT\simples\activator
$env:RUST_LOG="info"
cargo run --release
```

The backend should autostart on the first `/api` request.

Default config file:

- `activator\config\services.toml`

Useful scripts:

- `activator\smoke_test.ps1`: basic gateway and proxy smoke test
- `activator\concurrent_startup_test.ps1`: cold-start concurrency check
- `activator\check_backend_shutdown.ps1`: idle shutdown check

Example fast idle-shutdown test:

```powershell
cd C:\Users\Nicol\dev\leptos\FXiT\simples\activator
$env:RUST_LOG="info"
$env:ACTIVATOR_REAPER_INTERVAL_MS="250"
$env:ACTIVATOR_CONFIG_PATH="config/services.fast.toml"
cargo run --release
```

Then in another terminal:

```powershell
cd C:\Users\Nicol\dev\leptos\FXiT\simples\activator
.\check_backend_shutdown.ps1 -IdleWaitSeconds 10 -ConfiguredIdleTimeoutSeconds 5
```

Where `config/services.fast.toml` is a copy of `config/services.toml` with `idle_timeout_secs = 5`.

## Config File

The activator now loads services from TOML at startup. The default path is `config/services.toml`, or you can override it with `ACTIVATOR_CONFIG_PATH`.

Current default file:

```toml
[[service]]
route_prefix = "api"
command = "cargo"
args = ["run"]
port = 9001
startup_timeout_ms = 15000
idle_timeout_secs = 120
health_path = "/health"
working_directory = "../../hello_backend"
```

Supported service fields:

- `route_prefix`
- `command`
- `args`
- `port`
- `startup_timeout_ms`
- `idle_timeout_secs`
- `health_path`
- `working_directory`
- `strip_prefix` (optional, defaults to `true`)
- `environment` (optional TOML table)

## Environment Variables

### Activator

| Variable | Default | Purpose |
| --- | --- | --- |
| `RUST_LOG` | `info` fallback if unset | Tracing filter used by `tracing-subscriber`. |
| `ACTIVATOR_BIND_ADDR` | `127.0.0.1:3000` | Address the activator binds to. |
| `ACTIVATOR_CONFIG_PATH` | `config/services.toml` | TOML file that defines managed services. |
| `ACTIVATOR_REAPER_INTERVAL_MS` | `1000` | Background reaper sweep interval in milliseconds. |

### Hello Backend

These are mainly for testing and are normally injected by the activator or tests.

| Variable | Default | Purpose |
| --- | --- | --- |
| `HELLO_BACKEND_BIND_ADDR` | `127.0.0.1:9001` | Address the test backend binds to. |
| `HELLO_BACKEND_START_DELAY_MS` | `0` | Optional artificial startup delay for startup/concurrency tests. |
| `HELLO_BACKEND_START_MARKER` | unset | Optional file path; when set, the backend appends `started` on boot. Used by tests to prove how many times it started. |

## Notes

- Service definitions now come from TOML and are validated on startup.
- The activator fails fast on missing config, parse errors, duplicate route prefixes, and duplicate backend ports.
- Only the `/api` route is configured by default.
- The default `/api` backend startup command is development-oriented and assumes the sibling `hello_backend` crate exists.
- If you want a real cold-start test, make sure nothing is already listening on the backend port before running the scripts.
