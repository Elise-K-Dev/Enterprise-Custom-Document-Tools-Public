#![recursion_limit = "256"]

use std::{
    collections::{BTreeMap, HashMap},
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
};

use axum::{
    extract::{Query, State},
    http::{
        header::{CONTENT_DISPOSITION, CONTENT_TYPE},
        HeaderMap, HeaderValue, StatusCode,
    },
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tower_http::{
    cors::{Any, CorsLayer},
    trace::TraceLayer,
};
use tracing::info;
use uuid::Uuid;

mod legacy_engine;
mod legacy_port;

use legacy_port::{
    build_purchase_reason_text, decide_purchase_v2, load_named_templates, render_docx_bytes,
    select_template_for_row, template_dir_candidates, DocumentRow as LegacyDocumentRow,
    PurchaseDecision,
};

#[derive(Clone)]
struct AppState {
    templates: Arc<HashMap<String, TemplateDefinition>>,
    sessions: Arc<RwLock<HashMap<String, SessionState>>>,
    download_grants: Arc<RwLock<HashMap<String, DownloadGrant>>>,
    legacy_workdir: Option<PathBuf>,
    public_base_url: String,
    markdown_pdf_base_url: String,
    http_client: reqwest::Client,
}

#[derive(Debug, Clone)]
struct TemplateDefinition {
    id: &'static str,
    display_name: &'static str,
    required_fields: Vec<&'static str>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionState {
    session_id: String,
    template_id: String,
    fields: BTreeMap<String, serde_json::Value>,
    missing_fields: Vec<String>,
}

#[derive(Debug, Clone)]
struct RegisteredUser {
    id: String,
    email: String,
    name: String,
}

#[derive(Debug, Clone)]
struct DownloadGrant {
    path: String,
    user: RegisteredUser,
    expires_at: i64,
}

#[derive(Debug, Deserialize)]
struct FillRequest {
    template_id: String,
    session_id: String,
    #[serde(default)]
    current_fields: BTreeMap<String, serde_json::Value>,
    user_message: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct FillResponse {
    updated_fields: BTreeMap<String, serde_json::Value>,
    missing_fields: Vec<String>,
    next_question: Option<String>,
    preview_text: String,
    session: SessionState,
}

#[derive(Debug, Deserialize)]
struct CreateRequest {
    template_id: String,
    input_text: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct CreateResponse {
    session_id: String,
    updated_fields: BTreeMap<String, serde_json::Value>,
    missing_fields: Vec<String>,
    next_question: Option<String>,
    preview_text: String,
}

#[derive(Debug, Deserialize)]
struct ExportRequest {
    template_id: String,
    fields: BTreeMap<String, serde_json::Value>,
    format: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ExportResponse {
    file_name: String,
    format: String,
    mime_type: String,
    content_base64: String,
    preview_text: String,
    generated_at: String,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
}

#[derive(Debug, Deserialize)]
struct LegacyRunRequest {
    #[serde(default = "default_true")]
    force: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct LegacyRunResponse {
    workdir: String,
    output_dir: String,
    generated_count: usize,
    generated_files: Vec<String>,
    snapshot_json_path: Option<String>,
    batch_report_path: Option<String>,
    stdout: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct LegacyPackageResponse {
    generated_count: usize,
    zip_path: String,
    zip_file_name: String,
    download_path: String,
    download_url: String,
    message: String,
    assistant_summary: String,
    generated_files_preview: Vec<String>,
    batch_report_path: Option<String>,
    snapshot_json_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LegacyDownloadQuery {
    path: String,
    token: String,
}

#[derive(Debug, Deserialize)]
struct MarkdownPdfDownloadQuery {
    path: String,
    token: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct RenderMarkdownPdfRequest {
    title: String,
    markdown: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    page_size: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    orientation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generated_for: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    account_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    account_email: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_at: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RenderChatDocumentRequest {
    title: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transcript: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generated_for: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    account_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    account_email: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RenderMarkdownPdfResponse {
    title: String,
    file_name: String,
    download_path: String,
    download_url: String,
    assistant_summary: String,
}

#[derive(Debug, Deserialize)]
struct LegacyShortagesQuery {
    query: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct LegacyInventoryQuery {
    query: Option<String>,
    status: Option<String>,
    match_status: Option<String>,
    sort: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
struct LegacyShortagesResponse {
    data_source: String,
    source_policy: String,
    snapshot_json_path: String,
    snapshot_date: Option<String>,
    total_count: usize,
    unverified_count: usize,
    markdown_table: String,
    unverified_markdown_table: Option<String>,
    items: Vec<LegacyShortageItem>,
    unverified_items: Vec<LegacyShortageItem>,
}

#[derive(Debug, Serialize)]
struct LegacyInventoryResponse {
    data_source: String,
    source_policy: String,
    snapshot_json_path: String,
    snapshot_date: Option<String>,
    total_count: usize,
    returned_count: usize,
    filter_options: LegacyInventoryFilterOptions,
    markdown_table: String,
    items: Vec<LegacyShortageItem>,
}

#[derive(Debug, Serialize)]
struct LegacyInventoryReportResponse {
    output_path: String,
    download_path: String,
    download_url: String,
    file_name: String,
    generated_count: usize,
    assistant_summary: String,
}

#[derive(Debug, Serialize)]
struct LegacyInventoryFilterOptions {
    status: Vec<&'static str>,
    match_status: Vec<&'static str>,
    sort: Vec<&'static str>,
}

#[derive(Debug, Serialize, Clone)]
struct LegacyShortageItem {
    part_name: String,
    part_no: String,
    current_stock: Option<f64>,
    required_stock: Option<f64>,
    available_stock: Option<f64>,
    shortage_gap: Option<f64>,
    shortage_quantity: Option<f64>,
    projected_stock_balance: Option<f64>,
    movement_net_qty: f64,
    inbound_qty_sum: f64,
    outbound_qty_sum: f64,
    outbound_count: usize,
    inventory_confirmed: bool,
    inventory_match_status: String,
    stock_status: String,
    unit_price: Option<f64>,
    purchase_priority: String,
    purchase_policy_note: String,
    summary: String,
    document_request_hint: String,
}

#[derive(Debug, Deserialize)]
struct LegacyItemContextQuery {
    part_name: Option<String>,
    part_no: Option<String>,
}

#[derive(Debug, Serialize)]
struct GuidedFieldSpec {
    field: String,
    label: String,
    prompt: String,
}

#[derive(Debug, Serialize)]
struct LegacyItemContextResponse {
    part_name: String,
    part_no: String,
    context: String,
    data_source: String,
    source_policy: String,
    snapshot_json_path: String,
    fields_seed: BTreeMap<String, serde_json::Value>,
    guided_fields: Vec<GuidedFieldSpec>,
    assistant_summary: String,
}

#[derive(Debug, Deserialize)]
struct LegacyItemExportRequest {
    part_name: Option<String>,
    part_no: Option<String>,
    fields: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct LegacyItemExportResponse {
    output_path: String,
    download_path: String,
    download_url: String,
    file_name: String,
    assistant_summary: String,
}

#[derive(Debug, Deserialize)]
struct LegacyApproveRequest {
    part_name: Option<String>,
    part_no: Option<String>,
    #[serde(default)]
    fields: BTreeMap<String, serde_json::Value>,
    purchase_reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct LegacyApproveResponse {
    pricing_policy_note: String,
    draft_preview: String,
    resolved_fields: BTreeMap<String, serde_json::Value>,
    output_path: String,
    download_path: String,
    download_url: String,
    file_name: String,
    assistant_summary: String,
}

const INTERNAL_TOKEN_HEADER: &str = "x-port-project-internal-token";
const OPEN_WEBUI_USER_EMAIL_HEADER: &str = "x-openwebui-user-email";
const OPEN_WEBUI_USER_ID_HEADER: &str = "x-openwebui-user-id";
const OPEN_WEBUI_USER_NAME_HEADER: &str = "x-openwebui-user-name";
const DEFAULT_DOWNLOAD_TOKEN_TTL_SECONDS: i64 = 3600;

fn default_true() -> bool {
    true
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "document_service=debug,tower_http=info".into()),
        )
        .init();

    let app = app_router();
    let host = std::env::var("DOCUMENT_SERVICE_HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
    let port = std::env::var("DOCUMENT_SERVICE_PORT")
        .ok()
        .and_then(|raw| u16::from_str(&raw).ok())
        .unwrap_or(8001);
    let addr = SocketAddr::from_str(&format!("{host}:{port}")).unwrap();
    info!("document service listening on {addr}");

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn openapi_spec() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "openapi": "3.0.0",
        "info": {
            "title": "Document Generation Gateway",
            "version": "1.0.0",
            "description": "구매 품의서, 재고 보고서, Markdown 기반 PDF 보고서, 보고서/채팅 기록 Word/Excel 내보내기를 8001 단일 문서 생성 도구로 처리하고 다운로드 정보를 반환하는 도구 서버"
        },
        "servers": [
            { "url": "http://document-service:8001" }
        ],
        "paths": {
            "/health": {
                "get": {
                    "operationId": "health_check",
                    "summary": "Health check",
                    "responses": {
                        "200": {
                            "description": "Healthy",
                            "content": {
                                "application/json": {
                                    "schema": {
                                        "type": "object",
                                        "properties": {
                                            "status": { "type": "string", "example": "ok" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            },
            "/document/create": {
                "post": {
                    "operationId": "create_document",
                    "summary": "구매 품의서 전용 대화형 문서 채우기 세션 시작",
                    "description": "구매 품의서(purchase_request) 전용 도구다. template_id는 반드시 purchase_request만 사용한다. 수리 완료 보고서, 업무 보고서, 회의록, 일반 요약 보고서, repair_report 같은 템플릿은 이 도구로 만들지 말고 Markdown 본문을 작성한 뒤 render_markdown_pdf를 호출한다. 품명 또는 품번이 입력에 포함되면 전처리 완료된 stock_in_out_monthly.json 스냅샷을 기준으로 품명, 품번, 현재고, 교체이력 관련 필드를 자동 보강한다.",
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": {
                                    "type": "object",
                                    "required": ["template_id", "input_text"],
                                    "properties": {
                                        "template_id": { "type": "string", "enum": ["purchase_request"], "example": "purchase_request" },
                                        "input_text": { "type": "string" }
                                    }
                                }
                            }
                        }
                    },
                    "responses": { "200": { "description": "Document filling session created" } }
                }
            },
            "/document/fill": {
                "post": {
                    "operationId": "fill_document",
                    "summary": "구매 품의서 전용 대화형 문서 필드 추가 채움",
                    "description": "purchase_request 구매 품의서 세션 전용 도구다. 수리 완료 보고서, 업무 보고서, 회의록, 일반 요약 보고서에는 사용하지 않는다. 이전 세션 상태와 현재 사용자 답변을 합쳐 필드를 갱신하고 다음으로 채울 칸을 반환한다. 사용자가 납품업체: 지정 협력사 처럼 말하면 해당 칸을 확정한다. 품명 또는 품번이 확인되면 전처리 완료된 stock_in_out_monthly.json 스냅샷을 기준으로 재고 및 이력 필드를 다시 보강한다.",
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": {
                                    "type": "object",
                                    "required": ["template_id", "session_id", "user_message"],
                                    "properties": {
                                        "template_id": { "type": "string", "enum": ["purchase_request"], "example": "purchase_request" },
                                        "session_id": { "type": "string" },
                                        "current_fields": {
                                            "type": "object",
                                            "additionalProperties": true
                                        },
                                        "user_message": { "type": "string" }
                                    }
                                }
                            }
                        }
                    },
                    "responses": { "200": { "description": "Document fields updated" } }
                }
            },
            "/document/export": {
                "post": {
                    "operationId": "export_document",
                    "summary": "구매 품의서 전용 채워진 필드로 문서 내보내기",
                    "description": "purchase_request 구매 품의서 필드를 사용해 문서 파일 내용을 생성한다. 수리 완료 보고서, 업무 보고서, 회의록, 일반 요약 보고서에는 사용하지 않는다. docx 형식이면 Rust 레거시 DOCX 렌더러를 사용한다. 품명 또는 품번이 채워져 있으면 렌더 직전에 전처리 완료된 stock_in_out_monthly.json 스냅샷 기준으로 재고/이력 필드를 다시 보강한다.",
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": {
                                    "type": "object",
                                    "required": ["template_id", "fields", "format"],
                                    "properties": {
                                        "template_id": { "type": "string", "enum": ["purchase_request"], "example": "purchase_request" },
                                        "fields": {
                                            "type": "object",
                                            "additionalProperties": true
                                        },
                                        "format": { "type": "string", "example": "docx" }
                                    }
                                }
                            }
                        }
                    },
                    "responses": { "200": { "description": "Document exported" } }
                }
            },
            "/document/legacy/package": {
                "post": {
                    "operationId": "generate_purchase_document_package",
                    "summary": "Run the full purchase document batch and return a ZIP package for download",
                    "description": "사용자가 구매 품의 문서 전체 작성, 일괄 생성, 전체 ZIP 다운로드를 요청하면 이 도구를 사용한다. 레거시 Rust 배치를 실행하고 생성된 DOCX 전체를 ZIP으로 묶은 뒤 다운로드 URL을 반환한다.",
                    "requestBody": {
                        "required": false,
                        "content": {
                            "application/json": {
                                "schema": {
                                    "type": "object",
                                    "properties": {
                                        "force": { "type": "boolean", "example": true }
                                    }
                                }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "ZIP package created",
                            "content": {
                                "application/json": {
                                    "schema": {
                                        "type": "object",
                                        "properties": {
                                            "workdir": { "type": "string" },
                                            "output_dir": { "type": "string" },
                                            "generated_count": { "type": "integer" },
                                            "zip_path": { "type": "string" },
                                            "zip_file_name": { "type": "string" },
                                            "download_path": { "type": "string" },
                                            "download_url": {
                                                "type": "string",
                                                "description": "사용자에게 그대로 안내할 ZIP 다운로드 URL"
                                            },
                                            "message": {
                                                "type": "string",
                                                "description": "사용자에게 바로 보여줄 짧은 완료 메시지"
                                            },
                                            "assistant_summary": {
                                                "type": "string",
                                                "description": "모델이 그대로 사용자에게 안내해도 되는 자연어 요약"
                                            },
                                            "generated_files_preview": {
                                                "type": "array",
                                                "description": "생성된 파일 예시 일부",
                                                "items": { "type": "string" }
                                            },
                                            "batch_report_path": { "type": ["string", "null"] },
                                            "snapshot_json_path": { "type": ["string", "null"] },
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            },
            "/document/legacy/shortages": {
                "get": {
                    "operationId": "list_shortage_items",
                    "summary": "현재 재고 부족 또는 0 이하인 품목 조회",
                    "description": "사용자가 현재 재고가 없는 품목, 부족한 품목, 구매가 필요한 품목을 물으면 이 도구를 사용한다. 반드시 전처리 완료된 stock_in_out_monthly.json 스냅샷만 기준으로 답하고, 원천 엑셀 파일을 직접 현재 조회 근거로 설명하면 안 된다. 답변할 때 shortage_gap 같은 원시 필드명을 앞세우지 말고, 반드시 '현재고 X개, 필수재고 Y개로 Z개 부족' 형식의 자연어를 사용한다. 응답에 markdown_table 필드가 있으면 그 값을 우선 사용해 마크다운 표로 보여준다.",
                    "parameters": [
                        {
                            "name": "query",
                            "in": "query",
                            "required": false,
                            "schema": { "type": "string" },
                            "description": "품목명 또는 품번 필터"
                        },
                        {
                            "name": "limit",
                            "in": "query",
                            "required": false,
                            "schema": { "type": "integer", "default": 20 },
                            "description": "최대 반환 개수"
                        }
                    ],
                    "responses": {
                        "200": {
                            "description": "Shortage items listed"
                        }
                    }
                }
            },
            "/document/legacy/items": {
                "get": {
                    "operationId": "list_inventory_items",
                    "summary": "전체 품목 인덱스 및 재고/소모 기준 조회",
                    "description": "사용자가 전체 품목 리스트, 재고가 충분한 품목, 재고 상태별 필터, 품번/품명 검색, 재고 매칭 상태, 부품 소모속도 빠른 순 조회를 요청하면 이 도구를 사용한다. 반드시 전처리 완료된 stock_in_out_monthly.json 스냅샷만 기준으로 답하고, 원천 엑셀 파일을 직접 현재 조회 근거로 설명하면 안 된다. 응답의 filter_options는 사용자가 다시 필터링할 수 있는 조건이며, markdown_table 필드가 있으면 그 값을 우선 사용해 마크다운 표로 보여준다.",
                    "parameters": [
                        {
                            "name": "query",
                            "in": "query",
                            "required": false,
                            "schema": { "type": "string" },
                            "description": "품목명 또는 품번 키워드"
                        },
                        {
                            "name": "status",
                            "in": "query",
                            "required": false,
                            "schema": {
                                "type": "string",
                                "enum": ["all", "shortage", "sufficient", "out_of_stock", "unverified", "confirmed"]
                            },
                            "description": "재고 상태 필터. all은 전체, shortage는 부족/재고없음, sufficient는 재고 충분, out_of_stock은 현재고 0 이하, unverified는 재고 미확인, confirmed는 재고 확인 품목"
                        },
                        {
                            "name": "match_status",
                            "in": "query",
                            "required": false,
                            "schema": {
                                "type": "string",
                                "enum": ["matched_all", "stock_inbound", "stock_outbound", "stock_only", "movement_only", "inbound_only", "outbound_only", "unclassified"]
                            },
                            "description": "재고/입고/출고 매칭 상태 필터"
                        },
                        {
                            "name": "sort",
                            "in": "query",
                            "required": false,
                            "schema": {
                                "type": "string",
                                "default": "priority",
                                "enum": ["priority", "consumption", "net_decrease", "shortage", "stock", "name", "outbound"]
                            },
                            "description": "정렬 기준. consumption은 최근 출고합계가 큰 순, net_decrease는 이동 순증감이 낮은 순, shortage는 부족수량 큰 순"
                        },
                        {
                            "name": "limit",
                            "in": "query",
                            "required": false,
                            "schema": { "type": "integer", "default": 50 },
                            "description": "최대 반환 개수"
                        }
                    ],
                    "responses": {
                        "200": {
                            "description": "Inventory items listed"
                        }
                    }
                }
            },
            "/document/legacy/items/report": {
                "get": {
                    "operationId": "export_inventory_report",
                    "summary": "전체 또는 필터된 품목 재고 현황 보고서 파일 생성",
                    "description": "사용자가 전체 품목 리스트, 재고확인상태, 구매 우선순위, 단가가 들어간 문서 파일이나 보고서 파일 생성을 요청하면 이 도구를 사용한다. query/status/match_status/sort/limit 조건은 list_inventory_items와 동일하게 적용된다. 생성 파일에는 품명, 품번, 현재고, 필수재고, 재고확인상태, 매칭상태, 단가, 구매 우선순위, 구매판단, 출고합계, 이동 순증감이 포함된다.",
                    "parameters": [
                        {
                            "name": "query",
                            "in": "query",
                            "required": false,
                            "schema": { "type": "string" },
                            "description": "품목명 또는 품번 키워드"
                        },
                        {
                            "name": "status",
                            "in": "query",
                            "required": false,
                            "schema": {
                                "type": "string",
                                "enum": ["all", "shortage", "sufficient", "out_of_stock", "unverified", "confirmed"]
                            },
                            "description": "재고 상태 필터"
                        },
                        {
                            "name": "match_status",
                            "in": "query",
                            "required": false,
                            "schema": {
                                "type": "string",
                                "enum": ["matched_all", "stock_inbound", "stock_outbound", "stock_only", "movement_only", "inbound_only", "outbound_only", "unclassified"]
                            },
                            "description": "재고/입고/출고 매칭 상태 필터"
                        },
                        {
                            "name": "sort",
                            "in": "query",
                            "required": false,
                            "schema": {
                                "type": "string",
                                "default": "priority",
                                "enum": ["priority", "consumption", "net_decrease", "shortage", "stock", "name", "outbound"]
                            },
                            "description": "정렬 기준"
                        },
                        {
                            "name": "limit",
                            "in": "query",
                            "required": false,
                            "schema": { "type": "integer", "default": 500 },
                            "description": "최대 보고서 행 수"
                        }
                    ],
                    "responses": {
                        "200": {
                            "description": "Inventory report generated"
                        }
                    }
                }
            },
            "/document/legacy/item-context": {
                "get": {
                    "operationId": "get_item_document_context",
                    "summary": "선택한 품목의 문서 작성 컨텍스트 조회",
                    "description": "사용자가 특정 품목으로 구매 품의 문서를 작성하려 할 때 이 도구를 사용한다. 문서 채우기에 필요한 컨텍스트와 필드 seed, 한국어 guided field 목록을 반환한다. 반드시 전처리 완료된 stock_in_out_monthly.json 스냅샷만 기준으로 답하고, 원천 엑셀 파일을 직접 현재 조회 근거로 설명하면 안 된다.",
                    "parameters": [
                        {
                            "name": "part_name",
                            "in": "query",
                            "required": false,
                            "schema": { "type": "string" }
                        },
                        {
                            "name": "part_no",
                            "in": "query",
                            "required": false,
                            "schema": { "type": "string" }
                        }
                    ],
                    "responses": {
                        "200": {
                            "description": "Item context resolved"
                        }
                    }
                }
            },
            "/document/legacy/item-export": {
                "post": {
                    "operationId": "export_single_item_document",
                    "summary": "선택 품목의 단건 구매 품의 문서 생성",
                    "description": "선택한 품목의 seed 필드와 대화형으로 채운 필드를 합쳐 단건 DOCX를 만들고 다운로드 URL을 반환한다. 품명, 품번, 현재고, 입고/출고 이력은 전처리 완료된 stock_in_out_monthly.json 스냅샷 값을 권위값으로 사용한다.",
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": {
                                    "type": "object",
                                    "required": ["fields"],
                                    "properties": {
                                        "part_name": { "type": "string" },
                                        "part_no": { "type": "string" },
                                        "fields": {
                                            "type": "object",
                                            "additionalProperties": true
                                        }
                                    }
                                }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "Single document exported"
                        }
                    }
                }
            },
            "/document/legacy/item-approve": {
                "post": {
                    "operationId": "approve_and_generate_item_document",
                    "summary": "승인/진행 의사 이후 단건 구매 품의 문서 최종 생성",
                    "description": "사용자가 승인해, 진행해 같은 긍정 의사를 보이면 이 도구를 사용한다. 가격 기준 문서 생성 방침을 적용하고, 일반적인 기본값을 채워 초안과 다운로드 URL을 함께 반환한다. 재고와 교체 이력은 전처리 완료된 stock_in_out_monthly.json 스냅샷을 기준으로 덮어쓴다.",
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": {
                                    "type": "object",
                                    "properties": {
                                        "part_name": { "type": "string" },
                                        "part_no": { "type": "string" },
                                        "purchase_reason": { "type": "string" },
                                        "fields": {
                                            "type": "object",
                                            "additionalProperties": true
                                        }
                                    }
                                }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "Approved single document generated"
                        }
                    }
                }
            },
            "/document/pdf/render": {
                "post": {
                    "operationId": "render_markdown_pdf",
                    "summary": "Markdown 보고서를 PDF 파일로 생성",
                    "description": "사용자가 수리 완료 보고서, 업무 보고, 회의록, 분석 결과, 요약문을 PDF 또는 형식이 지정되지 않은 문서 파일로 요청하면 Markdown 본문을 이 도구에 전달해 PDF 다운로드 링크를 생성한다. 사용자가 Word/DOCX 또는 Excel/XLSX를 명시하면 이 도구가 아니라 해당 형식의 렌더링 도구를 호출한다. 제목은 title에만 넣고 markdown 첫 줄에 같은 제목을 반복하지 않는다. 본문은 생성 정보, 개요, 세부 내용, 표/목록, 결론 또는 조치사항 순서로 정리한다. PDF를 직접 생성할 수 없다고 답하지 말고 이 도구를 호출한다. 구매 품의서 자체 DOCX/ZIP 생성은 구매 품의서 전용 도구를 사용하고, 보고용 PDF는 이 도구를 사용한다.",
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": {
                                    "type": "object",
                                    "required": ["title", "markdown"],
                                    "properties": {
                                        "title": {
                                            "type": "string",
                                            "description": "보고서 제목",
                                            "example": "엘리베이터 5기 수리 및 점검 세부 내역 보고서"
                                        },
                                        "markdown": {
                                            "type": "string",
                                            "description": "PDF로 렌더링할 Markdown 보고서 본문"
                                        },
                                        "file_name": {
                                            "type": ["string", "null"],
                                            "description": "선택 PDF 파일명",
                                            "example": "elevator_repair_report.pdf"
                                        },
                                        "page_size": {
                                            "type": ["string", "null"],
                                            "default": "A4",
                                            "enum": ["A4", "Letter", null]
                                        },
                                        "orientation": {
                                            "type": ["string", "null"],
                                            "default": "portrait",
                                            "enum": ["portrait", "landscape", null]
                                        },
                                        "generated_for": {
                                            "type": ["string", "null"],
                                            "description": "문서 생성 대상자 이름. 알고 있는 현재 사용자/요청자 이름을 넣는다."
                                        },
                                        "account_name": {
                                            "type": ["string", "null"],
                                            "description": "문서를 요청한 계정 이름"
                                        },
                                        "account_email": {
                                            "type": ["string", "null"],
                                            "description": "문서를 요청한 계정 이메일"
                                        }
                                    }
                                }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "PDF rendered through the internal Markdown PDF service"
                        }
                    }
                }
            },
            "/document/chat/docx": {
                "post": {
                    "operationId": "render_chat_docx",
                    "summary": "본문 또는 채팅 기록을 Word DOCX 파일로 내보내기",
                    "description": "사용자가 보고서, 요약문, 업무보고, 재고현황 보고서, 현재 대화 내용, 채팅 기록, 이전 답변을 워드 파일, Word 파일, DOCX 문서로 요청하면 작성한 본문을 transcript에 넣거나 messages를 전달해 DOCX 다운로드 링크를 생성한다. 제목은 title에만 넣고 transcript 첫 줄에 같은 제목을 반복하지 않는다. Word 출력에는 Markdown 문법 기호가 남지 않으며 Markdown 표는 실제 Word 표로 렌더링된다. title만 전달하지 않는다. 구매 품의서 템플릿 DOCX 생성에는 사용하지 않는다.",
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": {
                                    "type": "object",
                                    "required": ["title", "transcript"],
                                    "properties": {
                                        "title": {
                                            "type": "string",
                                            "description": "문서 제목"
                                        },
                                        "messages": {
                                            "type": "array",
                                            "description": "내보낼 채팅 메시지 목록",
                                            "items": {
                                                "type": "object",
                                                "required": ["role", "content"],
                                                "properties": {
                                                    "role": { "type": "string" },
                                                    "content": { "type": "string" },
                                                    "name": { "type": ["string", "null"] },
                                                    "created_at": { "type": ["string", "null"] }
                                                }
                                            }
                                        },
                                        "transcript": {
                                            "type": "string",
                                            "description": "Word 문서에 넣을 보고서 본문 또는 messages 대신 사용할 전체 채팅 전문. 빈 값으로 보내지 않는다."
                                        },
                                        "file_name": {
                                            "type": ["string", "null"],
                                            "description": "선택 DOCX 파일명",
                                            "example": "chat_export.docx"
                                        },
                                        "generated_for": {
                                            "type": ["string", "null"],
                                            "description": "문서 생성 대상자 이름. 알고 있는 현재 사용자/요청자 이름을 넣는다."
                                        },
                                        "account_name": {
                                            "type": ["string", "null"],
                                            "description": "문서를 요청한 계정 이름"
                                        },
                                        "account_email": {
                                            "type": ["string", "null"],
                                            "description": "문서를 요청한 계정 이메일"
                                        }
                                    }
                                }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "Chat DOCX rendered through the internal document renderer"
                        }
                    }
                }
            },
            "/document/chat/xlsx": {
                "post": {
                    "operationId": "render_chat_xlsx",
                    "summary": "본문 또는 채팅 기록을 Excel XLSX 파일로 내보내기",
                    "description": "사용자가 보고서, 요약문, 업무보고, 재고현황 보고서, 현재 대화 내용, 채팅 기록, 이전 답변을 엑셀 파일, Excel 파일, XLSX 문서로 요청하면 표 형식 본문을 transcript에 넣거나 messages를 전달해 XLSX 다운로드 링크를 생성한다. 제목은 title에만 넣고 transcript 첫 줄에 같은 제목을 반복하지 않는다. Excel 출력에는 Markdown 문법 기호가 남지 않으며 Markdown 표는 실제 행/열로 렌더링된다. title만 전달하지 않는다.",
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": {
                                    "type": "object",
                                    "required": ["title", "transcript"],
                                    "properties": {
                                        "title": {
                                            "type": "string",
                                            "description": "문서 제목"
                                        },
                                        "messages": {
                                            "type": "array",
                                            "description": "내보낼 채팅 메시지 목록",
                                            "items": {
                                                "type": "object",
                                                "required": ["role", "content"],
                                                "properties": {
                                                    "role": { "type": "string" },
                                                    "content": { "type": "string" },
                                                    "name": { "type": ["string", "null"] },
                                                    "created_at": { "type": ["string", "null"] }
                                                }
                                            }
                                        },
                                        "transcript": {
                                            "type": "string",
                                            "description": "Excel 파일에 넣을 보고서 본문 또는 messages 대신 사용할 전체 채팅 전문. 빈 값으로 보내지 않는다."
                                        },
                                        "file_name": {
                                            "type": ["string", "null"],
                                            "description": "선택 XLSX 파일명",
                                            "example": "chat_export.xlsx"
                                        },
                                        "generated_for": {
                                            "type": ["string", "null"],
                                            "description": "문서 생성 대상자 이름. 알고 있는 현재 사용자/요청자 이름을 넣는다."
                                        },
                                        "account_name": {
                                            "type": ["string", "null"],
                                            "description": "문서를 요청한 계정 이름"
                                        },
                                        "account_email": {
                                            "type": ["string", "null"],
                                            "description": "문서를 요청한 계정 이메일"
                                        }
                                    }
                                }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "Chat XLSX rendered through the internal document renderer"
                        }
                    }
                }
            }
        }
    }))
}

