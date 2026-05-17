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
    rank: Option<String>,
    groups: Option<String>,
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
const OPEN_WEBUI_USER_RANK_HEADER: &str = "x-openwebui-user-rank";
const OPEN_WEBUI_USER_GROUPS_HEADER: &str = "x-openwebui-user-groups";
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
            "description": "Single document tool server on port 8001 that processes purchase requests, inventory reports, Markdown-based PDF reports, Word/Excel exports for reports and chat history, and returns download info."
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
                    "summary": "Start interactive document filling session for purchase requests only",
                    "description": "Tool exclusively for purchase requests (template_id must be purchase_request). Do not use for repair reports, work reports, meeting minutes, summaries, or repair_report templates; write Markdown body and call render_markdown_pdf instead. If item name or item code is in input, auto-enrich fields like item name, item code, current stock, and replacement history using the preprocessed stock_in_out_monthly.json snapshot.",
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
                    "summary": "Add field values to purchase request document filling session",
                    "description": "Tool exclusively for purchase request sessions. Do not use for repair reports, work reports, meeting minutes, or summaries. Merge previous session state with current user answer, update fields, and return next field to fill. If user says 'supplier: designated partner', confirm that field. If item name or code is confirmed, re-enrich stock and history fields using preprocessed stock_in_out_monthly.json snapshot.",
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
                    "summary": "Export document using filled fields for purchase requests only",
                    "description": "Generate document content using purchase request fields. Do not use for repair reports, work reports, meeting minutes, or summaries. Use Rust legacy DOCX renderer for docx format. If item name or code is filled, re-enrich stock/history fields using preprocessed stock_in_out_monthly.json snapshot just before rendering.",
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
                    "description": "Use when user requests complete purchase request document writing, batch generation, and full ZIP download. Run legacy Rust batch, bundle all generated DOCX files into ZIP, and return download URL.",
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
                                                "description": "ZIP download URL to provide directly to user"
                                            },
                                            "message": {
                                                "type": "string",
                                                "description": "Brief completion message to show directly to user"
                                            },
                                            "assistant_summary": {
                                                "type": "string",
                                                "description": "Natural language summary that model can relay directly to user"
                                            },
                                            "generated_files_preview": {
                                                "type": "array",
                                                "description": "Sample of generated files",
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
                    "summary": "Query items with current stock shortage or at/below zero",
                    "description": "Use when user asks about items with no current stock, shortage items, or items needing purchase. Answer strictly using preprocessed stock_in_out_monthly.json snapshot; do not cite source Excel file. Use natural language format like 'X current stock, Y required stock, Z shortage quantity' instead of raw field names like shortage_gap. If response has markdown_table field, prioritize it for display.",
                    "parameters": [
                        {
                            "name": "query",
                            "in": "query",
                            "required": false,
                            "schema": { "type": "string" },
                            "description": "Item name or item code filter"
                        },
                        {
                            "name": "limit",
                            "in": "query",
                            "required": false,
                            "schema": { "type": "integer", "default": 20 },
                            "description": "Maximum items to return"
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
                    "summary": "Query full item index with stock/consumption criteria",
                    "description": "Use when user requests full item list, sufficient stock items, stock status filters, item code/name search, stock match status, or parts by consumption rate. Answer strictly using preprocessed stock_in_out_monthly.json snapshot; do not cite source Excel file. filter_options provides conditions for further filtering; if markdown_table exists, use it for display.",
                    "parameters": [
                        {
                            "name": "query",
                            "in": "query",
                            "required": false,
                            "schema": { "type": "string" },
                            "description": "Item name or item code keyword"
                        },
                        {
                            "name": "status",
                            "in": "query",
                            "required": false,
                            "schema": {
                                "type": "string",
                                "enum": ["all", "shortage", "sufficient", "out_of_stock", "unverified", "confirmed"]
                            },
                            "description": "Stock status filter: all=all, shortage=shortage/out of stock, sufficient=adequate stock, out_of_stock=current stock <= 0, unverified=unverified inventory, confirmed=confirmed inventory items"
                        },
                        {
                            "name": "match_status",
                            "in": "query",
                            "required": false,
                            "schema": {
                                "type": "string",
                                "enum": ["matched_all", "stock_inbound", "stock_outbound", "stock_only", "movement_only", "inbound_only", "outbound_only", "unclassified"]
                            },
                            "description": "Stock/inbound/outbound matching status filter"
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
                            "description": "Sort order: consumption=recent total outbound (largest first), net_decrease=net movement (lowest first), shortage=shortage quantity (largest first)"
                        },
                        {
                            "name": "limit",
                            "in": "query",
                            "required": false,
                            "schema": { "type": "integer", "default": 50 },
                            "description": "Maximum items to return"
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
                    "summary": "Generate inventory status report file for all or filtered items",
                    "description": "Use when user requests document or report file with full item list, inventory confirmation status, purchase priority, and unit price. Same query/status/match_status/sort/limit filters apply as list_inventory_items. Generated file includes item name, item code, current stock, required stock, inventory confirmation status, matching status, unit price, purchase priority, purchase decision, total outbound, and net movement.",
                    "parameters": [
                        {
                            "name": "query",
                            "in": "query",
                            "required": false,
                            "schema": { "type": "string" },
                            "description": "Item name or item code keyword"
                        },
                        {
                            "name": "status",
                            "in": "query",
                            "required": false,
                            "schema": {
                                "type": "string",
                                "enum": ["all", "shortage", "sufficient", "out_of_stock", "unverified", "confirmed"]
                            },
                            "description": "Stock status filter"
                        },
                        {
                            "name": "match_status",
                            "in": "query",
                            "required": false,
                            "schema": {
                                "type": "string",
                                "enum": ["matched_all", "stock_inbound", "stock_outbound", "stock_only", "movement_only", "inbound_only", "outbound_only", "unclassified"]
                            },
                            "description": "Stock/inbound/outbound matching status filter"
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
                            "description": "Sort order"
                        },
                        {
                            "name": "limit",
                            "in": "query",
                            "required": false,
                            "schema": { "type": "integer", "default": 500 },
                            "description": "Maximum report rows"
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
                    "summary": "Query document writing context for selected item",
                    "description": "Use when user wants to write purchase request document for specific item. Return context needed for filling, field seeds, and guided field list. Answer strictly using preprocessed stock_in_out_monthly.json snapshot; do not cite source Excel file.",
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
                    "summary": "Generate single-item purchase request document for selected item",
                    "description": "Merge selected item's seed fields with interactively filled fields to generate single DOCX and return download URL. Item name, item code, current stock, and inbound/outbound history use preprocessed stock_in_out_monthly.json snapshot as authoritative source.",
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
                    "summary": "Generate final single-item purchase request document after approval/confirmation",
                    "description": "Use when user expresses approval or intent to proceed. Apply price-based document generation policy, fill default values, and return draft with download URL. Stock and replacement history overwritten using preprocessed stock_in_out_monthly.json snapshot.",
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
                    "summary": "Render Markdown report as PDF file",
                    "description": "Use when user requests repair report, work report, meeting minutes, analysis, or summary as PDF or unspecified format. Pass Markdown body to this tool to generate PDF download link. If user specifies Word/DOCX or Excel/XLSX, call appropriate format rendering tool instead. Put title in title field only, not repeated on first Markdown line. Organize body as: generation info, overview, details, tables/lists, conclusion/actions. Always call this tool for PDF; do not say you cannot generate PDF. Use purchase-specific tool for purchase request DOCX/ZIP; use this for reporting PDFs.",
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
                                            "description": "Report title",
                                            "example": "Elevator 5-unit repair and inspection details report"
                                        },
                                        "markdown": {
                                            "type": "string",
                                            "description": "Markdown report body to render as PDF"
                                        },
                                        "file_name": {
                                            "type": ["string", "null"],
                                            "description": "Optional PDF filename",
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
                                            "description": "Name of document recipient; use current user/requester name if known"
                                        },
                                        "account_name": {
                                            "type": ["string", "null"],
                                            "description": "Account name that requested document"
                                        },
                                        "account_email": {
                                            "type": ["string", "null"],
                                            "description": "Account email that requested document"
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
                    "summary": "Export body or chat history as Word DOCX file",
                    "description": "Use when user requests reports, summaries, work reports, inventory reports, current chat, chat history, or previous answers as Word/DOCX. Put body in transcript or pass messages to generate DOCX download link. Put title in title field only, not repeated on first transcript line. Word output has no Markdown syntax symbols; Markdown tables render as actual Word tables. Do not pass title alone. Do not use for purchase request template DOCX generation.",
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
                                            "description": "Document title"
                                        },
                                        "messages": {
                                            "type": "array",
                                            "description": "Chat messages to export",
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
                                            "description": "Report body for Word document, or full chat transcript to use instead of messages. Do not send empty."
                                        },
                                        "file_name": {
                                            "type": ["string", "null"],
                                            "description": "Optional DOCX filename",
                                            "example": "chat_export.docx"
                                        },
                                        "generated_for": {
                                            "type": ["string", "null"],
                                            "description": "Name of document recipient; use current user/requester name if known"
                                        },
                                        "account_name": {
                                            "type": ["string", "null"],
                                            "description": "Account name that requested document"
                                        },
                                        "account_email": {
                                            "type": ["string", "null"],
                                            "description": "Account email that requested document"
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
                    "summary": "Export body or chat history as Excel XLSX file",
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
                                            "description": "Document title"
                                        },
                                        "messages": {
                                            "type": "array",
                                            "description": "Chat messages to export",
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
                                            "description": "Report body for Excel file, or full chat transcript to use instead of messages. Do not send empty."
                                        },
                                        "file_name": {
                                            "type": ["string", "null"],
                                            "description": "Optional XLSX filename",
                                            "example": "chat_export.xlsx"
                                        },
                                        "generated_for": {
                                            "type": ["string", "null"],
                                            "description": "Name of document recipient; use current user/requester name if known"
                                        },
                                        "account_name": {
                                            "type": ["string", "null"],
                                            "description": "Account name that requested document"
                                        },
                                        "account_email": {
                                            "type": ["string", "null"],
                                            "description": "Account email that requested document"
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
            display_name: "Purchase request",
            required_fields: vec![
                "Item name",
                "Quantity",
                "Supplier",
                "구매사유",
                "Responsible person (manual)",
                "Part role",
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
            .or_else(|_| std::env::var("PORT_PROJECT_PUBLIC_BASE_URL"))
            .unwrap_or_else(|_| "http://192.168.100.202".to_string())
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
        "Generated {} purchase request documents and completed ZIP file {} preparation.",
        run.generated_count, zip_file_name
    );
    let assistant_summary = format!(
        "Purchase request generation complete. {} documents generated; ZIP download link: {}",
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
        source_policy: "Query uses only preprocessed stock_in_out_monthly.json snapshot. Source inbound/stock/outbound Excel files are batch generation input data, not direct query basis.".into(),
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
        source_policy: "Query uses only preprocessed stock_in_out_monthly.json snapshot. Source inbound/stock/outbound Excel files are batch generation input data, not direct query basis.".into(),
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
        "Generated inventory status report file. {} items included; download link: {}",
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
        .unwrap_or("No record")
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
        .unwrap_or_else(|| "Stock unverified".to_string());
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
        "Item name: {part_name}\nItem code: {part_no}\nCurrent stock(per inventory file): {current_stock_text}\nRequired stock: {required_stock_text}\n가용재고: {available_stock_text}\nSurplus/shortage(원본): {shortage_gap_text}\n3개년 Net movement: {movement_net_qty:.0}\n이력 기반 Projected balance: {projected_stock_text}\n입고 합계: {inbound_qty_sum:.0}\n출고 합계: {outbound_qty_sum:.0}\nOutbound count: {outbound_count}\n재고 매칭 상태: {inventory_match_label} ({inventory_match_status})\n상태: {stock_state}"
    );

    let mut fields_seed = BTreeMap::new();
    merge_snapshot_part_into_fields(&mut fields_seed, &part, Some(snapshot_json_path.as_str()));
    fields_seed.insert(
        "Part role".into(),
        serde_json::Value::String("(Manual entry)".into()),
    );
    fields_seed
        .entry("New trading vendor".into())
        .or_insert_with(|| serde_json::Value::String("(Manual entry)".into()));

    let guided_fields = build_guided_fields_for_purchase_request(&fields_seed);

    let assistant_summary = format!(
        "Prepared document writing context for {} ({}). Query basis is the preprocessed stock_in_out_monthly.json snapshot, not direct source Excel reference. Fill values interactively using guided_fields, then generate single-item document.",
        part_name, part_no,
    );

    Ok(Json(LegacyItemContextResponse {
        part_name,
        part_no,
        context,
        data_source: "processed_snapshot_json".into(),
        source_policy: "Query uses only preprocessed stock_in_out_monthly.json snapshot. Source inbound/stock/outbound Excel files are batch generation input data, not direct query basis.".into(),
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

    let item_name = as_string(fields.get("Item name"), "purchase_request");
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
        "Generated single-item purchase request document. Template path: {}; download link: {}",
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
        .unwrap_or("No record")
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
        .entry("New trading vendor".into())
        .or_insert_with(|| serde_json::Value::String("(Manual entry)".into()));
    fields
        .entry("Manufacturer".into())
        .or_insert_with(|| serde_json::Value::String("Per registered manufacturer".into()));
    fields
        .entry("Unit".into())
        .or_insert_with(|| serde_json::Value::String("EA".into()));
    fields
        .entry("Responsible person (manual)".into())
        .or_insert_with(|| serde_json::Value::String("Materials Management Team contact".into()));
    fields.entry("Part role".into()).or_insert_with(|| {
        serde_json::Value::String(format!(
            "{} is a critical part needed to maintain equipment operation, used for maintaining field equipment function and preventive maintenance. Timely replacement and supply reduce equipment failure risk and downtime.",
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
        as_f64(fields.get("Required stock qty")),
        as_f64(fields.get("Current stock")).unwrap_or(0.0),
        as_f64(fields.get("Unit price")),
    );
    let draft_preview = render_preview(
        &TemplateDefinition {
            id: "purchase_request",
            display_name: "Purchase request",
            required_fields: vec!["Item name", "Quantity", "Supplier"],
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
        "Generated final document for {} ({}) using price-based generation policy. Review the draft and download the file from: {}",
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
            "Generated {} PDF file. Download link: {}",
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

        if !fields.contains_key("Item name") && input.contains("SSD") {
            fields.insert("Item name".into(), serde_json::Value::String("SSD".into()));
        }

        if !fields.contains_key("Item name") && input.contains("HDD") {
            fields.insert("Item name".into(), serde_json::Value::String("HDD".into()));
        }

        if !fields.contains_key("Quantity") {
            if let Some(captures) = quantity_re.captures(input) {
                if let Ok(quantity) = captures[1].parse::<u64>() {
                    fields.insert("Quantity".into(), serde_json::Value::Number(quantity.into()));
                }
            }
        }

        if !fields.contains_key("Supplier") {
            if let Some(vendor) = extract_vendor(input) {
                fields.insert("Supplier".into(), serde_json::Value::String(vendor));
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
        "Item name",
        "Quantity",
        "Supplier",
        "구매사유",
        "Responsible person (manual)",
        "Part role",
        "Item code",
        "Current stock",
        "Required stock qty",
        "Available stock qty",
        "Surplus/shortage",
        "Net movement",
        "Projected balance",
        "Stock confirmation status",
        "Stock match status",
        "Unit price",
        "Manufacturer",
        "Unit",
        "Total replacement qty",
        "Replacement history",
        "Inbound date",
        "Last use date",
        "Use location",
        "Issue",
        "Replacement reason",
        "Date 1",
        "Date 2",
        "Date 3",
        "Date 4",
        "Date 5",
        "Date 6",
        "Replacement qty 1",
        "Replacement qty 2",
        "Replacement qty 3",
        "Replacement qty 4",
        "Replacement qty 5",
        "Replacement qty 6",
        "Equipment 1",
        "Equipment 2",
        "Equipment 3",
        "Equipment 4",
        "Equipment 5",
        "Equipment 6",
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
        r"Supplier는\s*([^\s,.]+)",
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
        "Item name" => "What item do you need? Tell me item name or code and stock will auto-populate using snapshot JSON.".into(),
        "Quantity" => "How many units do you need?".into(),
        "Supplier" => "Which supplier?".into(),
        "구매사유" => "How should we state the purchase reason? Provide evidence: inventory shortage, equipment downtime risk, or documented usage history.".into(),
        "Responsible person (manual)" => "Who is the responsible person? If unknown, we can use the Materials Management Team contact.".into(),
        "Part role" => "Describe this part's role. Tell me what function it serves or its purpose in the equipment.".into(),
        _ => format!("{field} 값을 알려주세요."),
    }
}

fn render_preview(
    template: &TemplateDefinition,
    fields: &BTreeMap<String, serde_json::Value>,
) -> String {
    let item = string_or_placeholder(fields.get("Item name"));
    let part_no = string_or_placeholder(fields.get("Item code"));
    let current_stock = string_or_placeholder(fields.get("Current stock"));
    let quantity = string_or_placeholder(fields.get("Quantity"));
    let vendor = string_or_placeholder(fields.get("Supplier"));
    let inventory_status = string_or_placeholder(fields.get("Stock confirmation status"));

    format!(
        "[{}]\n- Item name: {}\n- Item code: {}\n- Current stock(per inventory file): {}\n- Stock confirmation status: {}\n- Quantity: {}\n- Supplier: {}\n{}",
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
        _ => "(Not entered)".into(),
    }
}

fn preview_purchase_note(fields: &BTreeMap<String, serde_json::Value>) -> String {
    if fields
        .get("Stock confirmation status")
        .and_then(|value| value.as_str())
        .map(|value| value == "미확인")
        .unwrap_or(false)
    {
        return "- Purchase decision: Stock unverified\n- 자동사유: 현재 재고파일에서 매칭되는 재고 행이 없어 재고를 확정할 수 없습니다. 최근 입출고 이력을 검토한 뒤 구매 여부를 판단해야 합니다.\n".into();
    }
    let decision = decide_purchase_v2(
        as_f64(fields.get("Required stock qty")),
        as_f64(fields.get("Current stock")).unwrap_or(0.0),
        as_f64(fields.get("Unit price")),
    );
    let reason = build_purchase_reason_text(&build_legacy_row(fields, &decision));
    format!("- Purchase decision: {}\n- 자동사유: {}\n", decision.note, reason)
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
        as_string(fields.get(format!("교체Quantity{}", idx + 1).as_str()), "")
    });
    let replacement_hosts =
        std::array::from_fn(|idx| as_string(fields.get(format!("호기{}", idx + 1).as_str()), ""));
    let item_name = as_string(fields.get("Item name"), "No record");
    let purchase_qty = as_f64(fields.get("Quantity")).unwrap_or(1.0).max(1.0);
    let has_replacement_history = fields
        .get("Replacement history")
        .and_then(|v| v.as_str())
        .map(|v| v == "유")
        .unwrap_or_else(|| replacement_dates.iter().any(|v| !v.is_empty()));

    LegacyDocumentRow {
        part_key: as_string(fields.get("파트키"), &item_name),
        part_no: as_string(fields.get("Item code"), &item_name),
        part_name: item_name,
        received_date: as_string(fields.get("Inbound date"), "No inbound record"),
        used_date_last: as_string(fields.get("Last use date"), "No outbound record"),
        used_where: as_string(fields.get("Use location"), "No record"),
        usage_reason: as_string(fields.get("Issue"), "No record"),
        replacement_reason: as_string(fields.get("Replacement reason"), "No record"),
        current_stock_before: as_f64(fields.get("Current stock")).unwrap_or(0.0),
        required_stock: as_f64(fields.get("Required stock qty")),
        purchase_qty,
        purchase_order_note: decision.note.clone(),
        issued_qty: as_string(fields.get("Total replacement qty"), &format!("{purchase_qty:.0}")),
        replacement_dates,
        replacement_qtys,
        replacement_hosts,
        vendor_name: first_string_field(
            fields,
            &["Previous vendor", "Old vendor", "Old vendor", "Supplier"],
            "No record",
        ),
        manufacturer_name: as_string(fields.get("Manufacturer"), "No record"),
        unit: as_string(fields.get("Unit"), "No record"),
        unit_price: as_string(fields.get("Unit price"), "No record"),
        part_role: as_string(fields.get("Part role"), "(Manual entry)"),
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
        as_f64(fields.get("Required stock qty")),
        as_f64(fields.get("Current stock")).unwrap_or(0.0),
        as_f64(fields.get("Unit price")),
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
                "No record" | "(Manual entry)" | "(직접기입)" | "(Not entered)"
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
            rank: None,
            groups: None,
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

    let rank_raw = header_string(headers, OPEN_WEBUI_USER_RANK_HEADER);
    let groups_raw = header_string(headers, OPEN_WEBUI_USER_GROUPS_HEADER);
    Ok(RegisteredUser {
        id,
        email: email.clone(),
        name: if name.is_empty() { email } else { name },
        rank: if rank_raw.is_empty() { None } else { Some(rank_raw) },
        groups: if groups_raw.is_empty() { None } else { Some(groups_raw) },
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
    let mut b = builder
        .header(INTERNAL_TOKEN_HEADER, configured_internal_token()?)
        .header(OPEN_WEBUI_USER_ID_HEADER, user.id.as_str())
        .header(OPEN_WEBUI_USER_EMAIL_HEADER, user.email.as_str())
        .header(OPEN_WEBUI_USER_NAME_HEADER, user.name.as_str());
    if let Some(rank) = user.rank.as_deref() {
        b = b.header(OPEN_WEBUI_USER_RANK_HEADER, rank);
    }
    if let Some(groups) = user.groups.as_deref() {
        b = b.header(OPEN_WEBUI_USER_GROUPS_HEADER, groups);
    }
    Ok(b)
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
        .unwrap_or("No record")
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
        .unwrap_or("No inbound record")
        .to_string();
    let used_date = part
        .get("outbound_dates")
        .and_then(|value| value.as_array())
        .and_then(|value| value.last())
        .and_then(|value| value.as_str())
        .unwrap_or("No outbound record")
        .to_string();

    fields.insert("Item name".into(), serde_json::Value::String(part_name.clone()));
    fields.insert("Item code".into(), serde_json::Value::String(part_no.clone()));
    set_optional_numeric_field(fields, "Current stock", current_stock);
    set_optional_numeric_field(fields, "Required stock qty", required_stock);
    set_optional_numeric_field(fields, "Available stock qty", available_stock);
    set_optional_numeric_field(fields, "Surplus/shortage", shortage_gap);
    set_optional_numeric_field(fields, "Net movement", Some(movement_net_qty));
    set_optional_numeric_field(fields, "Projected balance", projected_stock_balance);
    fields.insert(
        "Stock confirmation status".into(),
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
        "Stock match status".into(),
        serde_json::Value::String(describe_inventory_match_status(&inventory_match_status).into()),
    );
    fields.insert("Total replacement qty".into(), serde_json::json!(outbound_qty_sum));
    fields.insert(
        "Replacement history".into(),
        serde_json::Value::String(if outbound_count > 0 { "유" } else { "무" }.into()),
    );
    fields.insert("Inbound date".into(), serde_json::Value::String(inbound_date));
    fields.insert("Last use date".into(), serde_json::Value::String(used_date));

    if let Some(document_context) = part
        .get("document_context")
        .and_then(|value| value.as_object())
    {
        if let Some(vendor_name) = snapshot_context_text(document_context.get("vendor_name")) {
            fields
                .entry("Previous vendor".into())
                .or_insert_with(|| serde_json::Value::String(vendor_name.clone()));
            fields
                .entry("Old vendor".into())
                .or_insert_with(|| serde_json::Value::String(vendor_name.clone()));
            fields
                .entry("Supplier".into())
                .or_insert_with(|| serde_json::Value::String(vendor_name));
        }
        if let Some(manufacturer_name) =
            snapshot_context_text(document_context.get("manufacturer_name"))
        {
            fields
                .entry("Manufacturer".into())
                .or_insert_with(|| serde_json::Value::String(manufacturer_name));
        }
        if let Some(unit) = snapshot_context_text(document_context.get("unit")) {
            fields
                .entry("Unit".into())
                .or_insert_with(|| serde_json::Value::String(unit));
        }
        if let Some(unit_price) = document_context
            .get("unit_price")
            .and_then(snapshot_context_number)
        {
            fields
                .entry("Unit price".into())
                .or_insert_with(|| serde_json::json!(unit_price));
        }
        if let Some(received_date) = snapshot_context_text(document_context.get("received_date")) {
            fields.insert("Inbound date".into(), serde_json::Value::String(received_date));
        }
        if let Some(used_date_last) = snapshot_context_text(document_context.get("used_date_last"))
        {
            fields.insert("Last use date".into(), serde_json::Value::String(used_date_last));
        }
        if let Some(used_where) = snapshot_context_text(document_context.get("used_where")) {
            fields.insert("Use location".into(), serde_json::Value::String(used_where));
        }
        if let Some(usage_reason) = snapshot_context_text(document_context.get("usage_reason")) {
            fields.insert("Issue".into(), serde_json::Value::String(usage_reason));
        }
        if let Some(replacement_reason) =
            snapshot_context_text(document_context.get("replacement_reason"))
        {
            fields.insert(
                "Replacement reason".into(),
                serde_json::Value::String(replacement_reason),
            );
        }
        if let Some(issued_qty) = snapshot_context_text(document_context.get("issued_qty")) {
            fields.insert("Total replacement qty".into(), parse_field_value(&issued_qty));
        }
        if let Some(has_replacement_history) = document_context
            .get("has_replacement_history")
            .and_then(|value| value.as_bool())
        {
            fields.insert(
                "Replacement history".into(),
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
            ("replacement_qtys", "교체Quantity"),
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
        .entry("Quantity".into())
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
        .get("Replacement history")
        .and_then(|value| value.as_str())
        .map(|value| value == "유")
        .unwrap_or(false);
    let is_over_500k = as_f64(fields.get("Unit price")).unwrap_or(0.0) >= 500_000.0;

    let specs: &[(&str, &str, &str)] = match (is_over_500k, has_replacement_history) {
        (true, true) => &[
            (
                "구매사유",
                "Strengthen purchase reason",
                "Explain why this part is needed now based on stock and replacement history.",
            ),
            (
                "Responsible person (manual)",
                "Confirm responsible person",
                "Provide contact info for the person who can review or explain this document.",
            ),
            (
                "Supplier",
                "Confirm supplier/vendor",
                "Provide planned supplier or comparable vendor information.",
            ),
            (
                "Part role",
                "Part description",
                "Describe this part's function and field use purpose for document body.",
            ),
        ],
        (true, false) => &[
            (
                "구매사유",
                "Strengthen purchase reason",
                "Explain purchase need based on stock level despite sparse replacement history.",
            ),
            (
                "Responsible person (manual)",
                "Confirm responsible person",
                "Provide document owner or knowledgeable contact.",
            ),
            (
                "Supplier",
                "Confirm supplier/vendor",
                "Provide supplier or quote target vendor information.",
            ),
            (
                "Part role",
                "Part description",
                "Briefly describe part's core function and non-replaceability.",
            ),
        ],
        (false, true) => &[
            (
                "구매사유",
                "구매 사유",
                "State purchase necessity concisely for small purchase document.",
            ),
            (
                "Responsible person (manual)",
                "Confirm responsible person",
                "Provide responsible person's name or department.",
            ),
            (
                "Supplier",
                "Confirm supplier/vendor",
                "State planned trading vendor briefly.",
            ),
            (
                "Part role",
                "Part description",
                "Briefly describe this part's equipment role.",
            ),
        ],
        (false, false) => &[
            (
                "구매사유",
                "구매 사유",
                "State core reason for this part briefly.",
            ),
            (
                "Responsible person (manual)",
                "Confirm responsible person",
                "Provide responsible person's name or department.",
            ),
            (
                "Supplier",
                "Confirm supplier/vendor",
                "Provide purchasing supplier or vendor.",
            ),
            (
                "Part role",
                "Part description",
                "Describe part's purpose in one or two sentences.",
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
    if raw.is_empty() || matches!(raw.as_str(), "No record" | "No outbound record" | "No inbound record")
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
        "matched_all" => "All stock/inbound/outbound matched",
        "stock_inbound" => "Stock/inbound matched",
        "stock_outbound" => "Stock/outbound matched",
        "stock_only" => "Stock only matched",
        "movement_only" => "Inbound/outbound only matched",
        "inbound_only" => "Inbound only matched",
        "outbound_only" => "Outbound only matched",
        _ => "Unclassified",
    }
}

fn describe_inventory_state(
    current_stock: Option<f64>,
    required_stock: Option<f64>,
    inventory_confirmed: bool,
) -> String {
    if !inventory_confirmed {
        return "Stock unverified".into();
    }

    let Some(current_stock) = current_stock else {
        return "Stock unverified".into();
    };

    if current_stock <= 0.0 {
        return "No stock".into();
    }

    if let Some(required_stock) = required_stock {
        if current_stock < required_stock {
            return "Stock shortage".into();
        }
    }

    "Stock confirmed".into()
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
            "{} has no matching stock row in inventory file so current stock cannot be confirmed. Recent 3-year outbound: {} units ({} transactions); net movement: {}. Review stock status and purchase need before proceeding.",
            part_name, outbound_qty_sum, outbound_count, movement_net_qty
        );
    }

    let current_stock = current_stock.unwrap_or(0.0);
    if let Some(required_stock) = required_stock {
        let shortage_gap = current_stock - required_stock;
        format!(
            "{} has current stock {} and required stock {}; shortage {}. Recent 3-year outbound: {} units ({} transactions); net movement: {}. Purchase review needed to prevent equipment downtime from stock shortage.",
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
            "{} current stock: {} units. Recent 3-year outbound: {} ({} transactions); net movement: {}. Review purchase need based on usage history and stock level.",
            part_name, current_stock, outbound_qty_sum, outbound_count, movement_net_qty
        )
    }
}

fn find_snapshot_part_for_document(
    parts: &serde_json::Map<String, serde_json::Value>,
    input: &str,
    fields: &BTreeMap<String, serde_json::Value>,
) -> Option<serde_json::Value> {
    let part_name = meaningful_lookup_text(fields.get("Item name"));
    let part_no = meaningful_lookup_text(fields.get("Item code"));
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
        "미입력" | "직접입력" | "No record" | "No outbound record" | "No inbound record"
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
            "{} ({})는 per inventory file Current stock {:.0}개, Required stock {:.0}개라서 {:.0}개가 부족한 상태입니다. 최근 3개년 Net movement은 {:.0}개입니다.",
            part_name, part_no, current_value, required_stock, shortage_quantity, movement_net_qty
        ),
        (Some(required_stock), None) => format!(
            "{} ({}) has current stock {} and required stock {}; purchase review needed. Recent 3-year net movement: {}",
            part_name, part_no, current_value, required_stock, movement_net_qty
        ),
        (None, _) => format!(
            "{} ({}) current stock {}; recent 3-year outbound {} with net movement {}; purchase review needed.",
            part_name, part_no, current_value, outbound_qty_sum, movement_net_qty
        ),
    };
    let document_request_hint = format!("Create purchase request document for {part_name} ({part_no})");
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
        "{} ({}) has no matching stock row; current stock unconfirmed. Recent 3-year inbound: {} outbound: {} ({} transactions); net movement: {}. Stock status needs re-verification.",
        part_name, part_no, inbound_qty_sum, outbound_qty_sum, outbound_count, movement_net_qty
    );
    let document_request_hint = format!(
        "Verify snapshot and source inventory mapping first for {part_name} ({part_no}) due to unconfirmed stock status"
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
        stock_status: "Stock unverified".into(),
        unit_price: part_unit_price(part),
        purchase_priority: "Verification needed".into(),
        purchase_policy_note: "Stock unverified: determine purchase need after stock row matching".into(),
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
            "{} ({})는 재고파일과 매칭되는 재고 행이 없어 Current stock를 확정할 수 없습니다. 최근 3개년 출고합계 {:.0}개, Net movement {:.0}개입니다.",
            part_name, part_no, outbound_qty_sum, movement_net_qty
        )
    } else if let (Some(current), Some(required), Some(shortage_quantity)) =
        (current_stock, required_stock, shortage_quantity)
    {
        format!(
            "{} ({})는 per inventory file Current stock {:.0}개, Required stock {:.0}개라서 {:.0}개가 부족합니다. 최근 3개년 출고합계는 {:.0}개입니다.",
            part_name, part_no, current, required, shortage_quantity, outbound_qty_sum
        )
    } else if let (Some(current), Some(required)) = (current_stock, required_stock) {
        format!(
            "{} ({}) confirmed with current stock {} and required stock {}. Recent 3-year total outbound: {}",
            part_name, part_no, current, required, outbound_qty_sum
        )
    } else {
        format!(
            "{} ({}) current stock {}, recent 3-year outbound {}, net movement: {}",
            part_name,
            part_no,
            format_count(current_stock),
            outbound_qty_sum,
            movement_net_qty
        )
    };
    let document_request_hint = format!("Create purchase request document for {part_name} ({part_no})");

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
        return "Verification needed".into();
    }

    let current_stock = current_stock.unwrap_or(0.0);
    if current_stock <= 0.0 {
        return "Urgent".into();
    }

    if let Some(required_stock) = required_stock {
        if current_stock < required_stock {
            if outbound_qty_sum > 0.0 || movement_net_qty < 0.0 {
                return "High".into();
            }
            return "Medium".into();
        }
    }

    if movement_net_qty < 0.0 && outbound_qty_sum > 0.0 {
        "Monitoring".into()
    } else {
        "Low".into()
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
        "shortage" => item.stock_status == "Stock shortage" || item.stock_status == "No stock",
        "sufficient" => item.inventory_confirmed && item.stock_status == "Stock confirmed",
        "out_of_stock" => item
            .current_stock
            .map(|stock| stock <= 0.0)
            .unwrap_or(false),
        "unverified" => !item.inventory_confirmed || item.stock_status == "Stock unverified",
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
    if item.stock_status == "No stock" {
        0
    } else if item.stock_status == "Stock shortage" {
        1
    } else if !item.inventory_confirmed {
        2
    } else {
        3
    }
}


fn stock_status_korean(status: &str) -> &str {
    match status {
        "Stock shortage" => "재고 부족",
        "No stock" => "재고 없음",
        "Stock unverified" => "재고 미확인",
        "Stock confirmed" => "재고 확인됨",
        _ => status,
    }
}

fn purchase_priority_korean(priority: &str) -> &str {
    match priority {
        "Urgent" => "긴급",
        "High" => "상",
        "Medium" => "중",
        "Monitoring" => "관찰",
        "Verification needed" => "확인 필요",
        _ => priority,
    }
}

fn build_confirmed_shortage_markdown_table(items: &[LegacyShortageItem]) -> String {
    if items.is_empty() {
        return "조건에 맞는 부족 재고 품목이 없습니다.".into();
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
            escape_markdown_table_cell(stock_status_korean(&item.stock_status)),
        ));
    }

    lines.join("\n")
}

fn build_unverified_shortage_markdown_table(items: &[LegacyShortageItem]) -> Option<String> {
    if items.is_empty() {
        return None;
    }

    let mut lines = vec![
        "| 품목명 | 품번 | 최근 출고합계 | 이동 순량 | 상태 |".to_string(),
        "| --- | --- | ---: | ---: | --- |".to_string(),
    ];

    for item in items {
        lines.push(format!(
            "| {} | {} | {} | {} | {} |",
            escape_markdown_table_cell(&item.part_name),
            escape_markdown_table_cell(&item.part_no),
            format_count(Some(item.outbound_qty_sum)),
            format_count(Some(item.movement_net_qty)),
            escape_markdown_table_cell(stock_status_korean(&item.stock_status)),
        ));
    }

    Some(lines.join("\n"))
}

fn build_inventory_markdown_table(items: &[LegacyShortageItem]) -> String {
    if items.is_empty() {
        return "조건에 맞는 품목이 없습니다.".into();
    }

    let mut lines = vec![
        "| 품목명 | 품번 | 현재고 | 필수재고 | 최근 출고합계 | 이동 순량 | 재고 상태 | 단가 | 구매 우선도 |".to_string(),
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
            escape_markdown_table_cell(stock_status_korean(&item.stock_status)),
            format_price_plain(item.unit_price),
            escape_markdown_table_cell(purchase_priority_korean(&item.purchase_priority)),
        ));
    }

    lines.join("\n")
}

fn build_inventory_report_csv(items: &[LegacyShortageItem]) -> String {
    let mut lines = vec![[
        "Item name",
        "Item code",
        "Current stock",
        "Required stock",
        "Shortage qty",
        "Stock confirmation status",
        "Stock match status",
        "Unit price",
        "Purchase priority",
        "Purchase decision",
        "Recent inbound total",
        "Recent outbound total",
        "Net movement",
        "Outbound count",
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
            .get("Item name")
            .and_then(|value| meaningful_lookup_text(Some(value)))
    });
    let explicit_part_no = part_no.map(|value| value.to_string()).or_else(|| {
        fields
            .get("Item code")
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
                r#"{"template_id":"purchase_request","input_text":"Item name: __UNIT_TEST_UNMATCHED_PART__\nQuantity: 3"}"#,
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let payload: CreateResponse = serde_json::from_slice(&body).unwrap();

        assert_eq!(
            payload.updated_fields.get("Item name"),
            Some(&serde_json::Value::String(
                "__UNIT_TEST_UNMATCHED_PART__".into()
            ))
        );
        assert_eq!(
            payload.updated_fields.get("Quantity"),
            Some(&serde_json::Value::Number(3_u64.into()))
        );
        assert_eq!(
            payload.missing_fields,
            vec!["Supplier", "구매사유", "Responsible person (manual)", "Part role"]
        );
        assert_eq!(
            payload.next_question.as_deref(),
            Some("Which supplier?")
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
                r#"{"template_id":"purchase_request","input_text":"Item name: __UNIT_TEST_UNMATCHED_PART_TWO__\nQuantity: 3"}"#,
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
            "user_message": "Supplier는 하이닉스야"
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
            vec!["구매사유", "Responsible person (manual)", "Part role"]
        );
        assert_eq!(
            payload.next_question.as_deref(),
            Some("How should we state the purchase reason? Provide evidence: inventory shortage, equipment downtime risk, or documented usage history.")
        );
        assert_eq!(
            payload.updated_fields.get("Supplier"),
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
        fields.insert("Item name".into(), serde_json::Value::String("수기값".into()));
        fields.insert("Current stock".into(), serde_json::json!(999.0));

        merge_snapshot_part_into_fields(
            &mut fields,
            &part,
            Some("/tmp/output/stock_in_out_monthly.json"),
        );

        assert_eq!(
            fields.get("Item name"),
            Some(&serde_json::Value::String("AUTO PART".into()))
        );
        assert_eq!(
            fields.get("Item code"),
            Some(&serde_json::Value::String("AUTO-001".into()))
        );
        assert_eq!(fields.get("Current stock"), Some(&serde_json::json!(12.0)));
        assert_eq!(fields.get("Required stock qty"), Some(&serde_json::json!(8.0)));
        assert_eq!(fields.get("Available stock qty"), Some(&serde_json::json!(10.0)));
        assert_eq!(fields.get("Surplus/shortage"), Some(&serde_json::json!(4.0)));
        assert_eq!(fields.get("Net movement"), Some(&serde_json::json!(-8.0)));
        assert_eq!(fields.get("Projected balance"), Some(&serde_json::json!(4.0)));
        assert_eq!(
            fields.get("Stock confirmation status"),
            Some(&serde_json::Value::String("확인".into()))
        );
        assert_eq!(
            fields.get("Stock match status"),
            Some(&serde_json::Value::String(
                "All stock/inbound/outbound matched".into()
            ))
        );
        assert_eq!(fields.get("Total replacement qty"), Some(&serde_json::json!(11)));
        assert_eq!(
            fields.get("Replacement history"),
            Some(&serde_json::Value::String("유".into()))
        );
        assert_eq!(
            fields.get("Inbound date"),
            Some(&serde_json::Value::String("2026-03-28".into()))
        );
        assert_eq!(
            fields.get("Last use date"),
            Some(&serde_json::Value::String("2026-04-21".into()))
        );
        assert_eq!(
            fields.get("재고데이터기준"),
            Some(&serde_json::Value::String(
                "stock_in_out_monthly.json".into()
            ))
        );
        assert_eq!(
            fields.get("Supplier"),
            Some(&serde_json::Value::String("지정 협력사".into()))
        );
        assert_eq!(
            fields.get("Manufacturer"),
            Some(&serde_json::Value::String("OEM".into()))
        );
        assert_eq!(fields.get("Unit price"), Some(&serde_json::json!(125000.0)));
        assert_eq!(
            fields.get("Use location"),
            Some(&serde_json::Value::String("1호기".into()))
        );
        assert_eq!(
            fields.get("Issue"),
            Some(&serde_json::Value::String("베어링 마모".into()))
        );
        assert_eq!(
            fields.get("Replacement reason"),
            Some(&serde_json::Value::String("예방 교체".into()))
        );
        assert_eq!(
            fields.get("Date 1"),
            Some(&serde_json::Value::String("2026-04-01".into()))
        );
        assert_eq!(
            fields.get("교체Quantity3"),
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
        assert_eq!(item.stock_status, "Stock shortage");
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
        assert_eq!(item.stock_status, "Stock unverified");
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
        assert_eq!(item.stock_status, "Stock confirmed");
        assert_eq!(item.unit_price, Some(250000.0));
        assert_eq!(item.purchase_priority, "Monitoring");
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
            stock_status: "Stock shortage".into(),
            unit_price: Some(125000.0),
            purchase_priority: "High".into(),
            purchase_policy_note: "구매 진행".into(),
            summary: "Current stock 1개, Required stock 11개라서 10개가 부족한 상태입니다.".into(),
            document_request_hint: "TEST FILTER 문서 작성".into(),
        }]);

        assert!(table.contains("| 품목명 | 품번 | 현재고 | 필수재고 | 부족수량 | 상태 |"));
        assert!(table.contains("| TEST FILTER | TF-001 | 1 | 11 | 10 | 재고 부족 |"));

    }
}
