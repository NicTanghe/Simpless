# Home Activator Gateway for On-Demand Axum Services

## Goal

Build a small always-on Rust gateway on the home server that exposes a single public port, accepts requests only from the Lambda-facing website path, and starts internal Axum services on demand. Internal services should listen only on loopback, be reachable through path-based routing, and stop again after an idle timeout.

This gives a local "serverless-like" model without relying on systemd socket activation, containers, Shuttle, or Spin.

---

## Problem Statement

The home machine has many jobs, so exposing multiple always-on services directly increases blast radius and complexity. The desired system should:

- expose only one public ingress port
- keep internal services private
- start a backend only when it is actually needed
- shut unused backends down after an idle period
- centralize auth, routing, logging, and rate limiting
- stay fully within the Rust ecosystem

---

## High-Level Architecture

```text
User
  ↓
AWS Lambda website / API layer
  ↓
Home public IP:3000
  ↓
Activator Gateway (Axum + Tower)
  ↓
Path-based internal routing
  ├─ /api/*    → 127.0.0.1:9001
  ├─ /media/*  → 127.0.0.1:9002
  └─ /admin/*  → 127.0.0.1:9003
```

### Core idea

Only the activator gateway is exposed externally.

Each internal Axum service:

- binds only to `127.0.0.1:<port>`
- is not reachable from outside the machine
- is spawned by the activator when first requested
- is killed after a configurable idle timeout

---

## Why This Architecture

### Benefits

- Single exposed port and firewall rule
- Clear choke point for authentication and request validation
- Internal services remain private
- Services do not consume memory when idle
- Central place for observability and policy enforcement
- No dependency on systemd socket activation
- No container overhead or orchestration complexity
- Fits Rust + Axum + Tower cleanly

### Tradeoffs

- First request to a sleeping service pays a cold-start cost
- The activator becomes critical infrastructure and must be reliable
- Process spawning and readiness probing must be implemented carefully
- A custom gateway is more work than dropping in nginx or xinetd

---

## Scope

### In scope

- one always-on activator process
- path-based routing to internal services
- on-demand spawning of internal services
- idle shutdown of internal services
- loopback-only backend binding
- centralized auth hooks
- basic structured logging and health checks
- implementation order for an MVP and hardening path

### Out of scope for v1

- automatic container orchestration
- service discovery across multiple machines
- hot reload / blue-green deployment
- autoscaling beyond one process per service
- per-service cgroup limits
- mutual TLS between Lambda and home server

These can be added later if needed.

---

## Request Lifecycle

1. A request arrives at the home server on the single exposed port.
2. The activator gateway accepts the request.
3. The gateway validates that the request is allowed.
4. The gateway extracts the first path segment, for example `api` from `/api/orders/123`.
5. The gateway looks up the configured service entry for that segment.
6. If the backend process is not running, the gateway starts it.
7. The gateway waits until the backend is ready on its loopback port.
8. The request URI is rewritten to target the internal backend.
9. The request is proxied to the backend.
10. The gateway updates the service's last-used timestamp.
11. A background reaper later kills the service if it stays idle past its timeout.

---

## Trust Boundaries

### Boundary 1: Internet to home ingress

Only the activator's public port is exposed. The firewall should allow traffic only from the Lambda-facing path you control.

### Boundary 2: Activator to internal services

Internal services are trusted less than the activator. They should not implement their own public ingress. They should bind to loopback only.

### Boundary 3: Internal service to filesystem / secrets

Each backend should only receive the paths and environment variables it actually needs.

---

## Security Model

### External exposure

- expose only one port, for example `3000`
- firewall should allow only the expected upstream
- reject unknown routes early
- optionally require a shared secret header or signed request marker from Lambda

### Internal service exposure

Each backend must bind to loopback only:

```rust
let listener = tokio::net::TcpListener::bind("127.0.0.1:9001").await?;
```

Never bind internal services to `0.0.0.0`.

### Process isolation

Even without containers, backends can still run as different users later. For v1, keep them loopback-only and minimize what environment and filesystem access they need.

### Choke-point protections in the activator

The activator is the right place to add:

- auth header validation
- request size limits
- route allowlists
- rate limiting
- structured audit logging
- health check handling

---

## Component Design

## 1. Activator Gateway

The activator is a small Axum service that:

- listens on the single public port
- matches the first path segment to a service definition
- ensures the target service is running
- proxies the request to the internal service
- tracks last-use timestamps
- runs a background idle reaper

### Responsibilities

- routing
- startup orchestration
- readiness probing
- reverse proxy behavior
- lifecycle bookkeeping
- central security hooks

### Non-responsibilities

- business logic for internal services
- direct database work unless it is its own service
- complicated service mesh behavior

---

## 2. Service Registry

The activator needs a registry that maps a route prefix to a backend service definition.

Suggested structure:

```rust
struct ServiceConfig {
    route_prefix: String,
    command: String,
    args: Vec<String>,
    port: u16,
    startup_timeout_ms: u64,
    idle_timeout_secs: u64,
    health_path: String,
}
```