fn app_router() -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/openapi.json", get(openapi_spec))
        .route("/health", get(health))
        .route("/document/create", post(create_document))
        .route("/document/fill", post(fill_document))
        .route("/document/export", post(export_document))
        .route("/document/legacy/run", post(run_legacy_document_batch))
        .route("/document/legacy/download", get(download_legacy_document))
        .route(
            "/document/legacy/package",
            post(generate_legacy_document_package),
        )
        .route("/document/legacy/shortages", get(list_legacy_shortages))
        .route("/document/legacy/items", get(list_legacy_inventory_items))
        .route(
            "/document/legacy/items/report",
            get(export_legacy_inventory_report),
        )
        .route(
            "/document/legacy/item-context",
            get(get_legacy_item_context),
        )
        .route(
            "/document/legacy/item-export",
            post(export_legacy_item_document),
        )
        .route(
            "/document/legacy/item-approve",
            post(approve_legacy_item_document),
        )
        .route("/document/pdf/render", post(render_markdown_pdf_proxy))
        .route("/document/chat/docx", post(render_chat_docx_proxy))
        .route("/document/chat/xlsx", post(render_chat_xlsx_proxy))
        .route("/document/file/download", get(download_markdown_pdf_proxy))
        .route("/document/pdf/download", get(download_markdown_pdf_proxy))
        .with_state(build_state())
        .layer(cors)
        .layer(TraceLayer::new_for_http())
}

fn build_state() -> AppState {
    let templates = HashMap::from([(
        "purchase_request".to_string(),
        TemplateDefinition {
            id: "purchase_request",
            display_name: "구매 품의서",
            required_fields: vec![
                "품명",
                "수량",
                "납품업체",
                "구매사유",
                "담당자 직접입력",
                "부품역할",
            ],
        },
    )]);

    AppState {
        templates: Arc::new(templates),
        sessions: Arc::new(RwLock::new(HashMap::new())),
        download_grants: Arc::new(RwLock::new(HashMap::new())),
        legacy_workdir: std::env::var("PORT_PROJECT_LEGACY_WORKDIR")
            .ok()
            .map(PathBuf::from)
            .or_else(|| Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("DB")))
            .filter(|path| path.exists()),
        public_base_url: std::env::var("DOCUMENT_SERVICE_PUBLIC_BASE_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8001".to_string())
            .trim_end_matches('/')
            .to_string(),
        markdown_pdf_base_url: std::env::var("MARKDOWN_PDF_INTERNAL_BASE_URL")
            .unwrap_or_else(|_| "http://markdown-pdf-service:8003".to_string())
            .trim_end_matches('/')
            .to_string(),
        http_client: reqwest::Client::new(),
    }
}

async fn create_document(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateRequest>,
) -> Result<Json<CreateResponse>, AppError> {
    let _user = require_registered_tool_user(&headers)?;
    let template = template_for(&state, &req.template_id)?;
    let session_id = Uuid::new_v4().to_string();

    let mut fields = extract_fields(template, &req.input_text);
    enrich_purchase_request_from_snapshot(&state, &req.input_text, &mut fields)?;
    let missing_fields = compute_missing_fields(template, &fields);
    let next_question = missing_fields.first().map(|field| next_question_for(field));
    let preview_text = render_preview(template, &fields);

    let session = SessionState {
        session_id: session_id.clone(),
        template_id: template.id.to_string(),
        fields: fields.clone(),
        missing_fields: missing_fields.clone(),
    };

    state
        .sessions
        .write()
        .await
        .insert(session_id.clone(), session);

    Ok(Json(CreateResponse {
        session_id,
        updated_fields: fields,
        missing_fields,
        next_question,
        preview_text,
    }))
}

async fn fill_document(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<FillRequest>,
) -> Result<Json<FillResponse>, AppError> {
    let _user = require_registered_tool_user(&headers)?;
    let template = template_for(&state, &req.template_id)?;
    let mut merged_fields = req.current_fields.clone();

    if let Some(existing) = state.sessions.read().await.get(&req.session_id).cloned() {
        for (key, value) in existing.fields {
            merged_fields.entry(key).or_insert(value);
        }
    }

    let extracted = extract_fields(template, &req.user_message);
    for (key, value) in extracted {
        merged_fields.insert(key, value);
    }

    enrich_purchase_request_from_snapshot(&state, &req.user_message, &mut merged_fields)?;

    let missing_fields = compute_missing_fields(template, &merged_fields);
    let next_question = missing_fields.first().map(|field| next_question_for(field));
    let preview_text = render_preview(template, &merged_fields);

    let session = SessionState {
        session_id: req.session_id.clone(),
        template_id: template.id.to_string(),
        fields: merged_fields.clone(),
        missing_fields: missing_fields.clone(),
    };

    state
        .sessions
        .write()
        .await
        .insert(req.session_id.clone(), session.clone());

    Ok(Json(FillResponse {
        updated_fields: merged_fields,
        missing_fields,
        next_question,
        preview_text,
        session,
    }))
}

