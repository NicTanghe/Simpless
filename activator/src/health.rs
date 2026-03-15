use axum::{Json, http::StatusCode, response::IntoResponse};

#[derive(serde::Serialize)]
struct HealthResponse {
    status: &'static str,
}

pub async fn health() -> impl IntoResponse {
    Json(HealthResponse { status: "ok" })
}

pub async fn ready() -> impl IntoResponse {
    (StatusCode::OK, Json(HealthResponse { status: "ready" }))
}