Runtime state can be stored separately:

```rust
struct ServiceRuntime {
    process: Option<tokio::process::Child>,
    last_used: std::time::Instant,
    starting: bool,
}
```

This split keeps configuration static and runtime state mutable.

---

## 3. Proxy Layer

The activator should act as a reverse proxy.

Behavior:

- preserve method, headers, and body
- rewrite the target URI to `http://127.0.0.1:<port>/<rest-of-path>`
- optionally strip the route prefix before forwarding
- forward response status, headers, and body back to caller

Tower and Hyper are a good fit here because they integrate naturally with Axum.

---

## 4. Startup Manager

When a request targets a sleeping service:

- mark the service as starting
- spawn the configured command
- poll the health endpoint or open port until ready
- clear the starting flag
- then proxy the request

### Important concurrency rule

If five requests arrive at once for the same sleeping service, only one startup should happen. The others should wait for readiness, not spawn five copies.

This means the runtime state needs either:

- a startup lock per service, or
- a notify/wait mechanism for concurrent callers

---

## 5. Idle Reaper

A background task should periodically inspect each running service.

If:

- the process is running, and
- `now - last_used > idle_timeout`

then the activator kills the service and clears its runtime state.

This creates the local scale-to-zero behavior.

---

## Route Strategy

Recommended path prefixes:

- `/api/*`
- `/media/*`
- `/admin/*`
- `/auth/*`

### Recommendation

Keep the mapping explicit and static in v1. Avoid dynamic route registration at first.

---

## Configuration Strategy

### v1

Use a small SQLite database so configuration stays mutable and can later be edited through an admin endpoint without introducing another storage layer.

Example:

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

### Why SQLite

- single-file deployment
- transactional updates from future API endpoints
- enough structure to enforce uniqueness and basic validation

---

## Failure Modes and Expected Behavior

## Unknown route prefix

Return `404 Not Found`.

## Backend fails to spawn

Return `502 Bad Gateway` or `500 Internal Server Error` and log the command failure.

## Backend starts but never becomes ready

Return `504 Gateway Timeout` after `startup_timeout_ms` expires.

## Backend crashes while handling requests

Return `502 Bad Gateway` and clear runtime state so the next request can trigger a fresh start.

## Repeated crash loop

Add backoff later. For v1, log clearly and do not spin infinitely.

---

## Observability

### Minimum for v1

- startup event per service
- shutdown event per service
- proxy target selected
- startup timeout
- bad route / auth rejection
- backend failure to proxy

### Suggested stack

- `tracing`
- `tracing-subscriber`
- request IDs in logs

---

## Health Endpoints

### Activator health

Expose a lightweight health route on the activator, for example:

- `/health`
- `/ready`

### Backend health

Each managed service should expose:

- `/health`

This makes readiness probing simple and explicit.

---

## Performance Notes

### Cold starts

The first request to a sleeping service will pay startup cost. The activator must probe for readiness rather than sleeping a fixed amount.

### Warm services

Once started, a service should behave like a normal local Axum service and proxy overhead should be minimal.

### Recommendation

Start with one process per service. Do not try to do multiple worker replicas in v1.

---

## Implementation Order

## Phase 0 — Foundation

Goal: create a minimal activator that can route and proxy to an already-running backend.

Tasks:

1. Create a new Rust project for the activator.
2. Add dependencies: `axum`, `tokio`, `tower`, `hyper`, `serde`, `rusqlite`, `serde_json`, `tracing`, `tracing-subscriber`.
3. Define a static service registry in code.
4. Implement path-prefix extraction.
5. Implement URI rewrite and reverse proxy to a loopback backend.
6. Verify that `/api/*` correctly forwards to a manually started backend on `127.0.0.1:9001`.

Exit criteria:

- one public port works
- one internal service can be reached through the activator
- backend is loopback-only

---

## Phase 1 — On-Demand Spawn

Goal: start a backend if it is not already running.

Tasks:

1. Add runtime state for each service.
2. Spawn the backend with `tokio::process::Command`.
3. Add a readiness probe loop that calls the backend health endpoint.
4. Block or queue the triggering request until backend is ready.
5. Store the child process handle.
6. Update `last_used` after each successful request.

Exit criteria:

- backend starts automatically on first request
- request succeeds without manual startup
- only one child process is running for the service

---

## Phase 2 — Concurrency Control

Goal: prevent duplicate startup races.

Tasks:

1. Add per-service startup locking or notification.
2. Ensure concurrent requests to a sleeping service wait for the same startup.
3. Add tests for many simultaneous requests to the same prefix.

Exit criteria:

- ten concurrent requests do not spawn ten copies
- waiting requests resume once service becomes ready

---

## Phase 3 — Idle Shutdown

Goal: scale services back down when idle.

Tasks:

1. Add a background reaper task.
2. Compare `last_used` with `idle_timeout_secs`.
3. Kill and clean up idle child processes.
4. Handle already-exited children cleanly.

Exit criteria:

- unused services stop automatically
- later requests start them again successfully

---

## Phase 4 — Config File Support

Goal: stop hardcoding service definitions.

Tasks:

1. Move service definitions into a SQLite config database.
2. Load config rows at startup.
3. Validate duplicate route prefixes and duplicate ports.
4. Fail fast on invalid configuration.

Exit criteria:

- services are defined entirely in SQLite
- activator can manage multiple services without recompilation

---
this phase should not be implemented and needs human verification.
<!-- ## Phase 5 — Security Hooks -->

<!-- Goal: make the ingress safer before production use. -->

<!-- Tasks: -->

<!-- 1. Add a shared-secret header check or signed request validation. -->
<!-- 2. Add request size limits. -->
<!-- 3. Add route allowlist behavior. -->
<!-- 4. Add structured auth-failure logging. -->
<!-- 5. Make sure unknown paths fail closed. -->

<!-- Exit criteria: -->

<!-- - only trusted upstream requests are accepted -->
<!-- - invalid requests are rejected early and logged -->

---

## Phase 6 — Observability and Operations

Goal: make the system operable.

Tasks:

1. Add `tracing` spans for route match, startup, proxy, and shutdown.
2. Add activator `/health` and `/ready` routes.
3. Add service status introspection endpoint, for example `/internal/services`.
4. Add basic metrics later if needed.

Exit criteria:

- you can see what service started, when, and why
- you can inspect whether a service is warm or sleeping

---

## Phase 7 — Hardening

Goal: improve safety and reliability after the MVP proves itself.

Tasks:

1. Run internal services under separate users where practical.
2. Add startup backoff for crash loops.
3. Add graceful shutdown where needed.
4. Add per-service environment whitelists.
5. Add integration tests for crash recovery.
6. Optionally add mTLS or stronger upstream verification.

Exit criteria:

- predictable recovery from failures
- limited blast radius if one service misbehaves

---

## Recommended Initial Milestone Plan

### Milestone A — Proof of concept

Deliver:

- one activator
- one backend service
- one route prefix
- proxy to a manually started backend

### Milestone B — Real on-demand startup

Deliver:

- backend spawned automatically
- readiness probing
- no duplicate startup race

### Milestone C — Production-ready local gateway

Deliver:

- config file
- idle reaper
- auth header check
- health endpoints
- structured logs

---

## Suggested Internal Project Structure

```text
activator/
  src/
    main.rs
    app.rs
    config.rs
    proxy.rs
    registry.rs
    startup.rs
    reaper.rs
    auth.rs
    health.rs
  config/
    services.db
```

### Module roles

- `app.rs`: router construction
- `config.rs`: config loading and validation
- `registry.rs`: service config + runtime state
- `proxy.rs`: request forwarding logic
- `startup.rs`: process spawn + readiness probing
- `reaper.rs`: idle timeout shutdown logic
- `auth.rs`: upstream verification
- `health.rs`: activator health endpoints

---

## Design Decisions

## Decision 1: Single public port

Chosen because it simplifies firewalling, ingress policy, logging, and external routing.

## Decision 2: Path-based routing instead of port fan-out

Chosen because it hides internal service ports and makes the activator the only ingress point.

## Decision 3: Loopback-only backends

Chosen to reduce accidental exposure and centralize external access policy.

## Decision 4: Config-driven service registry

Chosen so services can be added without recompiling once the MVP is working.

## Decision 5: Rust-native activator instead of systemd socket activation

Chosen because it keeps control in the application layer and avoids relying on a workflow you dislike.

---

## Risks

### Risk: activator becomes a single point of failure

Mitigation:

- keep it small
- add clear health checks
- fail fast on invalid config
- log aggressively

### Risk: startup race conditions

Mitigation:

- per-service startup lock
- readiness notification for waiting requests

### Risk: cold starts become noticeable

Mitigation:

- keep services lean
- use health probing instead of blind sleep
- increase idle timeout for frequently used services

### Risk: internal service binds publicly by mistake

Mitigation:

- enforce `127.0.0.1` in all service configs and code review this explicitly

---

## Practical Defaults

Recommended defaults for v1:

- activator public port: `3000`
- startup timeout: `4000 ms`
- idle timeout: `120 s`
- readiness endpoint: `/health`
- backend bind address: `127.0.0.1`

---

## Summary

This design creates a small Rust-native gateway that provides a local serverless-like runtime for multiple Axum services behind a single external port.

It is a strong fit when:

- the home server has many jobs
- you want to reduce always-on service footprint
- you want one clear ingress point
- you want to stay inside Rust and avoid systemd socket activation

The most sensible build order is:

1. proxy to an already-running backend
2. add process spawning
3. add readiness coordination
4. add idle shutdown
5. move to config file
6. add security and observability

That order keeps the system debuggable and lets you validate the architecture one layer at a time.