async fn export_document(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ExportRequest>,
) -> Result<Json<ExportResponse>, AppError> {
    let _user = require_registered_tool_user(&headers)?;
    let template = template_for(&state, &req.template_id)?;
    let mut fields = req.fields.clone();
    enrich_purchase_request_from_snapshot(&state, "", &mut fields)?;
    let preview_text = render_preview(template, &fields);
    let format = req.format.clone();
    let file_name = format!("{}_{}.{}", template.id, Uuid::new_v4(), format);
    let mime_type = match format.as_str() {
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "json" => "application/json",
        _ => "text/plain",
    }
    .to_string();

    if let Some((template_path, bytes)) =
        try_render_legacy_docx(&fields, &format).map_err(AppError::bad_request)?
    {
        return Ok(Json(ExportResponse {
            file_name,
            format,
            mime_type,
            content_base64: STANDARD.encode(bytes),
            preview_text: format!(
                "{preview_text}\n- export: legacy DOCX template applied from {template_path}"
            ),
            generated_at: Utc::now().to_rfc3339(),
        }));
    }

    let export_payload = serde_json::json!({
        "template_id": template.id,
        "display_name": template.display_name,
        "format": format,
        "fields": fields,
        "preview_text": preview_text.clone(),
        "note": "MVP stub. Legacy DOCX templates were not found in Port-Project templates directory."
    });

    Ok(Json(ExportResponse {
        file_name,
        format,
        mime_type,
        content_base64: STANDARD.encode(export_payload.to_string()),
        preview_text,
        generated_at: Utc::now().to_rfc3339(),
    }))
}

async fn run_legacy_document_batch(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Option<Json<LegacyRunRequest>>,
) -> Result<Json<LegacyRunResponse>, AppError> {
    let _user = require_registered_tool_user(&headers)?;
    let workdir = state
        .legacy_workdir
        .clone()
        .ok_or_else(|| AppError::bad_request("legacy workdir is not configured".into()))?;

    let req = payload
        .map(|Json(body)| body)
        .unwrap_or(LegacyRunRequest { force: true });
    let response = run_legacy_batch_once(&workdir, req.force).await?;
    Ok(Json(response))
}

async fn download_legacy_document(
    State(state): State<AppState>,
    Query(query): Query<LegacyDownloadQuery>,
) -> Result<impl IntoResponse, AppError> {
    let _user = validate_download_grant(&state, &query.path, &query.token).await?;
    let workdir = state
        .legacy_workdir
        .clone()
        .ok_or_else(|| AppError::bad_request("legacy workdir is not configured".into()))?;
    let requested = workdir.join(&query.path);
    let canonical_db = workdir
        .canonicalize()
        .map_err(|err| AppError::bad_request(format!("invalid legacy DB path: {err}")))?;
    let canonical_file = requested
        .canonicalize()
        .map_err(|err| AppError::bad_request(format!("legacy file not found: {err}")))?;

    if !canonical_file.starts_with(&canonical_db) {
        return Err(AppError::bad_request(
            "requested file is outside the legacy DB directory".into(),
        ));
    }

    let bytes = fs::read(&canonical_file)
        .map_err(|err| AppError::bad_request(format!("failed to read legacy file: {err}")))?;
    let file_name = canonical_file
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("download.bin");
    let mime_type = match canonical_file
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
    {
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "json" => "application/json",
        "zip" => "application/zip",
        "csv" => "text/csv; charset=utf-8",
        "txt" | "log" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    };

    Ok((
        [
            (CONTENT_TYPE, HeaderValue::from_static(mime_type)),
            (
                CONTENT_DISPOSITION,
                HeaderValue::from_str(&format!(
                    "attachment; filename*=UTF-8''{}",
                    url_encode_filename(file_name)
                ))
                .map_err(|err| {
                    AppError::bad_request(format!("failed to build content disposition: {err}"))
                })?,
            ),
        ],
        bytes,
    ))
}

async fn generate_legacy_document_package(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Option<Json<LegacyRunRequest>>,
) -> Result<Json<LegacyPackageResponse>, AppError> {
    let user = require_registered_tool_user(&headers)?;
    let workdir = state
        .legacy_workdir
        .clone()
        .ok_or_else(|| AppError::bad_request("legacy workdir is not configured".into()))?;
    let req = payload
        .map(|Json(body)| body)
        .unwrap_or(LegacyRunRequest { force: true });
    let run = run_legacy_batch_once(&workdir, req.force).await?;
    let workdir = PathBuf::from(&run.workdir);
    let zip_file_name = format!(
        "purchase_documents_{}.zip",
        Utc::now().format("%Y%m%d_%H%M%S")
    );
    let zip_relative = PathBuf::from("output")
        .join("packages")
        .join(&zip_file_name);
    let zip_absolute = workdir.join(&zip_relative);

    create_zip_bundle(&workdir, &zip_absolute, &run.generated_files)
        .map_err(|err| AppError::bad_request(format!("failed to create ZIP package: {err}")))?;

    let zip_path = zip_relative.to_string_lossy().to_string();
    let download_path =
        issue_download_path(&state, "/document/legacy/download", &zip_path, &user).await;
    let download_url = format!("{}{download_path}", state.public_base_url);
    let generated_files_preview = run
        .generated_files
        .iter()
        .take(5)
        .cloned()
        .collect::<Vec<_>>();
    let message = format!(
        "구매 품의 문서 {}건을 생성했고 ZIP 파일 {} 준비를 완료했습니다.",
        run.generated_count, zip_file_name
    );
    let assistant_summary = format!(
        "구매 품의 문서 전체 생성이 완료되었습니다. 총 {}건이 생성되었고, ZIP 다운로드 링크는 {} 입니다.",
        run.generated_count, download_url
    );

    Ok(Json(LegacyPackageResponse {
        generated_count: run.generated_count,
        zip_path: zip_path.clone(),
        zip_file_name,
        download_path,
        download_url,
        message,
        assistant_summary,
        generated_files_preview,
        batch_report_path: run.batch_report_path,
        snapshot_json_path: run.snapshot_json_path,
    }))
}

async fn list_legacy_shortages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<LegacyShortagesQuery>,
) -> Result<Json<LegacyShortagesResponse>, AppError> {
    let _user = require_registered_tool_user(&headers)?;
    let snapshot = load_latest_snapshot_json(&state)?;
    let snapshot_json_path = current_snapshot_json_path(&state)?;
    let snapshot_date = snapshot
        .get("meta")
        .and_then(|meta| meta.get("snapshot_date"))
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());
    let parts = snapshot
        .get("parts")
        .and_then(|value| value.as_object())
        .ok_or_else(|| AppError::bad_request("invalid snapshot format: parts missing".into()))?;

    let needle = query.query.as_deref().map(normalize_lookup_text);
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let mut items = parts
        .values()
        .filter_map(build_shortage_item)
        .filter(|item| {
            if let Some(needle) = &needle {
                let hay = normalize_lookup_text(&format!("{} {}", item.part_name, item.part_no));
                hay.contains(needle)
            } else {
                true
            }
        })
        .collect::<Vec<_>>();
    let mut unverified_items = parts
        .values()
        .filter_map(build_unverified_shortage_item)
        .filter(|item| {
            if let Some(needle) = &needle {
                let hay = normalize_lookup_text(&format!("{} {}", item.part_name, item.part_no));
                hay.contains(needle)
            } else {
                true
            }
        })
        .collect::<Vec<_>>();

    items.sort_by(|a, b| {
        a.current_stock
            .unwrap_or(f64::MAX)
            .partial_cmp(&b.current_stock.unwrap_or(f64::MAX))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                a.shortage_gap
                    .unwrap_or(0.0)
                    .partial_cmp(&b.shortage_gap.unwrap_or(0.0))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                b.outbound_qty_sum
                    .partial_cmp(&a.outbound_qty_sum)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.part_name.cmp(&b.part_name))
    });
    unverified_items.sort_by(|a, b| {
        b.outbound_qty_sum
            .partial_cmp(&a.outbound_qty_sum)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                a.movement_net_qty
                    .partial_cmp(&b.movement_net_qty)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.part_name.cmp(&b.part_name))
    });
    let total_count = items.len();
    let unverified_count = unverified_items.len();
    items.truncate(limit);
    unverified_items.truncate(limit);
    let markdown_table = build_confirmed_shortage_markdown_table(&items);
    let unverified_markdown_table = build_unverified_shortage_markdown_table(&unverified_items);

    Ok(Json(LegacyShortagesResponse {
        data_source: "processed_snapshot_json".into(),
        source_policy: "현재 조회는 전처리 완료된 stock_in_out_monthly.json 스냅샷만 기준으로 합니다. 원천 입고/재고/출고 엑셀은 배치 생성용 입력 데이터이며 직접 조회 근거로 사용하지 않습니다.".into(),
        snapshot_json_path,
        snapshot_date,
        total_count,
        unverified_count,
        markdown_table,
        unverified_markdown_table,
        items,
        unverified_items,
    }))
}

async fn list_legacy_inventory_items(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<LegacyInventoryQuery>,
) -> Result<Json<LegacyInventoryResponse>, AppError> {
    let _user = require_registered_tool_user(&headers)?;
    let snapshot = load_latest_snapshot_json(&state)?;
    let snapshot_json_path = current_snapshot_json_path(&state)?;
    let snapshot_date = snapshot
        .get("meta")
        .and_then(|meta| meta.get("snapshot_date"))
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());
    let (items, total_count) = collect_inventory_items(&snapshot, &query, 50, 500)?;
    let markdown_table = build_inventory_markdown_table(&items);
    let returned_count = items.len();

    Ok(Json(LegacyInventoryResponse {
        data_source: "processed_snapshot_json".into(),
        source_policy: "현재 조회는 전처리 완료된 stock_in_out_monthly.json 스냅샷만 기준으로 합니다. 원천 입고/재고/출고 엑셀은 배치 생성용 입력 데이터이며 직접 조회 근거로 사용하지 않습니다.".into(),
        snapshot_json_path,
        snapshot_date,
        total_count,
        returned_count,
        filter_options: LegacyInventoryFilterOptions {
            status: vec![
                "all",
                "shortage",
                "sufficient",
                "out_of_stock",
                "unverified",
                "confirmed",
            ],
            match_status: vec![
                "matched_all",
                "stock_inbound",
                "stock_outbound",
                "stock_only",
                "movement_only",
                "inbound_only",
                "outbound_only",
                "unclassified",
            ],
            sort: vec![
                "priority",
                "consumption",
                "net_decrease",
                "shortage",
                "stock",
                "name",
                "outbound",
            ],
        },
        markdown_table,
        items,
    }))
}

async fn export_legacy_inventory_report(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<LegacyInventoryQuery>,
) -> Result<Json<LegacyInventoryReportResponse>, AppError> {
    let user = require_registered_tool_user(&headers)?;
    let workdir = state
        .legacy_workdir
        .clone()
        .ok_or_else(|| AppError::bad_request("legacy workdir is not configured".into()))?;
    let snapshot = load_latest_snapshot_json(&state)?;
    let (items, _total_count) = collect_inventory_items(&snapshot, &query, 500, 5_000)?;
    let csv = build_inventory_report_csv(&items);

    let file_name = format!(
        "inventory_report_{}.csv",
        Utc::now().format("%Y%m%d_%H%M%S")
    );
    let relative = PathBuf::from("output")
        .join("inventory_reports")
        .join(&file_name);
    let absolute = workdir.join(&relative);
    if let Some(parent) = absolute.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            AppError::bad_request(format!(
                "failed to create inventory report directory: {err}"
            ))
        })?;
    }
    fs::write(&absolute, csv)
        .map_err(|err| AppError::bad_request(format!("failed to write inventory report: {err}")))?;

    let output_path = relative.to_string_lossy().to_string();
    let download_path =
        issue_download_path(&state, "/document/legacy/download", &output_path, &user).await;
    let download_url = format!("{}{download_path}", state.public_base_url);
    let assistant_summary = format!(
        "재고 현황 보고서 파일을 생성했습니다. 총 {}개 품목이 포함되었고, 다운로드 링크는 {} 입니다.",
        items.len(), download_url
    );

    Ok(Json(LegacyInventoryReportResponse {
        output_path,
        download_path,
        download_url,
        file_name,
        generated_count: items.len(),
        assistant_summary,
    }))
}

async fn get_legacy_item_context(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<LegacyItemContextQuery>,
) -> Result<Json<LegacyItemContextResponse>, AppError> {
    let _user = require_registered_tool_user(&headers)?;
    let snapshot_json_path = current_snapshot_json_path(&state)?;
    let part = resolve_snapshot_part(&state, query.part_name.as_deref(), query.part_no.as_deref())?;
    let part_name = part
        .get("part_name")
        .and_then(|value| value.as_str())
        .unwrap_or("기록없음")
        .to_string();
    let part_no = part
        .get("part_no")
        .and_then(|value| value.as_str())
        .unwrap_or(&part_name)
        .to_string();
    let current_stock = part_current_stock(&part);
    let projected_stock_balance = part_projected_stock_balance(&part);
    let required_stock = part_required_stock(&part);
    let available_stock = part_available_stock(&part);
    let shortage_gap = part_shortage_gap(&part);
    let movement_net_qty = part_movement_net_qty(&part);
    let inventory_confirmed = part_inventory_confirmed(&part);
    let inventory_match_status = part_inventory_match_status(&part);
    let inventory_match_label = describe_inventory_match_status(&inventory_match_status);
    let inbound_qty_sum = part
        .get("inbound_qty_sum")
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0);
    let outbound_qty_sum = part
        .get("outbound_qty_sum")
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0);
    let outbound_count = part
        .get("outbound_count")
        .and_then(|value| value.as_u64())
        .unwrap_or(0) as usize;
    let current_stock_text = current_stock
        .map(|value| format!("{value:.0}"))
        .unwrap_or_else(|| "재고 미확인".to_string());
    let projected_stock_text = projected_stock_balance
        .map(|value| format!("{value:.0}"))
        .unwrap_or_else(|| "-".to_string());
    let required_stock_text = required_stock
        .map(|value| format!("{value:.0}"))
        .unwrap_or_else(|| "-".to_string());
    let available_stock_text = available_stock
        .map(|value| format!("{value:.0}"))
        .unwrap_or_else(|| "-".to_string());
    let shortage_gap_text = shortage_gap
        .map(|value| format!("{value:.0}"))
        .unwrap_or_else(|| "-".to_string());
    let stock_state = describe_inventory_state(current_stock, required_stock, inventory_confirmed);

    let context = format!(
        "품목명: {part_name}\n품번: {part_no}\n현재고(재고파일 기준): {current_stock_text}\n필수재고: {required_stock_text}\n가용재고: {available_stock_text}\n과부족(원본): {shortage_gap_text}\n3개년 이동 순증감: {movement_net_qty:.0}\n이력 기반 추정잔량: {projected_stock_text}\n입고 합계: {inbound_qty_sum:.0}\n출고 합계: {outbound_qty_sum:.0}\n출고 건수: {outbound_count}\n재고 매칭 상태: {inventory_match_label} ({inventory_match_status})\n상태: {stock_state}"
    );

    let mut fields_seed = BTreeMap::new();
    merge_snapshot_part_into_fields(&mut fields_seed, &part, Some(snapshot_json_path.as_str()));
    fields_seed.insert(
        "부품역할".into(),
        serde_json::Value::String("(직접입력)".into()),
    );
    fields_seed
        .entry("신규 거래업체".into())
        .or_insert_with(|| serde_json::Value::String("(직접입력)".into()));

    let guided_fields = build_guided_fields_for_purchase_request(&fields_seed);

    let assistant_summary = format!(
        "{} ({}) 품목의 문서 작성 컨텍스트를 준비했습니다. 현재 조회 기준은 전처리 완료된 stock_in_out_monthly.json 스냅샷이며 원천 엑셀을 직접 참조한 답변이 아닙니다. 이제 guided_fields를 기준으로 대화형으로 값을 채운 뒤 단건 문서를 생성하면 됩니다.",
        part_name, part_no,
    );

    Ok(Json(LegacyItemContextResponse {
        part_name,
        part_no,
        context,
        data_source: "processed_snapshot_json".into(),
        source_policy: "현재 조회는 전처리 완료된 stock_in_out_monthly.json 스냅샷만 기준으로 합니다. 원천 입고/재고/출고 엑셀은 배치 생성용 입력 데이터이며 직접 조회 근거로 사용하지 않습니다.".into(),
        snapshot_json_path,
        fields_seed,
        guided_fields,
        assistant_summary,
    }))
}

