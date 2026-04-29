use axum::{routing::{get, post}, Json, Router};
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

#[derive(Deserialize)]
struct CreateDocumentRequest {
    template_id: String,
    input_text: String,
}

#[derive(Serialize)]
struct DemoResponse {
    status: &'static str,
    message: &'static str,
    template_id: Option<String>,
}

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/health", get(health))
        .route("/document/create", post(create_document));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8001")
        .await
        .expect("bind document service");
    axum::serve(listener, app).await.expect("serve document service");
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn create_document(Json(payload): Json<CreateDocumentRequest>) -> Json<DemoResponse> {
    let _ = payload.input_text;
    Json(DemoResponse {
        status: "demo",
        message: "Public snapshot excludes private templates and runtime data.",
        template_id: Some(payload.template_id),
    })
}