async fn export_legacy_item_document(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<LegacyItemExportRequest>,
) -> Result<Json<LegacyItemExportResponse>, AppError> {
    let user = require_registered_tool_user(&headers)?;
    let workdir = state
        .legacy_workdir
        .clone()
        .ok_or_else(|| AppError::bad_request("legacy workdir is not configured".into()))?;
    let mut fields = req.fields.clone();
    let selected_part = resolve_snapshot_part_for_document(
        &state,
        &fields,
        req.part_name.as_deref(),
        req.part_no.as_deref(),
    )?;
    let snapshot_json_path = current_snapshot_json_path(&state)?;
    merge_snapshot_part_into_fields(
        &mut fields,
        &selected_part,
        Some(snapshot_json_path.as_str()),
    );

    let (template_path, bytes) = try_render_legacy_docx(&fields, "docx")
        .map_err(AppError::bad_request)?
        .ok_or_else(|| {
            AppError::bad_request("legacy DOCX template could not be resolved".into())
        })?;

    let item_name = as_string(fields.get("품명"), "purchase_request");
    let file_name = format!(
        "single_{}_{}.docx",
        sanitize_filename_for_output(&item_name),
        Utc::now().format("%Y%m%d_%H%M%S")
    );
    let relative = PathBuf::from("output").join("single").join(file_name);
    let absolute = workdir.join(&relative);
    if let Some(parent) = absolute.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            AppError::bad_request(format!("failed to create single output directory: {err}"))
        })?;
    }
    fs::write(&absolute, bytes)
        .map_err(|err| AppError::bad_request(format!("failed to write single document: {err}")))?;

    let output_path = relative.to_string_lossy().to_string();
    let download_path =
        issue_download_path(&state, "/document/legacy/download", &output_path, &user).await;
    let download_url = format!("{}{download_path}", state.public_base_url);
    let assistant_summary = format!(
        "단건 구매 품의 문서를 생성했습니다. 템플릿 경로는 {} 이고, 다운로드 링크는 {} 입니다.",
        template_path, download_url
    );

    Ok(Json(LegacyItemExportResponse {
        output_path,
        download_path,
        download_url,
        file_name: absolute
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("single.docx")
            .to_string(),
        assistant_summary,
    }))
}

async fn approve_legacy_item_document(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<LegacyApproveRequest>,
) -> Result<Json<LegacyApproveResponse>, AppError> {
    let user = require_registered_tool_user(&headers)?;
    let workdir = state
        .legacy_workdir
        .clone()
        .ok_or_else(|| AppError::bad_request("legacy workdir is not configured".into()))?;

    let part = resolve_snapshot_part_for_document(
        &state,
        &req.fields,
        req.part_name.as_deref(),
        req.part_no.as_deref(),
    )?;
    let part_name = part
        .get("part_name")
        .and_then(|value| value.as_str())
        .unwrap_or("기록없음")
        .to_string();
    let part_no = part
        .get("part_no")
        .and_then(|value| value.as_str())
        .unwrap_or(&part_name)
        .to_string();
    let outbound_qty_sum = part
        .get("outbound_qty_sum")
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0);

    let mut fields = req.fields.clone();
    let snapshot_json_path = current_snapshot_json_path(&state)?;
    merge_snapshot_part_into_fields(&mut fields, &part, Some(snapshot_json_path.as_str()));
    fields
        .entry("신규 거래업체".into())
        .or_insert_with(|| serde_json::Value::String("(직접입력)".into()));
    fields
        .entry("제조사".into())
        .or_insert_with(|| serde_json::Value::String("기존 등록 제조사 기준".into()));
    fields
        .entry("단위".into())
        .or_insert_with(|| serde_json::Value::String("EA".into()));
    fields
        .entry("담당자 직접입력".into())
        .or_insert_with(|| serde_json::Value::String("자재관리팀 담당자".into()));
    fields.entry("부품역할".into()).or_insert_with(|| {
        serde_json::Value::String(format!(
            "{}는 설비의 정상 운전을 유지하기 위해 필요한 핵심 부품으로, 현장 장비의 기능 유지와 예방 정비 목적에 사용됩니다. 적기 교체 및 확보를 통해 설비 고장 위험과 비가동 시간을 줄이는 데 활용됩니다.",
            part_name
        ))
    });

    let purchase_reason = req.purchase_reason.unwrap_or_else(|| {
        build_purchase_reason_from_snapshot(
            &part_name,
            part_current_stock(&part),
            part_required_stock(&part),
            part_movement_net_qty(&part),
            outbound_qty_sum,
            part.get("outbound_count")
                .and_then(|value| value.as_u64())
                .unwrap_or(0),
            part_inventory_confirmed(&part),
        )
    });
    fields.insert(
        "구매사유".into(),
        serde_json::Value::String(purchase_reason),
    );

    let pricing_decision = decide_purchase_v2(
        as_f64(fields.get("필수재고량")),
        as_f64(fields.get("현재고")).unwrap_or(0.0),
        as_f64(fields.get("단가")),
    );
    let draft_preview = render_preview(
        &TemplateDefinition {
            id: "purchase_request",
            display_name: "구매 품의서",
            required_fields: vec!["품명", "수량", "납품업체"],
        },
        &fields,
    );

    let (template_path, bytes) = try_render_legacy_docx(&fields, "docx")
        .map_err(AppError::bad_request)?
        .ok_or_else(|| {
            AppError::bad_request("legacy DOCX template could not be resolved".into())
        })?;

    let file_name = format!(
        "approved_{}_{}.docx",
        sanitize_filename_for_output(&part_name),
        Utc::now().format("%Y%m%d_%H%M%S")
    );
    let relative = PathBuf::from("output").join("approved").join(file_name);
    let absolute = workdir.join(&relative);
    if let Some(parent) = absolute.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            AppError::bad_request(format!("failed to create approved output directory: {err}"))
        })?;
    }
    fs::write(&absolute, bytes).map_err(|err| {
        AppError::bad_request(format!("failed to write approved document: {err}"))
    })?;

    let output_path = relative.to_string_lossy().to_string();
    let download_path =
        issue_download_path(&state, "/document/legacy/download", &output_path, &user).await;
    let download_url = format!("{}{download_path}", state.public_base_url);
    let assistant_summary = format!(
        "{} ({}) 품목에 대해 가격 기준 문서 생성 방침을 적용하여 최종 문서를 생성했습니다. 초안을 검토한 뒤 다운로드 링크 {} 에서 파일을 받을 수 있습니다.",
        part_name, part_no, download_url
    );

    Ok(Json(LegacyApproveResponse {
        pricing_policy_note: format!("{} | 템플릿 경로: {}", pricing_decision.note, template_path),
        draft_preview,
        resolved_fields: fields,
        output_path,
        download_path,
        download_url,
        file_name: absolute
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("approved.docx")
            .to_string(),
        assistant_summary,
    }))
}

async fn render_markdown_pdf_proxy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut req): Json<RenderMarkdownPdfRequest>,
) -> Result<Json<RenderMarkdownPdfResponse>, AppError> {
    let user = require_registered_tool_user(&headers)?;
    apply_render_user_defaults(&mut req, &user);
    let endpoint = format!("{}/render/markdown-pdf", state.markdown_pdf_base_url);
    let request = authenticated_upstream_request(state.http_client.post(&endpoint), &user)?;
    let response = request
        .json(&req)
        .send()
        .await
        .map_err(|err| {
            AppError::bad_request(format!("failed to call markdown PDF service: {err}"))
        })?;
    let mut upstream = parse_renderer_response(response, "markdown PDF service").await?;

    if let Some(encoded_path) = extract_markdown_pdf_download_path(&upstream.download_path) {
        let decoded_path = url_decode_query_value(&encoded_path);
        upstream.download_path =
            issue_download_path(&state, "/document/pdf/download", &decoded_path, &user).await;
        upstream.download_url = format!("{}{}", state.public_base_url, upstream.download_path);
        upstream.assistant_summary = format!(
            "{} PDF 파일을 생성했습니다. 다운로드 링크는 {} 입니다.",
            upstream.title, upstream.download_url
        );
    }

    Ok(Json(upstream))
}

async fn render_chat_docx_proxy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<RenderChatDocumentRequest>,
) -> Result<Json<RenderMarkdownPdfResponse>, AppError> {
    let user = require_registered_tool_user(&headers)?;
    render_chat_document_proxy(state, req, "chat-docx", "Word", user).await
}

async fn render_chat_xlsx_proxy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<RenderChatDocumentRequest>,
) -> Result<Json<RenderMarkdownPdfResponse>, AppError> {
    let user = require_registered_tool_user(&headers)?;
    render_chat_document_proxy(state, req, "chat-xlsx", "Excel", user).await
}

async fn render_chat_document_proxy(
    state: AppState,
    mut req: RenderChatDocumentRequest,
    kind: &str,
    label: &str,
    user: RegisteredUser,
) -> Result<Json<RenderMarkdownPdfResponse>, AppError> {
    apply_chat_document_user_defaults(&mut req, &user);
    let has_transcript = req
        .transcript
        .as_deref()
        .map(str::trim)
        .map(|value| !value.is_empty())
        .unwrap_or(false);
    let has_message = req
        .messages
        .iter()
        .any(|message| !message.content.trim().is_empty());
    if !has_transcript && !has_message {
        return Err(AppError::bad_request(format!(
            "{label} document content is required: provide transcript or messages"
        )));
    }

    let endpoint = format!("{}/render/{kind}", state.markdown_pdf_base_url);
    let request = authenticated_upstream_request(state.http_client.post(&endpoint), &user)?;
    let response = request
        .json(&req)
        .send()
        .await
        .map_err(|err| {
            AppError::bad_request(format!("failed to call renderer service: {err}"))
        })?;
    let mut upstream = parse_renderer_response(response, "renderer service").await?;

    if let Some(encoded_path) = extract_markdown_pdf_download_path(&upstream.download_path) {
        let decoded_path = url_decode_query_value(&encoded_path);
        upstream.download_path =
            issue_download_path(&state, "/document/file/download", &decoded_path, &user).await;
        upstream.download_url = format!("{}{}", state.public_base_url, upstream.download_path);
        upstream.assistant_summary = format!(
            "{} {label} 파일을 생성했습니다. 다운로드 링크는 {} 입니다.",
            upstream.title, upstream.download_url
        );
    }

    Ok(Json(upstream))
}

async fn download_markdown_pdf_proxy(
    State(state): State<AppState>,
    Query(query): Query<MarkdownPdfDownloadQuery>,
) -> Result<impl IntoResponse, AppError> {
    let user = validate_download_grant(&state, &query.path, &query.token).await?;
    let endpoint = format!(
        "{}/download?path={}",
        state.markdown_pdf_base_url,
        url_encode_query_value(&query.path)
    );
    let request = authenticated_upstream_request(state.http_client.get(&endpoint), &user)?;
    let response = request
        .send()
        .await
        .map_err(|err| {
            AppError::bad_request(format!("failed to download markdown PDF file: {err}"))
        })?
        .error_for_status()
        .map_err(|err| AppError::bad_request(format!("rendered file download error: {err}")))?;

    let bytes = response.bytes().await.map_err(|err| {
        AppError::bad_request(format!("failed to read rendered file bytes: {err}"))
    })?;
    let file_name = Path::new(&query.path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("download.bin");

    Ok((
        [
            (
                CONTENT_TYPE,
                HeaderValue::from_static(media_type_for_download(file_name)),
            ),
            (
                CONTENT_DISPOSITION,
                HeaderValue::from_str(&format!(
                    "attachment; filename*=UTF-8''{}",
                    url_encode_filename(file_name)
                ))
                .map_err(|err| {
                    AppError::bad_request(format!("failed to build content disposition: {err}"))
                })?,
            ),
        ],
        bytes,
    ))
}

fn template_for<'a>(
    state: &'a AppState,
    template_id: &str,
) -> Result<&'a TemplateDefinition, AppError> {
    state
        .templates
        .get(template_id)
        .ok_or_else(|| AppError::bad_request(format!("unknown template_id: {template_id}")))
}

fn compute_missing_fields(
    template: &TemplateDefinition,
    fields: &BTreeMap<String, serde_json::Value>,
) -> Vec<String> {
    template
        .required_fields
        .iter()
        .filter(|field| match fields.get(**field) {
            Some(serde_json::Value::Null) => true,
            Some(serde_json::Value::String(value)) => value.trim().is_empty(),
            Some(_) => false,
            None => true,
        })
        .map(|field| (*field).to_string())
        .collect()
}

fn extract_fields(
    template: &TemplateDefinition,
    input: &str,
) -> BTreeMap<String, serde_json::Value> {
    let mut fields = BTreeMap::new();

    if template.id == "purchase_request" {
        merge_explicit_field_values(&mut fields, input);
        let quantity_re = Regex::new(r"(\d+)\s*(개|EA|ea)?").unwrap();

        if !fields.contains_key("품명") && input.contains("SSD") {
            fields.insert("품명".into(), serde_json::Value::String("SSD".into()));
        }

        if !fields.contains_key("품명") && input.contains("HDD") {
            fields.insert("품명".into(), serde_json::Value::String("HDD".into()));
        }

        if !fields.contains_key("수량") {
            if let Some(captures) = quantity_re.captures(input) {
                if let Ok(quantity) = captures[1].parse::<u64>() {
                    fields.insert("수량".into(), serde_json::Value::Number(quantity.into()));
                }
            }
        }

        if !fields.contains_key("납품업체") {
            if let Some(vendor) = extract_vendor(input) {
                fields.insert("납품업체".into(), serde_json::Value::String(vendor));
            }
        }
    }

    fields
}

fn merge_explicit_field_values(fields: &mut BTreeMap<String, serde_json::Value>, input: &str) {
    if let Some(json_fields) = extract_json_field_values(input) {
        for (key, value) in json_fields {
            fields.insert(key, value);
        }
    }

    let known_fields = [
        "품명",
        "수량",
        "납품업체",
        "구매사유",
        "담당자 직접입력",
        "부품역할",
        "품번",
        "현재고",
        "필수재고량",
        "가용재고량",
        "과부족",
        "이동순증감",
        "추정잔량",
        "재고확인상태",
        "재고매칭상태",
        "단가",
        "제조사",
        "단위",
        "총 교체수량",
        "교체내역 유무",
        "입고일",
        "사용일",
        "사용처",
        "문제점",
        "교체사유",
        "날짜1",
        "날짜2",
        "날짜3",
        "날짜4",
        "날짜5",
        "날짜6",
        "교체수량1",
        "교체수량2",
        "교체수량3",
        "교체수량4",
        "교체수량5",
        "교체수량6",
        "호기1",
        "호기2",
        "호기3",
        "호기4",
        "호기5",
        "호기6",
    ];

    for raw_line in input.lines() {
        let line = raw_line
            .trim()
            .trim_start_matches(['-', '*', '•', ' '])
            .trim();
        let Some((field, value)) = split_field_line(line) else {
            continue;
        };
        if known_fields.contains(&field) && !value.trim().is_empty() {
            fields.insert(field.to_string(), parse_field_value(value.trim()));
        }
    }
}

fn extract_json_field_values(input: &str) -> Option<BTreeMap<String, serde_json::Value>> {
    let start = input.find('{')?;
    let end = input.rfind('}')?;
    if end <= start {
        return None;
    }
    let parsed = serde_json::from_str::<serde_json::Value>(&input[start..=end]).ok()?;
    let object = parsed
        .get("fields")
        .and_then(|value| value.as_object())
        .or_else(|| parsed.as_object())?;

    let mut out = BTreeMap::new();
    for (key, value) in object {
        if !key.trim().is_empty() && !value.is_null() {
            out.insert(key.trim().to_string(), value.clone());
        }
    }
    Some(out)
}

fn split_field_line(line: &str) -> Option<(&str, &str)> {
    for delimiter in [":", "=", "："] {
        if let Some((field, value)) = line.split_once(delimiter) {
            return Some((field.trim(), value.trim()));
        }
    }
    None
}

fn parse_field_value(value: &str) -> serde_json::Value {
    let normalized = value.replace(',', "");
    if let Ok(number) = normalized.parse::<i64>() {
        return serde_json::Value::Number(number.into());
    }
    if let Ok(number) = normalized.parse::<f64>() {
        if let Some(value) = serde_json::Number::from_f64(number) {
            return serde_json::Value::Number(value);
        }
    }
    serde_json::Value::String(value.to_string())
}

fn extract_vendor(input: &str) -> Option<String> {
    let patterns = [
        r"납품업체는\s*([^\s,.]+)",
        r"업체는\s*([^\s,.]+)",
        r"([A-Za-z0-9가-힣]+)\s*에서\s*구매",
    ];

    for pattern in patterns {
        let re = Regex::new(pattern).unwrap();
        if let Some(captures) = re.captures(input) {
            return Some(captures[1].trim().to_string());
        }
    }

    None
}

fn next_question_for(field: &str) -> String {
    match field {
        "품명" => "어떤 품목을 요청할까요? 품명이나 품번을 알려주면 스냅샷 JSON 기준으로 재고를 자동 반영합니다.".into(),
        "수량" => "수량은 몇 개로 할까요?".into(),
        "납품업체" => "납품업체는 어디로 할까?".into(),
        "구매사유" => "구매사유는 어떻게 적을까요? 재고 부족, 설비 중단 위험, 사용 이력 중 확인된 근거를 알려주세요.".into(),
        "담당자 직접입력" => "담당자는 누구로 적을까요? 모르면 자재관리팀 담당자로 둘 수 있습니다.".into(),
        "부품역할" => "부품역할 칸에 넣을 설명이 필요합니다. 이 부품이 설비에서 하는 역할이나 사용 목적을 알려주세요.".into(),
        _ => format!("{field} 값을 알려주세요."),
    }
}

fn render_preview(
    template: &TemplateDefinition,
    fields: &BTreeMap<String, serde_json::Value>,
) -> String {
    let item = string_or_placeholder(fields.get("품명"));
    let part_no = string_or_placeholder(fields.get("품번"));
    let current_stock = string_or_placeholder(fields.get("현재고"));
    let quantity = string_or_placeholder(fields.get("수량"));
    let vendor = string_or_placeholder(fields.get("납품업체"));
    let inventory_status = string_or_placeholder(fields.get("재고확인상태"));

    format!(
        "[{}]\n- 품명: {}\n- 품번: {}\n- 현재고(재고파일 기준): {}\n- 재고확인상태: {}\n- 수량: {}\n- 납품업체: {}\n{}",
        template.display_name,
        item,
        part_no,
        current_stock,
        inventory_status,
        quantity,
        vendor,
        preview_purchase_note(fields)
    )
}

fn string_or_placeholder(value: Option<&serde_json::Value>) -> String {
    match value {
        Some(serde_json::Value::String(v)) if !v.is_empty() => v.clone(),
        Some(serde_json::Value::Number(v)) => v.to_string(),
        Some(other) if !other.is_null() => other.to_string(),
        _ => "(미입력)".into(),
    }
}

fn preview_purchase_note(fields: &BTreeMap<String, serde_json::Value>) -> String {
    if fields
        .get("재고확인상태")
        .and_then(|value| value.as_str())
        .map(|value| value == "미확인")
        .unwrap_or(false)
    {
        return "- 구매판단: 재고 미확인\n- 자동사유: 현재 재고파일에서 매칭되는 재고 행이 없어 재고를 확정할 수 없습니다. 최근 입출고 이력을 검토한 뒤 구매 여부를 판단해야 합니다.\n".into();
    }
    let decision = decide_purchase_v2(
        as_f64(fields.get("필수재고량")),
        as_f64(fields.get("현재고")).unwrap_or(0.0),
        as_f64(fields.get("단가")),
    );
    let reason = build_purchase_reason_text(&build_legacy_row(fields, &decision));
    format!("- 구매판단: {}\n- 자동사유: {}\n", decision.note, reason)
}

fn as_f64(value: Option<&serde_json::Value>) -> Option<f64> {
    match value {
        Some(serde_json::Value::Number(n)) => n.as_f64(),
        Some(serde_json::Value::String(s)) => s.replace(',', "").trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn as_string(value: Option<&serde_json::Value>, fallback: &str) -> String {
    match value {
        Some(serde_json::Value::String(s)) if !s.trim().is_empty() => s.clone(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        Some(v) if !v.is_null() => v.to_string(),
        _ => fallback.to_string(),
    }
}

fn build_legacy_row(
    fields: &BTreeMap<String, serde_json::Value>,
    decision: &PurchaseDecision,
) -> LegacyDocumentRow {
    let replacement_dates =
        std::array::from_fn(|idx| as_string(fields.get(format!("날짜{}", idx + 1).as_str()), ""));
    let replacement_qtys = std::array::from_fn(|idx| {
        as_string(fields.get(format!("교체수량{}", idx + 1).as_str()), "")
    });
    let replacement_hosts =
        std::array::from_fn(|idx| as_string(fields.get(format!("호기{}", idx + 1).as_str()), ""));
    let item_name = as_string(fields.get("품명"), "기록없음");
    let purchase_qty = as_f64(fields.get("수량")).unwrap_or(1.0).max(1.0);
    let has_replacement_history = fields
        .get("교체내역 유무")
        .and_then(|v| v.as_str())
        .map(|v| v == "유")
        .unwrap_or_else(|| replacement_dates.iter().any(|v| !v.is_empty()));

    LegacyDocumentRow {
        part_key: as_string(fields.get("파트키"), &item_name),
        part_no: as_string(fields.get("품번"), &item_name),
        part_name: item_name,
        received_date: as_string(fields.get("입고일"), "입고기록없음"),
        used_date_last: as_string(fields.get("사용일"), "출고기록없음"),
        used_where: as_string(fields.get("사용처"), "기록없음"),
        usage_reason: as_string(fields.get("문제점"), "기록없음"),
        replacement_reason: as_string(fields.get("교체사유"), "기록없음"),
        current_stock_before: as_f64(fields.get("현재고")).unwrap_or(0.0),
        required_stock: as_f64(fields.get("필수재고량")),
        purchase_qty,
        purchase_order_note: decision.note.clone(),
        issued_qty: as_string(fields.get("총 교체수량"), &format!("{purchase_qty:.0}")),
        replacement_dates,
        replacement_qtys,
        replacement_hosts,
        vendor_name: first_string_field(
            fields,
            &["이전구매업체", "구거래처", "구-거래처", "납품업체"],
            "기록없음",
        ),
        manufacturer_name: as_string(fields.get("제조사"), "기록없음"),
        unit: as_string(fields.get("단위"), "기록없음"),
        unit_price: as_string(fields.get("단가"), "기록없음"),
        part_role: as_string(fields.get("부품역할"), "(직접입력)"),
        template_kind: decision.template_kind,
        has_replacement_history,
    }
}

fn try_render_legacy_docx(
    fields: &BTreeMap<String, serde_json::Value>,
    format: &str,
) -> Result<Option<(String, Vec<u8>)>, String> {
    if format != "docx" {
        return Ok(None);
    }

    let decision = decide_purchase_v2(
        as_f64(fields.get("필수재고량")),
        as_f64(fields.get("현재고")).unwrap_or(0.0),
        as_f64(fields.get("단가")),
    );
    let row = build_legacy_row(fields, &decision);
    let service_root = Path::new(env!("CARGO_MANIFEST_DIR"));

    for candidate in template_dir_candidates(service_root) {
        if let Some(templates) = load_named_templates(&candidate)? {
            let selected = select_template_for_row(&row, &templates);
            let bytes = render_docx_bytes(selected, &row, 1)?;
            return Ok(Some((candidate.display().to_string(), bytes)));
        }
    }

    Ok(None)
}

fn first_string_field(
    fields: &BTreeMap<String, serde_json::Value>,
    keys: &[&str],
    fallback: &str,
) -> String {
    for key in keys {
        let value = as_string(fields.get(*key), "");
        let trimmed = value.trim();
        if !trimmed.is_empty()
            && !matches!(
                trimmed,
                "기록없음" | "(직접입력)" | "(직접기입)" | "(미입력)"
            )
        {
            return value;
        }
    }
    fallback.to_string()
}

#[derive(Debug)]
struct AppError {
    status: StatusCode,
    message: String,
}

impl AppError {
    fn bad_request(message: String) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message,
        }
    }

    fn unauthorized(message: String) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message,
        }
    }

    fn forbidden(message: String) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message,
        }
    }

    fn internal(message: String) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message,
        }
    }
}

fn internal_token_configured() -> bool {
    std::env::var("PORT_PROJECT_INTERNAL_TOKEN")
        .ok()
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

fn configured_internal_token() -> Result<String, AppError> {
    let token = std::env::var("PORT_PROJECT_INTERNAL_TOKEN")
        .unwrap_or_default()
        .trim()
        .to_string();
    if token.is_empty() {
        if cfg!(test) {
            return Ok(String::new());
        }
        return Err(AppError::internal(
            "PORT_PROJECT_INTERNAL_TOKEN is not configured".into(),
        ));
    }
    Ok(token)
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

fn header_string(headers: &HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .trim()
        .to_string()
}

fn require_internal_request(headers: &HeaderMap) -> Result<(), AppError> {
    let expected = configured_internal_token()?;
    if expected.is_empty() && cfg!(test) {
        return Ok(());
    }
    let supplied = header_string(headers, INTERNAL_TOKEN_HEADER);
    if !constant_time_eq(&supplied, &expected) {
        return Err(AppError::forbidden("invalid internal tool token".into()));
    }
    Ok(())
}

fn require_registered_tool_user(headers: &HeaderMap) -> Result<RegisteredUser, AppError> {
    require_internal_request(headers)?;
    if cfg!(test) && !internal_token_configured() {
        return Ok(RegisteredUser {
            id: "unit-test-user".into(),
            email: "unit-test@example.local".into(),
            name: "unit-test".into(),
        });
    }

    let email = header_string(headers, OPEN_WEBUI_USER_EMAIL_HEADER).to_lowercase();
    let id = header_string(headers, OPEN_WEBUI_USER_ID_HEADER);
    let name = header_string(headers, OPEN_WEBUI_USER_NAME_HEADER);
    if email.is_empty() || id.is_empty() {
        return Err(AppError::unauthorized(
            "registered Open WebUI account is required".into(),
        ));
    }

    Ok(RegisteredUser {
        id,
        email: email.clone(),
        name: if name.is_empty() { email } else { name },
    })
}

fn download_token_ttl_seconds() -> i64 {
    std::env::var("PORT_PROJECT_DOWNLOAD_TOKEN_TTL_SECONDS")
        .ok()
        .and_then(|raw| raw.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_DOWNLOAD_TOKEN_TTL_SECONDS)
}

async fn issue_download_path(
    state: &AppState,
    route: &str,
    path: &str,
    user: &RegisteredUser,
) -> String {
    let token = Uuid::new_v4().to_string();
    let expires_at = Utc::now().timestamp() + download_token_ttl_seconds();
    {
        let mut grants = state.download_grants.write().await;
        let now = Utc::now().timestamp();
        grants.retain(|_, grant| grant.expires_at > now);
        grants.insert(
            token.clone(),
            DownloadGrant {
                path: path.to_string(),
                user: user.clone(),
                expires_at,
            },
        );
    }

    format!(
        "{route}?path={}&token={}",
        url_encode_query_value(path),
        url_encode_query_value(&token)
    )
}

async fn validate_download_grant(
    state: &AppState,
    path: &str,
    token: &str,
) -> Result<RegisteredUser, AppError> {
    if token.trim().is_empty() {
        return Err(AppError::unauthorized("download token is required".into()));
    }

    let mut grants = state.download_grants.write().await;
    let now = Utc::now().timestamp();
    grants.retain(|_, grant| grant.expires_at > now);
    let Some(grant) = grants.get(token) else {
        return Err(AppError::forbidden("invalid download token".into()));
    };
    if grant.path != path {
        return Err(AppError::forbidden("invalid download token".into()));
    }
    Ok(grant.user.clone())
}

fn apply_render_user_defaults(req: &mut RenderMarkdownPdfRequest, user: &RegisteredUser) {
    if req.generated_for.as_deref().map(str::trim).unwrap_or("").is_empty() {
        req.generated_for = Some(user.name.clone());
    }
    if req.account_name.as_deref().map(str::trim).unwrap_or("").is_empty() {
        req.account_name = Some(user.name.clone());
    }
    if req.account_email.as_deref().map(str::trim).unwrap_or("").is_empty() {
        req.account_email = Some(user.email.clone());
    }
}

fn apply_chat_document_user_defaults(req: &mut RenderChatDocumentRequest, user: &RegisteredUser) {
    if req.generated_for.as_deref().map(str::trim).unwrap_or("").is_empty() {
        req.generated_for = Some(user.name.clone());
    }
    if req.account_name.as_deref().map(str::trim).unwrap_or("").is_empty() {
        req.account_name = Some(user.name.clone());
    }
    if req.account_email.as_deref().map(str::trim).unwrap_or("").is_empty() {
        req.account_email = Some(user.email.clone());
    }
}

fn authenticated_upstream_request(
    builder: reqwest::RequestBuilder,
    user: &RegisteredUser,
) -> Result<reqwest::RequestBuilder, AppError> {
    Ok(builder
        .header(INTERNAL_TOKEN_HEADER, configured_internal_token()?)
        .header(OPEN_WEBUI_USER_ID_HEADER, user.id.as_str())
        .header(OPEN_WEBUI_USER_EMAIL_HEADER, user.email.as_str())
        .header(OPEN_WEBUI_USER_NAME_HEADER, user.name.as_str()))
}

async fn parse_renderer_response(
    response: reqwest::Response,
    label: &str,
) -> Result<RenderMarkdownPdfResponse, AppError> {
    let status = response.status();
    let body = response.text().await.map_err(|err| {
        AppError::bad_request(format!("failed to read {label} response body: {err}"))
    })?;

    if !status.is_success() {
        return Err(AppError::bad_request(format!(
            "{label} error: HTTP {status}: {body}"
        )));
    }

    serde_json::from_str(&body).map_err(|err| {
        AppError::bad_request(format!(
            "failed to parse {label} response: {err}; body: {body}"
        ))
    })
}

fn url_decode_query_value(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut idx = 0usize;
    while idx < bytes.len() {
        match bytes[idx] {
            b'%' if idx + 2 < bytes.len() => {
                let hi = hex_value(bytes[idx + 1]);
                let lo = hex_value(bytes[idx + 2]);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push((hi << 4) | lo);
                    idx += 3;
                    continue;
                }
                out.push(bytes[idx]);
                idx += 1;
            }
            b'+' => {
                out.push(b' ');
                idx += 1;
            }
            byte => {
                out.push(byte);
                idx += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| value.to_string())
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

async fn run_legacy_batch_once(workdir: &Path, force: bool) -> Result<LegacyRunResponse, AppError> {
    let workdir = workdir.to_path_buf();
    let summary = tokio::task::spawn_blocking(move || {
        legacy_engine::run_batch_once_from_workdir(&workdir, force, false)
    })
    .await
    .map_err(|err| AppError::bad_request(format!("failed to join legacy batch task: {err}")))?
    .map_err(|err| AppError::bad_request(format!("failed to run legacy batch: {err:#}")))?;

    Ok(LegacyRunResponse {
        workdir: summary.workdir.display().to_string(),
        output_dir: summary
            .output_dir
            .strip_prefix(&summary.workdir)
            .unwrap_or(&summary.output_dir)
            .to_string_lossy()
            .to_string(),
        generated_count: summary.generated_count,
        generated_files: summary.generated_files,
        snapshot_json_path: summary.snapshot_json_path,
        batch_report_path: summary.batch_report_path,
        stdout: summary.status_note,
    })
}

fn latest_legacy_output_dir(workdir: &Path) -> Option<PathBuf> {
    let output_root = workdir.join("output");
    let entries = fs::read_dir(output_root).ok()?;
    let mut dated_dirs = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    dated_dirs.sort();
    dated_dirs.pop()
}

fn list_docx_files(root: &Path, workdir: &Path) -> Result<Vec<String>, std::io::Error> {
    let mut stack = vec![root.to_path_buf()];
    let mut files = Vec::new();

    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("docx"))
                .unwrap_or(false)
            {
                if let Ok(relative) = path.strip_prefix(workdir) {
                    files.push(relative.to_string_lossy().to_string());
                }
            }
        }
    }

    files.sort();
    Ok(files)
}

fn latest_matching_file(dir: &Path, prefix: &str, suffix: &str) -> Option<PathBuf> {
    let mut files = fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.is_file())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.starts_with(prefix) && name.ends_with(suffix))
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    files.sort();
    files.pop()
}

fn load_latest_snapshot_json(state: &AppState) -> Result<serde_json::Value, AppError> {
    let workdir = state
        .legacy_workdir
        .clone()
        .ok_or_else(|| AppError::bad_request("legacy workdir is not configured".into()))?;
    let candidate = find_latest_snapshot_json(&workdir).or_else(|| {
        legacy_engine::run_batch_once_from_workdir(&workdir, true, false)
            .ok()
            .and_then(|summary| {
                summary
                    .snapshot_json_path
                    .map(|path| workdir.join(path))
                    .filter(|path| path.exists())
            })
            .or_else(|| find_latest_snapshot_json(&workdir))
    });
    let candidate =
        candidate.ok_or_else(|| AppError::bad_request("stock snapshot json not found".into()))?;
    let raw = fs::read_to_string(&candidate)
        .map_err(|err| AppError::bad_request(format!("failed to read snapshot json: {err}")))?;
    serde_json::from_str(&raw)
        .map_err(|err| AppError::bad_request(format!("failed to parse snapshot json: {err}")))
}

fn current_snapshot_json_path(state: &AppState) -> Result<String, AppError> {
    let workdir = state
        .legacy_workdir
        .clone()
        .ok_or_else(|| AppError::bad_request("legacy workdir is not configured".into()))?;
    let candidate = find_latest_snapshot_json(&workdir).or_else(|| {
        legacy_engine::run_batch_once_from_workdir(&workdir, true, false)
            .ok()
            .and_then(|summary| {
                summary
                    .snapshot_json_path
                    .map(|path| workdir.join(path))
                    .filter(|path| path.exists())
            })
            .or_else(|| find_latest_snapshot_json(&workdir))
    });
    let candidate =
        candidate.ok_or_else(|| AppError::bad_request("stock snapshot json not found".into()))?;
    Ok(candidate.to_string_lossy().to_string())
}

fn enrich_purchase_request_from_snapshot(
    state: &AppState,
    input: &str,
    fields: &mut BTreeMap<String, serde_json::Value>,
) -> Result<(), AppError> {
    let snapshot = load_latest_snapshot_json(state)?;
    let parts = snapshot
        .get("parts")
        .and_then(|value| value.as_object())
        .ok_or_else(|| AppError::bad_request("invalid snapshot format: parts missing".into()))?;
    let Some(part) = find_snapshot_part_for_document(parts, input, fields) else {
        return Ok(());
    };
    let snapshot_json_path = current_snapshot_json_path(state)?;
    merge_snapshot_part_into_fields(fields, &part, Some(snapshot_json_path.as_str()));

    Ok(())
}

fn merge_snapshot_part_into_fields(
    fields: &mut BTreeMap<String, serde_json::Value>,
    part: &serde_json::Value,
    snapshot_json_path: Option<&str>,
) {
    let part_name = part
        .get("part_name")
        .and_then(|value| value.as_str())
        .unwrap_or("기록없음")
        .to_string();
    let part_no = part
        .get("part_no")
        .and_then(|value| value.as_str())
        .unwrap_or(&part_name)
        .to_string();
    let current_stock = part_current_stock(part);
    let projected_stock_balance = part_projected_stock_balance(part);
    let required_stock = part_required_stock(part);
    let available_stock = part_available_stock(part);
    let shortage_gap = part_shortage_gap(part);
    let movement_net_qty = part_movement_net_qty(part);
    let inventory_confirmed = part_inventory_confirmed(part);
    let inventory_match_status = part_inventory_match_status(part);
    let outbound_qty_sum = part
        .get("outbound_qty_sum")
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0);
    let outbound_count = part
        .get("outbound_count")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let inbound_date = part
        .get("inbound_dates")
        .and_then(|value| value.as_array())
        .and_then(|value| value.first())
        .and_then(|value| value.as_str())
        .unwrap_or("입고기록없음")
        .to_string();
    let used_date = part
        .get("outbound_dates")
        .and_then(|value| value.as_array())
        .and_then(|value| value.last())
        .and_then(|value| value.as_str())
        .unwrap_or("출고기록없음")
        .to_string();

    fields.insert("품명".into(), serde_json::Value::String(part_name.clone()));
    fields.insert("품번".into(), serde_json::Value::String(part_no.clone()));
    set_optional_numeric_field(fields, "현재고", current_stock);
    set_optional_numeric_field(fields, "필수재고량", required_stock);
    set_optional_numeric_field(fields, "가용재고량", available_stock);
    set_optional_numeric_field(fields, "과부족", shortage_gap);
    set_optional_numeric_field(fields, "이동순증감", Some(movement_net_qty));
    set_optional_numeric_field(fields, "추정잔량", projected_stock_balance);
    fields.insert(
        "재고확인상태".into(),
        serde_json::Value::String(
            if inventory_confirmed {
                "확인"
            } else {
                "미확인"
            }
            .into(),
        ),
    );
    fields.insert(
        "재고매칭상태".into(),
        serde_json::Value::String(describe_inventory_match_status(&inventory_match_status).into()),
    );
    fields.insert("총 교체수량".into(), serde_json::json!(outbound_qty_sum));
    fields.insert(
        "교체내역 유무".into(),
        serde_json::Value::String(if outbound_count > 0 { "유" } else { "무" }.into()),
    );
    fields.insert("입고일".into(), serde_json::Value::String(inbound_date));
    fields.insert("사용일".into(), serde_json::Value::String(used_date));

    if let Some(document_context) = part
        .get("document_context")
        .and_then(|value| value.as_object())
    {
        if let Some(vendor_name) = snapshot_context_text(document_context.get("vendor_name")) {
            fields
                .entry("이전구매업체".into())
                .or_insert_with(|| serde_json::Value::String(vendor_name.clone()));
            fields
                .entry("구거래처".into())
                .or_insert_with(|| serde_json::Value::String(vendor_name.clone()));
            fields
                .entry("납품업체".into())
                .or_insert_with(|| serde_json::Value::String(vendor_name));
        }
        if let Some(manufacturer_name) =
            snapshot_context_text(document_context.get("manufacturer_name"))
        {
            fields
                .entry("제조사".into())
                .or_insert_with(|| serde_json::Value::String(manufacturer_name));
        }
        if let Some(unit) = snapshot_context_text(document_context.get("unit")) {
            fields
                .entry("단위".into())
                .or_insert_with(|| serde_json::Value::String(unit));
        }
        if let Some(unit_price) = document_context
            .get("unit_price")
            .and_then(snapshot_context_number)
        {
            fields
                .entry("단가".into())
                .or_insert_with(|| serde_json::json!(unit_price));
        }
        if let Some(received_date) = snapshot_context_text(document_context.get("received_date")) {
            fields.insert("입고일".into(), serde_json::Value::String(received_date));
        }
        if let Some(used_date_last) = snapshot_context_text(document_context.get("used_date_last"))
        {
            fields.insert("사용일".into(), serde_json::Value::String(used_date_last));
        }
        if let Some(used_where) = snapshot_context_text(document_context.get("used_where")) {
            fields.insert("사용처".into(), serde_json::Value::String(used_where));
        }
        if let Some(usage_reason) = snapshot_context_text(document_context.get("usage_reason")) {
            fields.insert("문제점".into(), serde_json::Value::String(usage_reason));
        }
        if let Some(replacement_reason) =
            snapshot_context_text(document_context.get("replacement_reason"))
        {
            fields.insert(
                "교체사유".into(),
                serde_json::Value::String(replacement_reason),
            );
        }
        if let Some(issued_qty) = snapshot_context_text(document_context.get("issued_qty")) {
            fields.insert("총 교체수량".into(), parse_field_value(&issued_qty));
        }
        if let Some(has_replacement_history) = document_context
            .get("has_replacement_history")
            .and_then(|value| value.as_bool())
        {
            fields.insert(
                "교체내역 유무".into(),
                serde_json::Value::String(
                    if has_replacement_history {
                        "유"
                    } else {
                        "무"
                    }
                    .into(),
                ),
            );
        }

        for (json_key, field_prefix) in [
            ("replacement_dates", "날짜"),
            ("replacement_qtys", "교체수량"),
            ("replacement_hosts", "호기"),
        ] {
            if let Some(values) = document_context
                .get(json_key)
                .and_then(|value| value.as_array())
            {
                for (idx, raw) in values.iter().take(6).enumerate() {
                    if let Some(value) = snapshot_context_text(Some(raw)) {
                        fields.insert(
                            format!("{field_prefix}{}", idx + 1),
                            serde_json::Value::String(value),
                        );
                    }
                }
            }
        }
    }

    fields
        .entry("수량".into())
        .or_insert_with(|| serde_json::json!(1));
    fields.insert(
        "재고데이터기준".into(),
        serde_json::Value::String("stock_in_out_monthly.json".into()),
    );
    if let Some(path) = snapshot_json_path {
        fields.insert(
            "재고스냅샷경로".into(),
            serde_json::Value::String(path.to_string()),
        );
    }
    fields.entry("구매사유".into()).or_insert_with(|| {
        serde_json::Value::String(build_purchase_reason_from_snapshot(
            &part_name,
            current_stock,
            required_stock,
            movement_net_qty,
            outbound_qty_sum,
            outbound_count,
            inventory_confirmed,
        ))
    });
}

fn build_guided_fields_for_purchase_request(
    fields: &BTreeMap<String, serde_json::Value>,
) -> Vec<GuidedFieldSpec> {
    let has_replacement_history = fields
        .get("교체내역 유무")
        .and_then(|value| value.as_str())
        .map(|value| value == "유")
        .unwrap_or(false);
    let is_over_500k = as_f64(fields.get("단가")).unwrap_or(0.0) >= 500_000.0;

    let specs: &[(&str, &str, &str)] = match (is_over_500k, has_replacement_history) {
        (true, true) => &[
            (
                "구매사유",
                "구매 사유 보강",
                "이 부품이 왜 지금 필요한지, 재고와 교체 이력을 근거로 정리해줘.",
            ),
            (
                "담당자 직접입력",
                "담당자 확인",
                "이 문서를 검토하거나 설명할 담당자 정보를 정리해줘.",
            ),
            (
                "납품업체",
                "업체/거래처 확인",
                "구매 예정 업체나 비교 가능한 공급업체 정보를 정리해줘.",
            ),
            (
                "부품역할",
                "부품 설명",
                "이 부품의 기능과 현장 사용 목적을 문서 본문용으로 정리해줘.",
            ),
        ],
        (true, false) => &[
            (
                "구매사유",
                "구매 사유 보강",
                "교체 이력은 적지만 구매가 필요한 이유를 재고 기준으로 정리해줘.",
            ),
            (
                "담당자 직접입력",
                "담당자 확인",
                "문서 담당자 또는 설명 가능한 담당자를 정리해줘.",
            ),
            (
                "납품업체",
                "업체/거래처 확인",
                "공급업체나 견적 대상 업체 정보를 정리해줘.",
            ),
            (
                "부품역할",
                "부품 설명",
                "부품의 핵심 기능과 대체 불가능성을 짧게 정리해줘.",
            ),
        ],
        (false, true) => &[
            (
                "구매사유",
                "구매 사유",
                "소액 구매 문서에 맞게 구매 필요성을 간단명료하게 정리해줘.",
            ),
            (
                "담당자 직접입력",
                "담당자 확인",
                "담당자 이름이나 부서를 정리해줘.",
            ),
            (
                "납품업체",
                "업체/거래처 확인",
                "현재 거래 예정 업체를 간단히 적어줘.",
            ),
            (
                "부품역할",
                "부품 설명",
                "이 부품이 장비에서 하는 역할을 짧게 정리해줘.",
            ),
        ],
        (false, false) => &[
            (
                "구매사유",
                "구매 사유",
                "이 부품이 왜 필요한지 핵심만 짧게 정리해줘.",
            ),
            (
                "담당자 직접입력",
                "담당자 확인",
                "담당자 이름이나 부서를 정리해줘.",
            ),
            (
                "납품업체",
                "업체/거래처 확인",
                "구매할 업체나 공급처를 정리해줘.",
            ),
            (
                "부품역할",
                "부품 설명",
                "부품의 사용 목적을 한두 문장으로 정리해줘.",
            ),
        ],
    };

    specs
        .iter()
        .map(|(field, label, prompt)| GuidedFieldSpec {
            field: (*field).to_string(),
            label: (*label).to_string(),
            prompt: (*prompt).to_string(),
        })
        .collect()
}

fn snapshot_context_number(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(number) => number.as_f64(),
        serde_json::Value::String(text) => text.replace(',', "").trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn snapshot_context_text(value: Option<&serde_json::Value>) -> Option<String> {
    let raw = match value {
        Some(serde_json::Value::String(text)) => text.trim().to_string(),
        Some(serde_json::Value::Number(number)) => number.to_string(),
        _ => return None,
    };
    if raw.is_empty() || matches!(raw.as_str(), "기록없음" | "출고기록없음" | "입고기록없음")
    {
        return None;
    }
    Some(raw)
}

fn part_current_stock(part: &serde_json::Value) -> Option<f64> {
    part.get("current_stock_before")
        .and_then(snapshot_context_number)
}

fn part_projected_stock_balance(part: &serde_json::Value) -> Option<f64> {
    part.get("current_stock_updated")
        .and_then(snapshot_context_number)
        .or_else(|| part_current_stock(part).map(|current| current + part_movement_net_qty(part)))
}

fn part_required_stock(part: &serde_json::Value) -> Option<f64> {
    part.get("required_stock")
        .and_then(snapshot_context_number)
        .or_else(|| {
            part.get("document_context")
                .and_then(|value| value.get("required_stock"))
                .and_then(snapshot_context_number)
        })
}

fn part_available_stock(part: &serde_json::Value) -> Option<f64> {
    part.get("available_stock_qty")
        .and_then(snapshot_context_number)
}

fn part_unit_price(part: &serde_json::Value) -> Option<f64> {
    part.get("unit_price")
        .and_then(snapshot_context_number)
        .or_else(|| {
            part.get("document_context")
                .and_then(|value| value.get("unit_price"))
                .and_then(snapshot_context_number)
        })
}

fn part_shortage_gap(part: &serde_json::Value) -> Option<f64> {
    part.get("shortage_gap")
        .and_then(snapshot_context_number)
        .or_else(
            || match (part_current_stock(part), part_required_stock(part)) {
                (Some(current), Some(required)) => Some(current - required),
                _ => None,
            },
        )
}

fn part_shortage_quantity(part: &serde_json::Value) -> Option<f64> {
    part_shortage_gap(part).and_then(|gap| if gap < 0.0 { Some(-gap) } else { None })
}

fn part_movement_net_qty(part: &serde_json::Value) -> f64 {
    part.get("movement_net_qty")
        .and_then(snapshot_context_number)
        .unwrap_or_else(|| {
            part.get("inbound_qty_sum")
                .and_then(snapshot_context_number)
                .unwrap_or(0.0)
                - part
                    .get("outbound_qty_sum")
                    .and_then(snapshot_context_number)
                    .unwrap_or(0.0)
        })
}

fn part_inventory_confirmed(part: &serde_json::Value) -> bool {
    part.get("inventory_confirmed")
        .and_then(|value| value.as_bool())
        .unwrap_or_else(|| {
            part.get("stock_row_idx")
                .and_then(|value| value.as_array())
                .map(|rows| !rows.is_empty())
                .unwrap_or_else(|| part_current_stock(part).is_some())
        })
}

fn part_inventory_match_status(part: &serde_json::Value) -> String {
    if let Some(status) = part
        .get("inventory_match_status")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return status.to_string();
    }

    let has_stock = part_inventory_confirmed(part);
    let has_inbound = part
        .get("inbound_row_idx")
        .and_then(|value| value.as_array())
        .map(|rows| !rows.is_empty())
        .unwrap_or_else(|| {
            part.get("inbound_qty_sum")
                .and_then(snapshot_context_number)
                .unwrap_or(0.0)
                > 0.0
        });
    let has_outbound = part
        .get("outbound_row_idx")
        .and_then(|value| value.as_array())
        .map(|rows| !rows.is_empty())
        .unwrap_or_else(|| {
            part.get("outbound_qty_sum")
                .and_then(snapshot_context_number)
                .unwrap_or(0.0)
                > 0.0
        });

    match (has_stock, has_inbound, has_outbound) {
        (true, true, true) => "matched_all",
        (true, true, false) => "stock_inbound",
        (true, false, true) => "stock_outbound",
        (true, false, false) => "stock_only",
        (false, true, true) => "movement_only",
        (false, true, false) => "inbound_only",
        (false, false, true) => "outbound_only",
        (false, false, false) => "unclassified",
    }
    .to_string()
}

fn describe_inventory_match_status(status: &str) -> &'static str {
    match status {
        "matched_all" => "재고/입고/출고 모두 매칭",
        "stock_inbound" => "재고/입고 매칭",
        "stock_outbound" => "재고/출고 매칭",
        "stock_only" => "재고만 매칭",
        "movement_only" => "입출고만 매칭",
        "inbound_only" => "입고만 매칭",
        "outbound_only" => "출고만 매칭",
        _ => "미분류",
    }
}

fn describe_inventory_state(
    current_stock: Option<f64>,
    required_stock: Option<f64>,
    inventory_confirmed: bool,
) -> String {
    if !inventory_confirmed {
        return "재고 미확인".into();
    }

    let Some(current_stock) = current_stock else {
        return "재고 미확인".into();
    };

    if current_stock <= 0.0 {
        return "재고 없음".into();
    }

    if let Some(required_stock) = required_stock {
        if current_stock < required_stock {
            return "재고 부족".into();
        }
    }

    "재고 확인".into()
}

fn set_optional_numeric_field(
    fields: &mut BTreeMap<String, serde_json::Value>,
    key: &str,
    value: Option<f64>,
) {
    if let Some(value) = value {
        fields.insert(key.to_string(), serde_json::json!(value));
    } else {
        fields.remove(key);
    }
}

fn build_purchase_reason_from_snapshot(
    part_name: &str,
    current_stock: Option<f64>,
    required_stock: Option<f64>,
    movement_net_qty: f64,
    outbound_qty_sum: f64,
    outbound_count: u64,
    inventory_confirmed: bool,
) -> String {
    if !inventory_confirmed {
        return format!(
            "{} 품목은 현재 재고파일에서 매칭되는 재고 행이 없어 현재고를 확정할 수 없습니다. 최근 3개년 출고 {:.0}개(출고 {}건), 이동 순증감 {:.0}개가 확인되어 재고 현황 재확인 후 구매 필요 여부를 검토해야 합니다.",
            part_name, outbound_qty_sum, outbound_count, movement_net_qty
        );
    }

    let current_stock = current_stock.unwrap_or(0.0);
    if let Some(required_stock) = required_stock {
        let shortage_gap = current_stock - required_stock;
        format!(
            "{} 품목은 재고파일 기준 현재고 {:.0}개, 필수재고 {:.0}개, 과부족 {:.0}개이며 최근 3개년 출고 {:.0}개(출고 {}건), 이동 순증감 {:.0}개가 확인되었습니다. 재고 부족 또는 소진으로 인한 설비 운영 차질 방지를 위해 구매 검토가 필요합니다.",
            part_name,
            current_stock,
            required_stock,
            shortage_gap,
            outbound_qty_sum,
            outbound_count,
            movement_net_qty,
        )
    } else {
        format!(
            "{} 품목은 재고파일 기준 현재고 {:.0}개이며 최근 3개년 출고 {:.0}개(출고 {}건), 이동 순증감 {:.0}개가 확인되었습니다. 사용 이력과 재고 수준을 근거로 구매 필요 여부를 검토해야 합니다.",
            part_name, current_stock, outbound_qty_sum, outbound_count, movement_net_qty
        )
    }
}

fn find_snapshot_part_for_document(
    parts: &serde_json::Map<String, serde_json::Value>,
    input: &str,
    fields: &BTreeMap<String, serde_json::Value>,
) -> Option<serde_json::Value> {
    let part_name = meaningful_lookup_text(fields.get("품명"));
    let part_no = meaningful_lookup_text(fields.get("품번"));
    if let Some(found) =
        score_snapshot_part_candidates(parts, part_name.as_deref(), part_no.as_deref(), None)
    {
        return Some(found);
    }

    let normalized_input = normalize_lookup_text(input);
    if normalized_input.len() < 2 {
        return None;
    }

    score_snapshot_part_candidates(parts, None, None, Some(&normalized_input))
}

fn meaningful_lookup_text(value: Option<&serde_json::Value>) -> Option<String> {
    let raw = match value {
        Some(serde_json::Value::String(s)) => s.trim().to_string(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        _ => return None,
    };
    if raw.is_empty() {
        return None;
    }
    let normalized = normalize_lookup_text(&raw);
    if normalized.is_empty() {
        return None;
    }
    if matches!(
        normalized.as_str(),
        "미입력" | "직접입력" | "기록없음" | "출고기록없음" | "입고기록없음"
    ) {
        return None;
    }
    Some(raw)
}

fn score_snapshot_part_candidates(
    parts: &serde_json::Map<String, serde_json::Value>,
    part_name: Option<&str>,
    part_no: Option<&str>,
    normalized_input: Option<&str>,
) -> Option<serde_json::Value> {
    let name_needle = part_name.map(normalize_lookup_text);
    let no_needle = part_no.map(normalize_lookup_text);
    let mut best_score = 0usize;
    let mut best_value: Option<serde_json::Value> = None;

    for value in parts.values() {
        let candidate_name = value
            .get("part_name")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let candidate_no = value.get("part_no").and_then(|v| v.as_str()).unwrap_or("");
        let normalized_name = normalize_lookup_text(candidate_name);
        let normalized_no = normalize_lookup_text(candidate_no);
        if normalized_name.is_empty() && normalized_no.is_empty() {
            continue;
        }

        let mut score = 0usize;
        if let Some(needle) = &no_needle {
            if normalized_no == *needle {
                score = score.max(10_000 + normalized_no.len());
            } else if normalized_no.contains(needle) || needle.contains(&normalized_no) {
                score = score.max(8_000 + needle.len());
            }
        }
        if let Some(needle) = &name_needle {
            if normalized_name == *needle {
                score = score.max(9_000 + normalized_name.len());
            } else if normalized_name.contains(needle) || needle.contains(&normalized_name) {
                score = score.max(7_000 + needle.len());
            }
        }
        if let Some(input) = normalized_input {
            if !normalized_no.is_empty() && input.contains(&normalized_no) {
                score = score.max(6_000 + normalized_no.len());
            }
            if !normalized_name.is_empty() && input.contains(&normalized_name) {
                score = score.max(5_000 + normalized_name.len());
            }
            if input.len() >= 4 && !normalized_no.is_empty() && normalized_no.contains(input) {
                score = score.max(3_000 + input.len());
            }
            if input.len() >= 4 && !normalized_name.is_empty() && normalized_name.contains(input) {
                score = score.max(2_000 + input.len());
            }
        }

        if score > best_score {
            best_score = score;
            best_value = Some(value.clone());
        }
    }

    best_value
}

fn find_latest_snapshot_json(workdir: &Path) -> Option<PathBuf> {
    let direct = workdir.join("output").join("stock_in_out_monthly.json");
    if direct.exists() {
        return Some(direct);
    }

    latest_matching_file(&workdir.join("output"), "stock_in_out_monthly", ".json")
}

fn build_shortage_item(part: &serde_json::Value) -> Option<LegacyShortageItem> {
    let part_name = part.get("part_name")?.as_str()?.trim().to_string();
    let part_no = part
        .get("part_no")
        .and_then(|value| value.as_str())
        .unwrap_or(&part_name)
        .trim()
        .to_string();
    let current_stock = part_current_stock(part);
    let required_stock = part_required_stock(part);
    let available_stock = part_available_stock(part);
    let shortage_gap = part_shortage_gap(part);
    let shortage_quantity = part_shortage_quantity(part);
    let projected_stock_balance = part_projected_stock_balance(part);
    let movement_net_qty = part_movement_net_qty(part);
    let inventory_confirmed = part_inventory_confirmed(part);
    let inventory_match_status = part_inventory_match_status(part);
    let inbound_qty_sum = part
        .get("inbound_qty_sum")
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0);
    let outbound_qty_sum = part
        .get("outbound_qty_sum")
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0);
    let outbound_count = part
        .get("outbound_count")
        .and_then(|value| value.as_u64())
        .unwrap_or(0) as usize;
    let unit_price = part_unit_price(part);

    if !inventory_confirmed {
        return None;
    }

    let current_value = current_stock?;
    let is_shortage = current_value <= 0.0
        || shortage_gap.map(|value| value < 0.0).unwrap_or(false)
        || required_stock
            .map(|required| current_value < required)
            .unwrap_or(false);
    if !is_shortage {
        return None;
    }

    let stock_status = describe_inventory_state(current_stock, required_stock, inventory_confirmed);
    let summary = match (required_stock, shortage_quantity) {
        (Some(required_stock), Some(shortage_quantity)) => format!(
            "{} ({})는 재고파일 기준 현재고 {:.0}개, 필수재고 {:.0}개라서 {:.0}개가 부족한 상태입니다. 최근 3개년 이동 순증감은 {:.0}개입니다.",
            part_name, part_no, current_value, required_stock, shortage_quantity, movement_net_qty
        ),
        (Some(required_stock), None) => format!(
            "{} ({})는 재고파일 기준 현재고 {:.0}개, 필수재고 {:.0}개이며 구매 검토가 필요한 상태입니다. 최근 3개년 이동 순증감은 {:.0}개입니다.",
            part_name, part_no, current_value, required_stock, movement_net_qty
        ),
        (None, _) => format!(
            "{} ({})는 재고파일 기준 현재고 {:.0}개이며 최근 3개년 출고합계 {:.0}개, 이동 순증감 {:.0}개가 확인되어 구매 검토가 필요한 상태입니다.",
            part_name, part_no, current_value, outbound_qty_sum, movement_net_qty
        ),
    };
    let document_request_hint = format!("{part_name} ({part_no}) 품목으로 구매 품의 문서 작성해줘");
    let purchase_priority = determine_purchase_priority(
        current_stock,
        required_stock,
        inventory_confirmed,
        outbound_qty_sum,
        movement_net_qty,
    );
    let purchase_decision =
        decide_purchase_v2(required_stock, current_stock.unwrap_or(0.0), unit_price);

    Some(LegacyShortageItem {
        part_name,
        part_no,
        current_stock,
        required_stock,
        available_stock,
        shortage_gap,
        shortage_quantity,
        projected_stock_balance,
        movement_net_qty,
        inbound_qty_sum,
        outbound_qty_sum,
        outbound_count,
        inventory_confirmed,
        inventory_match_status,
        stock_status,
        unit_price,
        purchase_priority,
        purchase_policy_note: purchase_decision.note,
        summary,
        document_request_hint,
    })
}

fn build_unverified_shortage_item(part: &serde_json::Value) -> Option<LegacyShortageItem> {
    let part_name = part.get("part_name")?.as_str()?.trim().to_string();
    let part_no = part
        .get("part_no")
        .and_then(|value| value.as_str())
        .unwrap_or(&part_name)
        .trim()
        .to_string();
    let inventory_confirmed = part_inventory_confirmed(part);
    if inventory_confirmed {
        return None;
    }

    let inbound_qty_sum = part
        .get("inbound_qty_sum")
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0);
    let outbound_qty_sum = part
        .get("outbound_qty_sum")
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0);
    let outbound_count = part
        .get("outbound_count")
        .and_then(|value| value.as_u64())
        .unwrap_or(0) as usize;
    let movement_net_qty = part_movement_net_qty(part);
    if outbound_qty_sum <= 0.0 && outbound_count == 0 && movement_net_qty >= 0.0 {
        return None;
    }

    let inventory_match_status = part_inventory_match_status(part);
    let summary = format!(
        "{} ({})는 재고파일과 매칭되는 재고 행이 없어 현재고를 확정할 수 없습니다. 최근 3개년 입고 {:.0}, 출고 {:.0} (출고 {}건), 이동 순증감 {:.0}이 확인되어 재고 현황 재확인이 필요합니다.",
        part_name, part_no, inbound_qty_sum, outbound_qty_sum, outbound_count, movement_net_qty
    );
    let document_request_hint = format!(
        "{part_name} ({part_no}) 품목은 재고 미확인 상태라 스냅샷과 원본 재고 매핑부터 확인해줘"
    );

    Some(LegacyShortageItem {
        part_name,
        part_no,
        current_stock: None,
        required_stock: part_required_stock(part),
        available_stock: part_available_stock(part),
        shortage_gap: part_shortage_gap(part),
        shortage_quantity: part_shortage_quantity(part),
        projected_stock_balance: part_projected_stock_balance(part),
        movement_net_qty,
        inbound_qty_sum,
        outbound_qty_sum,
        outbound_count,
        inventory_confirmed: false,
        inventory_match_status,
        stock_status: "재고 미확인".into(),
        unit_price: part_unit_price(part),
        purchase_priority: "확인 필요".into(),
        purchase_policy_note: "재고 미확인: 재고 행 매칭 후 구매 여부 판단 필요".into(),
        summary,
        document_request_hint,
    })
}

fn collect_inventory_items(
    snapshot: &serde_json::Value,
    query: &LegacyInventoryQuery,
    default_limit: usize,
    max_limit: usize,
) -> Result<(Vec<LegacyShortageItem>, usize), AppError> {
    let parts = snapshot
        .get("parts")
        .and_then(|value| value.as_object())
        .ok_or_else(|| AppError::bad_request("invalid snapshot format: parts missing".into()))?;
    let needle = query.query.as_deref().map(normalize_lookup_text);
    let status_filter = query.status.as_deref().unwrap_or("all").trim();
    let match_status_filter = query.match_status.as_deref().map(str::trim);
    let sort = query.sort.as_deref().unwrap_or("priority").trim();
    let limit = query.limit.unwrap_or(default_limit).clamp(1, max_limit);

    let mut items = parts
        .values()
        .filter_map(build_inventory_item)
        .filter(|item| inventory_item_matches_query(item, needle.as_deref()))
        .filter(|item| inventory_item_matches_status(item, status_filter))
        .filter(|item| {
            match_status_filter
                .map(|status| item.inventory_match_status == status)
                .unwrap_or(true)
        })
        .collect::<Vec<_>>();

    sort_inventory_items(&mut items, sort);
    let total_count = items.len();
    items.truncate(limit);
    Ok((items, total_count))
}

fn build_inventory_item(part: &serde_json::Value) -> Option<LegacyShortageItem> {
    let part_name = part.get("part_name")?.as_str()?.trim().to_string();
    let part_no = part
        .get("part_no")
        .and_then(|value| value.as_str())
        .unwrap_or(&part_name)
        .trim()
        .to_string();
    let current_stock = part_current_stock(part);
    let required_stock = part_required_stock(part);
    let available_stock = part_available_stock(part);
    let shortage_gap = part_shortage_gap(part);
    let shortage_quantity = part_shortage_quantity(part);
    let projected_stock_balance = part_projected_stock_balance(part);
    let movement_net_qty = part_movement_net_qty(part);
    let inventory_confirmed = part_inventory_confirmed(part);
    let inventory_match_status = part_inventory_match_status(part);
    let inbound_qty_sum = part
        .get("inbound_qty_sum")
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0);
    let outbound_qty_sum = part
        .get("outbound_qty_sum")
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0);
    let outbound_count = part
        .get("outbound_count")
        .and_then(|value| value.as_u64())
        .unwrap_or(0) as usize;
    let stock_status = describe_inventory_state(current_stock, required_stock, inventory_confirmed);
    let unit_price = part_unit_price(part);
    let purchase_decision =
        decide_purchase_v2(required_stock, current_stock.unwrap_or(0.0), unit_price);
    let purchase_priority = determine_purchase_priority(
        current_stock,
        required_stock,
        inventory_confirmed,
        outbound_qty_sum,
        movement_net_qty,
    );

    let summary = if !inventory_confirmed {
        format!(
            "{} ({})는 재고파일과 매칭되는 재고 행이 없어 현재고를 확정할 수 없습니다. 최근 3개년 출고합계 {:.0}개, 이동 순증감 {:.0}개입니다.",
            part_name, part_no, outbound_qty_sum, movement_net_qty
        )
    } else if let (Some(current), Some(required), Some(shortage_quantity)) =
        (current_stock, required_stock, shortage_quantity)
    {
        format!(
            "{} ({})는 재고파일 기준 현재고 {:.0}개, 필수재고 {:.0}개라서 {:.0}개가 부족합니다. 최근 3개년 출고합계는 {:.0}개입니다.",
            part_name, part_no, current, required, shortage_quantity, outbound_qty_sum
        )
    } else if let (Some(current), Some(required)) = (current_stock, required_stock) {
        format!(
            "{} ({})는 재고파일 기준 현재고 {:.0}개, 필수재고 {:.0}개로 재고가 확인되었습니다. 최근 3개년 출고합계는 {:.0}개입니다.",
            part_name, part_no, current, required, outbound_qty_sum
        )
    } else {
        format!(
            "{} ({})는 재고파일 기준 현재고 {}, 최근 3개년 출고합계 {:.0}개, 이동 순증감 {:.0}개입니다.",
            part_name,
            part_no,
            format_count(current_stock),
            outbound_qty_sum,
            movement_net_qty
        )
    };
    let document_request_hint = format!("{part_name} ({part_no}) 품목으로 구매 품의 문서 작성해줘");

    Some(LegacyShortageItem {
        part_name,
        part_no,
        current_stock,
        required_stock,
        available_stock,
        shortage_gap,
        shortage_quantity,
        projected_stock_balance,
        movement_net_qty,
        inbound_qty_sum,
        outbound_qty_sum,
        outbound_count,
        inventory_confirmed,
        inventory_match_status,
        stock_status,
        unit_price,
        purchase_priority,
        purchase_policy_note: purchase_decision.note,
        summary,
        document_request_hint,
    })
}

fn determine_purchase_priority(
    current_stock: Option<f64>,
    required_stock: Option<f64>,
    inventory_confirmed: bool,
    outbound_qty_sum: f64,
    movement_net_qty: f64,
) -> String {
    if !inventory_confirmed {
        return "확인 필요".into();
    }

    let current_stock = current_stock.unwrap_or(0.0);
    if current_stock <= 0.0 {
        return "긴급".into();
    }

    if let Some(required_stock) = required_stock {
        if current_stock < required_stock {
            if outbound_qty_sum > 0.0 || movement_net_qty < 0.0 {
                return "높음".into();
            }
            return "중간".into();
        }
    }

    if movement_net_qty < 0.0 && outbound_qty_sum > 0.0 {
        "모니터링".into()
    } else {
        "낮음".into()
    }
}

fn inventory_item_matches_query(item: &LegacyShortageItem, needle: Option<&str>) -> bool {
    if let Some(needle) = needle {
        let hay = normalize_lookup_text(&format!("{} {}", item.part_name, item.part_no));
        hay.contains(needle)
    } else {
        true
    }
}

fn inventory_item_matches_status(item: &LegacyShortageItem, status: &str) -> bool {
    match status {
        "" | "all" => true,
        "shortage" => item.stock_status == "재고 부족" || item.stock_status == "재고 없음",
        "sufficient" => item.inventory_confirmed && item.stock_status == "재고 확인",
        "out_of_stock" => item
            .current_stock
            .map(|stock| stock <= 0.0)
            .unwrap_or(false),
        "unverified" => !item.inventory_confirmed || item.stock_status == "재고 미확인",
        "confirmed" => item.inventory_confirmed,
        other => item.stock_status == other || item.inventory_match_status == other,
    }
}

fn sort_inventory_items(items: &mut [LegacyShortageItem], sort: &str) {
    items.sort_by(|a, b| match sort {
        "consumption" | "outbound" => b
            .outbound_qty_sum
            .partial_cmp(&a.outbound_qty_sum)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                a.movement_net_qty
                    .partial_cmp(&b.movement_net_qty)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.part_name.cmp(&b.part_name)),
        "net_decrease" => a
            .movement_net_qty
            .partial_cmp(&b.movement_net_qty)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                b.outbound_qty_sum
                    .partial_cmp(&a.outbound_qty_sum)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.part_name.cmp(&b.part_name)),
        "shortage" => b
            .shortage_quantity
            .unwrap_or(0.0)
            .partial_cmp(&a.shortage_quantity.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.part_name.cmp(&b.part_name)),
        "stock" => a
            .current_stock
            .unwrap_or(f64::MAX)
            .partial_cmp(&b.current_stock.unwrap_or(f64::MAX))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.part_name.cmp(&b.part_name)),
        "name" => a
            .part_name
            .cmp(&b.part_name)
            .then_with(|| a.part_no.cmp(&b.part_no)),
        _ => inventory_priority_rank(a)
            .cmp(&inventory_priority_rank(b))
            .then_with(|| {
                b.shortage_quantity
                    .unwrap_or(0.0)
                    .partial_cmp(&a.shortage_quantity.unwrap_or(0.0))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                b.outbound_qty_sum
                    .partial_cmp(&a.outbound_qty_sum)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.part_name.cmp(&b.part_name)),
    });
}

fn inventory_priority_rank(item: &LegacyShortageItem) -> u8 {
    if item.stock_status == "재고 없음" {
        0
    } else if item.stock_status == "재고 부족" {
        1
    } else if !item.inventory_confirmed {
        2
    } else {
        3
    }
}

fn build_confirmed_shortage_markdown_table(items: &[LegacyShortageItem]) -> String {
    if items.is_empty() {
        return "해당 조건에 맞는 재고 부족 품목이 없습니다.".into();
    }

    let mut lines = vec![
        "| 품목명 | 품번 | 현재고 | 필수재고 | 부족수량 | 상태 |".to_string(),
        "| --- | --- | ---: | ---: | ---: | --- |".to_string(),
    ];

    for item in items {
        lines.push(format!(
            "| {} | {} | {} | {} | {} | {} |",
            escape_markdown_table_cell(&item.part_name),
            escape_markdown_table_cell(&item.part_no),
            format_count(item.current_stock),
            format_count(item.required_stock),
            format_count(item.shortage_quantity),
            escape_markdown_table_cell(&item.stock_status),
        ));
    }

    lines.join("\n")
}

fn build_unverified_shortage_markdown_table(items: &[LegacyShortageItem]) -> Option<String> {
    if items.is_empty() {
        return None;
    }

    let mut lines = vec![
        "| 품목명 | 품번 | 최근 출고합계 | 이동 순증감 | 상태 |".to_string(),
        "| --- | --- | ---: | ---: | --- |".to_string(),
    ];

    for item in items {
        lines.push(format!(
            "| {} | {} | {} | {} | {} |",
            escape_markdown_table_cell(&item.part_name),
            escape_markdown_table_cell(&item.part_no),
            format_count(Some(item.outbound_qty_sum)),
            format_count(Some(item.movement_net_qty)),
            escape_markdown_table_cell(&item.stock_status),
        ));
    }

    Some(lines.join("\n"))
}

fn build_inventory_markdown_table(items: &[LegacyShortageItem]) -> String {
    if items.is_empty() {
        return "해당 조건에 맞는 품목이 없습니다.".into();
    }

    let mut lines = vec![
        "| 품목명 | 품번 | 현재고 | 필수재고 | 최근 출고합계 | 이동 순증감 | 재고확인상태 | 단가 | 구매 우선순위 |".to_string(),
        "| --- | --- | ---: | ---: | ---: | ---: | --- | ---: | --- |".to_string(),
    ];

    for item in items {
        lines.push(format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            escape_markdown_table_cell(&item.part_name),
            escape_markdown_table_cell(&item.part_no),
            format_count(item.current_stock),
            format_count(item.required_stock),
            format_count(Some(item.outbound_qty_sum)),
            format_count(Some(item.movement_net_qty)),
            escape_markdown_table_cell(&item.stock_status),
            format_price_plain(item.unit_price),
            escape_markdown_table_cell(&item.purchase_priority),
        ));
    }

    lines.join("\n")
}

fn build_inventory_report_csv(items: &[LegacyShortageItem]) -> String {
    let mut lines = vec![[
        "품목명",
        "품번",
        "현재고",
        "필수재고",
        "부족수량",
        "재고확인상태",
        "재고매칭상태",
        "단가",
        "구매 우선순위",
        "구매판단",
        "최근 입고합계",
        "최근 출고합계",
        "이동 순증감",
        "출고 건수",
    ]
    .join(",")];

    for item in items {
        lines.push(
            [
                csv_cell(&item.part_name),
                csv_cell(&item.part_no),
                csv_cell(&format_count(item.current_stock)),
                csv_cell(&format_count(item.required_stock)),
                csv_cell(&format_count(item.shortage_quantity)),
                csv_cell(&item.stock_status),
                csv_cell(describe_inventory_match_status(
                    &item.inventory_match_status,
                )),
                csv_cell(&format_price_plain(item.unit_price)),
                csv_cell(&item.purchase_priority),
                csv_cell(&item.purchase_policy_note),
                csv_cell(&format_count(Some(item.inbound_qty_sum))),
                csv_cell(&format_count(Some(item.outbound_qty_sum))),
                csv_cell(&format_count(Some(item.movement_net_qty))),
                csv_cell(&item.outbound_count.to_string()),
            ]
            .join(","),
        );
    }

    format!("\u{feff}{}\n", lines.join("\n"))
}

fn escape_markdown_table_cell(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', " ")
}

fn format_count(value: Option<f64>) -> String {
    match value {
        Some(value) => format!("{value:.0}"),
        None => "-".into(),
    }
}

fn format_price_plain(value: Option<f64>) -> String {
    match value {
        Some(value) => format!("{value:.0}"),
        None => "-".into(),
    }
}

fn csv_cell(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\"").replace('\n', " "))
}

fn resolve_snapshot_part(
    state: &AppState,
    part_name: Option<&str>,
    part_no: Option<&str>,
) -> Result<serde_json::Value, AppError> {
    let snapshot = load_latest_snapshot_json(state)?;
    let parts = snapshot
        .get("parts")
        .and_then(|value| value.as_object())
        .ok_or_else(|| AppError::bad_request("invalid snapshot format: parts missing".into()))?;

    score_snapshot_part_candidates(parts, part_name, part_no, None).ok_or_else(|| {
        AppError::bad_request("requested part was not found in the latest snapshot".into())
    })
}

fn resolve_snapshot_part_for_document(
    state: &AppState,
    fields: &BTreeMap<String, serde_json::Value>,
    part_name: Option<&str>,
    part_no: Option<&str>,
) -> Result<serde_json::Value, AppError> {
    let explicit_part_name = part_name.map(|value| value.to_string()).or_else(|| {
        fields
            .get("품명")
            .and_then(|value| meaningful_lookup_text(Some(value)))
    });
    let explicit_part_no = part_no.map(|value| value.to_string()).or_else(|| {
        fields
            .get("품번")
            .and_then(|value| meaningful_lookup_text(Some(value)))
    });

    resolve_snapshot_part(
        state,
        explicit_part_name.as_deref(),
        explicit_part_no.as_deref(),
    )
}

fn normalize_lookup_text(input: &str) -> String {
    input
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || ('가'..='힣').contains(ch))
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn sanitize_filename_for_output(input: &str) -> String {
    let cleaned = input
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            '\n' | '\r' | '\t' => ' ',
            _ => ch,
        })
        .collect::<String>();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join("_");
    if collapsed.is_empty() {
        "document".to_string()
    } else {
        collapsed
    }
}

fn create_zip_bundle(
    workdir: &Path,
    zip_absolute: &Path,
    generated_files: &[String],
) -> Result<(), String> {
    let parent = zip_absolute
        .parent()
        .ok_or_else(|| format!("invalid zip path: {}", zip_absolute.display()))?;
    fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    let file = fs::File::create(zip_absolute).map_err(|err| err.to_string())?;
    let mut writer = zip::ZipWriter::new(file);

    for relative in generated_files {
        let absolute = workdir.join(relative);
        let bytes = fs::read(&absolute)
            .map_err(|err| format!("read {} failed: {err}", absolute.display()))?;
        writer
            .start_file(
                relative.replace('\\', "/"),
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated)
                    .unix_permissions(0o644),
            )
            .map_err(|err| err.to_string())?;
        use std::io::Write as _;
        writer.write_all(&bytes).map_err(|err| err.to_string())?;
    }

    writer.finish().map_err(|err| err.to_string())?;
    Ok(())
}

fn url_encode_filename(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_' => {
                encoded.push(byte as char)
            }
            b' ' => encoded.push_str("%20"),
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn url_encode_query_value(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn extract_markdown_pdf_download_path(download_path: &str) -> Option<String> {
    let query = download_path.split_once('?').map(|(_, query)| query)?;
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == "path" && !value.is_empty()).then(|| value.to_string())
    })
}

fn media_type_for_download(file_name: &str) -> &'static str {
    match Path::new(file_name)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "pdf" => "application/pdf",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        _ => "application/octet-stream",
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        let body = Json(serde_json::json!({ "error": self.message }));
        (self.status, body).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn create_document_extracts_fields_and_returns_missing_vendor() {
        let app = app_router();
        let request = Request::builder()
            .method("POST")
            .uri("/document/create")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"template_id":"purchase_request","input_text":"품명: __UNIT_TEST_UNMATCHED_PART__\n수량: 3"}"#,
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let payload: CreateResponse = serde_json::from_slice(&body).unwrap();

        assert_eq!(
            payload.updated_fields.get("품명"),
            Some(&serde_json::Value::String(
                "__UNIT_TEST_UNMATCHED_PART__".into()
            ))
        );
        assert_eq!(
            payload.updated_fields.get("수량"),
            Some(&serde_json::Value::Number(3_u64.into()))
        );
        assert_eq!(
            payload.missing_fields,
            vec!["납품업체", "구매사유", "담당자 직접입력", "부품역할"]
        );
        assert_eq!(
            payload.next_question.as_deref(),
            Some("납품업체는 어디로 할까?")
        );
    }

    #[tokio::test]
    async fn fill_document_merges_session_fields_and_completes_template() {
        let app = app_router();
        let create_request = Request::builder()
            .method("POST")
            .uri("/document/create")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"template_id":"purchase_request","input_text":"품명: __UNIT_TEST_UNMATCHED_PART_TWO__\n수량: 3"}"#,
            ))
            .unwrap();

        let create_response = app.clone().oneshot(create_request).await.unwrap();
        let create_body = axum::body::to_bytes(create_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let created: CreateResponse = serde_json::from_slice(&create_body).unwrap();

        let fill_body = serde_json::json!({
            "template_id": "purchase_request",
            "session_id": created.session_id,
            "current_fields": created.updated_fields,
            "user_message": "납품업체는 하이닉스야"
        });

        let fill_request = Request::builder()
            .method("POST")
            .uri("/document/fill")
            .header("content-type", "application/json")
            .body(Body::from(fill_body.to_string()))
            .unwrap();

        let fill_response = app.oneshot(fill_request).await.unwrap();
        assert_eq!(fill_response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(fill_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let payload: FillResponse = serde_json::from_slice(&body).unwrap();

        assert_eq!(
            payload.missing_fields,
            vec!["구매사유", "담당자 직접입력", "부품역할"]
        );
        assert_eq!(
            payload.next_question.as_deref(),
            Some("구매사유는 어떻게 적을까요? 재고 부족, 설비 중단 위험, 사용 이력 중 확인된 근거를 알려주세요.")
        );
        assert_eq!(
            payload.updated_fields.get("납품업체"),
            Some(&serde_json::Value::String("하이닉스야".into()))
        );
    }

    #[test]
    fn merge_snapshot_part_into_fields_overwrites_inventory_fields() {
        let part = serde_json::json!({
            "part_name": "AUTO PART",
            "part_no": "AUTO-001",
            "current_stock_before": 12.0,
            "current_stock_updated": 4.0,
            "required_stock": 8.0,
            "available_stock_qty": 10.0,
            "shortage_gap": 4.0,
            "movement_net_qty": -8.0,
            "inventory_confirmed": true,
            "inventory_match_status": "matched_all",
            "inbound_qty_sum": 3.0,
            "outbound_qty_sum": 11.0,
            "outbound_count": 2,
            "inbound_dates": ["2026-04-01"],
            "outbound_dates": ["2026-04-10", "2026-04-20"],
            "document_context": {
                "received_date": "2026-03-28",
                "used_date_last": "2026-04-21",
                "used_where": "1호기",
                "usage_reason": "베어링 마모",
                "replacement_reason": "예방 교체",
                "required_stock": 8.0,
                "issued_qty": "11",
                "replacement_dates": ["2026-04-01", "", "2026-04-10", "", "", ""],
                "replacement_qtys": ["1", "", "2", "", "", ""],
                "replacement_hosts": ["A라인", "", "B라인", "", "", ""],
                "vendor_name": "지정 협력사",
                "manufacturer_name": "OEM",
                "unit": "EA",
                "unit_price": 125000.0,
                "has_replacement_history": true
            }
        });
        let mut fields = BTreeMap::new();
        fields.insert("품명".into(), serde_json::Value::String("수기값".into()));
        fields.insert("현재고".into(), serde_json::json!(999.0));

        merge_snapshot_part_into_fields(
            &mut fields,
            &part,
            Some("/tmp/output/stock_in_out_monthly.json"),
        );

        assert_eq!(
            fields.get("품명"),
            Some(&serde_json::Value::String("AUTO PART".into()))
        );
        assert_eq!(
            fields.get("품번"),
            Some(&serde_json::Value::String("AUTO-001".into()))
        );
        assert_eq!(fields.get("현재고"), Some(&serde_json::json!(12.0)));
        assert_eq!(fields.get("필수재고량"), Some(&serde_json::json!(8.0)));
        assert_eq!(fields.get("가용재고량"), Some(&serde_json::json!(10.0)));
        assert_eq!(fields.get("과부족"), Some(&serde_json::json!(4.0)));
        assert_eq!(fields.get("이동순증감"), Some(&serde_json::json!(-8.0)));
        assert_eq!(fields.get("추정잔량"), Some(&serde_json::json!(4.0)));
        assert_eq!(
            fields.get("재고확인상태"),
            Some(&serde_json::Value::String("확인".into()))
        );
        assert_eq!(
            fields.get("재고매칭상태"),
            Some(&serde_json::Value::String(
                "재고/입고/출고 모두 매칭".into()
            ))
        );
        assert_eq!(fields.get("총 교체수량"), Some(&serde_json::json!(11)));
        assert_eq!(
            fields.get("교체내역 유무"),
            Some(&serde_json::Value::String("유".into()))
        );
        assert_eq!(
            fields.get("입고일"),
            Some(&serde_json::Value::String("2026-03-28".into()))
        );
        assert_eq!(
            fields.get("사용일"),
            Some(&serde_json::Value::String("2026-04-21".into()))
        );
        assert_eq!(
            fields.get("재고데이터기준"),
            Some(&serde_json::Value::String(
                "stock_in_out_monthly.json".into()
            ))
        );
        assert_eq!(
            fields.get("납품업체"),
            Some(&serde_json::Value::String("지정 협력사".into()))
        );
        assert_eq!(
            fields.get("제조사"),
            Some(&serde_json::Value::String("OEM".into()))
        );
        assert_eq!(fields.get("단가"), Some(&serde_json::json!(125000.0)));
        assert_eq!(
            fields.get("사용처"),
            Some(&serde_json::Value::String("1호기".into()))
        );
        assert_eq!(
            fields.get("문제점"),
            Some(&serde_json::Value::String("베어링 마모".into()))
        );
        assert_eq!(
            fields.get("교체사유"),
            Some(&serde_json::Value::String("예방 교체".into()))
        );
        assert_eq!(
            fields.get("날짜1"),
            Some(&serde_json::Value::String("2026-04-01".into()))
        );
        assert_eq!(
            fields.get("교체수량3"),
            Some(&serde_json::Value::String("2".into()))
        );
        assert_eq!(
            fields.get("호기3"),
            Some(&serde_json::Value::String("B라인".into()))
        );
        assert_eq!(
            fields.get("재고스냅샷경로"),
            Some(&serde_json::Value::String(
                "/tmp/output/stock_in_out_monthly.json".into()
            ))
        );
    }

    #[test]
    fn build_shortage_item_uses_confirmed_stock_values() {
        let part = serde_json::json!({
            "part_name": "TEST PART",
            "part_no": "TP-001",
            "current_stock_before": 2.0,
            "current_stock_updated": -3.0,
            "required_stock": 5.0,
            "available_stock_qty": 2.0,
            "shortage_gap": -3.0,
            "movement_net_qty": -5.0,
            "inventory_confirmed": true,
            "inventory_match_status": "matched_all",
            "inbound_qty_sum": 1.0,
            "outbound_qty_sum": 6.0,
            "outbound_count": 2
        });

        let item = build_shortage_item(&part).expect("expected confirmed shortage item");
        assert_eq!(item.current_stock, Some(2.0));
        assert_eq!(item.required_stock, Some(5.0));
        assert_eq!(item.shortage_gap, Some(-3.0));
        assert_eq!(item.shortage_quantity, Some(3.0));
        assert!(item.inventory_confirmed);
        assert_eq!(item.stock_status, "재고 부족");
        assert!(item.summary.contains("3개가 부족한 상태"));
    }

    #[test]
    fn build_unverified_shortage_item_separates_missing_inventory_match() {
        let part = serde_json::json!({
            "part_name": "OUTBOUND ONLY",
            "part_no": "OB-001",
            "movement_net_qty": -12.0,
            "inventory_confirmed": false,
            "inventory_match_status": "outbound_only",
            "inbound_qty_sum": 0.0,
            "outbound_qty_sum": 12.0,
            "outbound_count": 3
        });

        let item =
            build_unverified_shortage_item(&part).expect("expected unverified shortage item");
        assert_eq!(item.current_stock, None);
        assert!(!item.inventory_confirmed);
        assert_eq!(item.stock_status, "재고 미확인");
        assert_eq!(item.inventory_match_status, "outbound_only");
        assert_eq!(item.shortage_quantity, None);
    }

    #[test]
    fn build_inventory_item_includes_sufficient_stock_and_unit_price() {
        let part = serde_json::json!({
            "part_name": "ENOUGH PART",
            "part_no": "EP-001",
            "current_stock_before": 12.0,
            "required_stock": 5.0,
            "movement_net_qty": -2.0,
            "inventory_confirmed": true,
            "inventory_match_status": "matched_all",
            "outbound_qty_sum": 3.0,
            "outbound_count": 1,
            "document_context": {
                "unit_price": 250000.0
            }
        });

        let item = build_inventory_item(&part).expect("expected inventory item");
        assert_eq!(item.stock_status, "재고 확인");
        assert_eq!(item.unit_price, Some(250000.0));
        assert_eq!(item.purchase_priority, "모니터링");
        assert!(inventory_item_matches_status(&item, "sufficient"));
    }

    #[test]
    fn build_confirmed_shortage_markdown_table_renders_markdown_rows() {
        let table = build_confirmed_shortage_markdown_table(&[LegacyShortageItem {
            part_name: "TEST FILTER".into(),
            part_no: "TF-001".into(),
            current_stock: Some(1.0),
            required_stock: Some(11.0),
            available_stock: Some(1.0),
            shortage_gap: Some(-10.0),
            shortage_quantity: Some(10.0),
            projected_stock_balance: Some(-4.0),
            movement_net_qty: -5.0,
            inbound_qty_sum: 0.0,
            outbound_qty_sum: 5.0,
            outbound_count: 2,
            inventory_confirmed: true,
            inventory_match_status: "matched_all".into(),
            stock_status: "재고 부족".into(),
            unit_price: Some(125000.0),
            purchase_priority: "높음".into(),
            purchase_policy_note: "구매 진행".into(),
            summary: "현재고 1개, 필수재고 11개라서 10개가 부족한 상태입니다.".into(),
            document_request_hint: "TEST FILTER 문서 작성".into(),
        }]);

        assert!(table.contains("| 품목명 | 품번 | 현재고 | 필수재고 | 부족수량 | 상태 |"));
        assert!(table.contains("| TEST FILTER | TF-001 | 1 | 11 | 10 | 재고 부족 |"));
    }
}
