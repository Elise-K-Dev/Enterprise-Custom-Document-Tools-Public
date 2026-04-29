from __future__ import annotations

import json
import os
import re
import hashlib
import secrets
import sys
import threading
import time
import urllib.error
import urllib.request
import html
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, List, Literal
from urllib.parse import quote

import requests
from fastapi import FastAPI, HTTPException, Query, Request
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import FileResponse
from markdown_it import MarkdownIt
from playwright.async_api import async_playwright
from pydantic import BaseModel, Field
from rank_bm25 import BM25Okapi
from kiwipiepy import Kiwi


app = FastAPI(title="document-parser-service", version="0.2.0", openapi_url=None)
app.add_middleware(
    CORSMiddleware,
    allow_origins=["*"],
    allow_credentials=False,
    allow_methods=["*"],
    allow_headers=["*"],
)


LEGACY_RAG_PROMPT_TEMPLATE = """다음 제공된 [Context] 문서들만 참고해서 [Query]에 대한 답변 작성.
Context에 없는 내용은 지어내지 말고, "해당 내용은 문서에서 확인할 수 없습니다"라고 할 것.
이전 대화가 있으면 맥락을 이어서 답변할 것. 사용자가 "다른건?", "더 없어?" 등 후속 질문을 하면 이전 대화 맥락을 참고할 것.
설명은 간결하고 핵심만.

[Context]
{context}
{history_block}
[Query]
{query}"""

LEGACY_PART_FUNCTION_PROMPT_TEMPLATE = """다음 산업 설비용 부품의 역할과 사용 목적을 구매 품의 문서에 바로 넣을 수 있게 작성해줘.
조건:
- 한국어 2~3문장
- 핵심 기능, 작동 원리 또는 역할, 실제 사용 목적을 포함
- 추측이라는 표현이나 주의문 없이 바로 본문으로 쓸 수 있게 작성
- 마크다운, 제목, 번호 없이 설명 문장만 출력
부품명: {part_name}
부품번호: {part_number}"""

LEGACY_ROUTER_PROMPT_TEMPLATE = """
질문 분석 후 JSON 응답.

[조건]
- years: 연도 (list of int). 현재 2026년 기준. (예: 최근 2년 -> [2025, 2026])
- months: 월 (list of int). 범위 질의는 모든 월을 나열 (예: 3월~6월 -> [3, 4, 5, 6]). 조건 없으면 []
- search_query: 본문 검색용 질의어 (string). 연도/월을 제외한 핵심 키워드만 추출

[질문]
"{query}"

[출력]
{{"years": [2025, 2026], "months": [], "search_query": "엘리베이터 수리 내역"}}
""".strip()

LEGACY_MD_INDEXER_DIR = Path(
    os.getenv(
        "LEGACY_MD_INDEXER_DIR",
        "/app/legacy-md-indexer-main",
    )
)
LEGACY_USERS_FILE = LEGACY_MD_INDEXER_DIR / "users.json"
LEGACY_CATALOG_FILE = LEGACY_MD_INDEXER_DIR / "file_catalog.json"
LEGACY_PROCESSED_DIR = LEGACY_MD_INDEXER_DIR / "processed_md"
if str(LEGACY_MD_INDEXER_DIR) not in sys.path:
    sys.path.insert(0, str(LEGACY_MD_INDEXER_DIR))
_KIWI: Kiwi | None = None
_LEGACY_RAG_GENERATOR: Any | None = None
_LEGACY_RAG_GENERATOR_LOCK = threading.RLock()
OPEN_WEBUI_EMAIL_RANK_MAP = {
    "analyst@example.com": "hi_rank",
    "admin@gmail.com": "hi_rank",
    "user@gmail.com": "low_rank",
}
OPEN_WEBUI_USER_EMAIL_HEADER = "x-openwebui-user-email"
OPEN_WEBUI_USER_ID_HEADER = "x-openwebui-user-id"
OPEN_WEBUI_USER_NAME_HEADER = "x-openwebui-user-name"
OPEN_WEBUI_USER_GROUPS_HEADER = "x-openwebui-user-groups"
OPEN_WEBUI_USER_RANK_HEADER = "x-openwebui-user-rank"
INTERNAL_TOKEN_HEADER = "x-port-project-internal-token"
PDF_OUTPUT_DIR = Path(os.getenv("PARSER_PDF_OUTPUT_DIR", "/app/output/pdf"))
PDF_PUBLIC_BASE_URL = os.getenv("PARSER_PDF_PUBLIC_BASE_URL", "http://127.0.0.1:8002").rstrip("/")
CHROMIUM_EXECUTABLE_PATH = os.getenv("CHROMIUM_EXECUTABLE_PATH", "/usr/bin/chromium")
DOWNLOAD_TOKEN_TTL_SECONDS = int(os.getenv("PORT_PROJECT_DOWNLOAD_TOKEN_TTL_SECONDS", "3600"))
_DOWNLOAD_GRANTS: dict[str, dict[str, Any]] = {}

REPORT_CSS = """
@page { size: A4; margin: 18mm 16mm; }
* { box-sizing: border-box; }
body {
  margin: 0;
  color: #1f2933;
  font-family: "Noto Sans CJK KR", "Noto Sans KR", "Noto Sans", Arial, sans-serif;
  font-size: 11pt;
  line-height: 1.58;
}
h1 {
  margin: 0 0 12px;
  padding-bottom: 8px;
  border-bottom: 2px solid #1f2933;
  color: #111827;
  font-size: 22pt;
  line-height: 1.25;
}
h2 { margin: 22px 0 8px; color: #111827; font-size: 15pt; }
h3 { margin: 16px 0 6px; color: #25313f; font-size: 12.5pt; }
p { margin: 6px 0; }
ul, ol { margin: 6px 0 10px 22px; padding: 0; }
li { margin: 3px 0; }
table {
  width: 100%;
  margin: 10px 0 14px;
  border-collapse: collapse;
  table-layout: fixed;
  font-size: 9.5pt;
}
th, td {
  padding: 6px 7px;
  border: 1px solid #cbd5e1;
  vertical-align: top;
  word-break: keep-all;
  overflow-wrap: anywhere;
}
th { background: #eef2f7; color: #111827; font-weight: 700; }
blockquote {
  margin: 10px 0;
  padding: 8px 12px;
  border-left: 4px solid #94a3b8;
  background: #f8fafc;
}
pre {
  padding: 10px;
  border: 1px solid #d7dee8;
  background: #f8fafc;
  white-space: pre-wrap;
}
.meta { margin: 0 0 16px; color: #52606d; font-size: 9.5pt; }
"""


class ParseRequest(BaseModel):
    content: str = Field(..., description="Raw text or extracted document content")


class ParseResponse(BaseModel):
    markdown: str
    sections: List[str]


class ChatTurn(BaseModel):
    role: str = Field(..., description="user 또는 assistant")
    content: str = Field(..., description="이전 대화 텍스트")


class GuidedField(BaseModel):
    field: str = Field(..., description="채울 필드명")
    label: str | None = Field(default=None, description="표시용 라벨")
    prompt: str = Field(..., description="해당 필드를 채우기 위한 한국어 지시문")


class FillFieldsRequest(BaseModel):
    query: str = Field(..., description="사용자 요청 또는 문서 작성 요청문")
    context: str = Field(..., description="증거 문서, 재고 정보, 업무 메모 등 문서 채우기 근거")
    chat_history: list[ChatTurn] = Field(default_factory=list, description="이전 대화 내역")
    guided_fields: list[GuidedField] = Field(
        default_factory=list,
        description="문서 템플릿에서 직접입력이 필요한 필드와 한국어 지시문",
    )
    part_name: str | None = Field(default=None, description="부품명")
    part_number: str | None = Field(default=None, description="부품번호")


class FillFieldsResponse(BaseModel):
    status: str
    tool: str
    message: str
    suggested_fields: dict[str, str]
    assistant_summary: str


class SearchDocumentRequest(BaseModel):
    user_email: str | None = Field(default=None, description="Open WebUI 로그인 이메일")
    username: str | None = Field(default=None, description="레거시 검색 계정")
    password: str | None = Field(default=None, description="레거시 검색 비밀번호")
    query: str = Field(..., description="검색할 자연어 질문")
    chat_history: list[ChatTurn] = Field(default_factory=list, description="이전 대화 내역")


class SearchReference(BaseModel):
    file_path: str
    score: float


class SearchDocumentResponse(BaseModel):
    status: str
    username: str
    rank: str
    auth_source: str
    query: str
    parameters: dict[str, Any]
    filtered_files_count: int
    references: list[SearchReference]
    answer: str
    assistant_summary: str


class ResolvedSearchIdentity(BaseModel):
    username: str
    rank: str
    auth_source: str


class RenderMarkdownPdfRequest(BaseModel):
    title: str = Field(..., description="보고서 제목", min_length=1, max_length=200)
    markdown: str = Field(..., description="PDF로 변환할 Markdown 본문", min_length=1)
    file_name: str | None = Field(default=None, description="생성할 PDF 파일명")
    page_size: Literal["A4", "Letter"] = Field(default="A4", description="PDF 용지 크기")
    orientation: Literal["portrait", "landscape"] = Field(default="portrait", description="PDF 방향")


class RenderMarkdownPdfResponse(BaseModel):
    output_path: str
    download_path: str
    download_url: str
    file_name: str
    title: str
    assistant_summary: str


@app.get("/openapi.json")
def openapi_spec() -> dict[str, Any]:
    return {
        "openapi": "3.0.0",
        "info": {
            "title": "Document Parser And Filler",
            "version": "1.1.0",
            "description": "RAW 문서 파싱과 구매 품의 문서 채우기 보조를 수행합니다.",
        },
        "servers": [
            {"url": "http://parser-service:8002"},
        ],
        "paths": {
            "/health": {
                "get": {
                    "operationId": "parser_health_check",
                    "summary": "Health check",
                    "responses": {
                        "200": {
                            "description": "Healthy",
                            "content": {
                                "application/json": {
                                    "schema": {
                                        "type": "object",
                                        "properties": {
                                            "status": {"type": "string", "example": "ok"}
                                        },
                                    }
                                }
                            },
                        }
                    },
                }
            },
            "/parser/to-md": {
                "post": {
                    "operationId": "parse_to_md",
                    "summary": "Convert raw content to markdown",
                    "requestBody": {
                        "required": True,
                        "content": {
                            "application/json": {
                                "schema": {
                                    "type": "object",
                                    "required": ["content"],
                                    "properties": {
                                        "content": {
                                            "type": "string",
                                            "example": "제목: 테스트\n\n본문: 확인",
                                        }
                                    },
                                }
                            }
                        },
                    },
                    "responses": {
                        "200": {
                            "description": "Markdown parsed",
                            "content": {
                                "application/json": {
                                    "schema": {
                                        "type": "object",
                                        "properties": {
                                            "markdown": {"type": "string"},
                                            "sections": {
                                                "type": "array",
                                                "items": {"type": "string"},
                                            },
                                        },
                                    }
                                }
                            },
                        }
                    },
                }
            },
            "/document/fill-fields": {
                "post": {
                    "operationId": "fill_document_fields_ko",
                    "summary": "Fill Korean document fields using the legacy Python prompt",
                    "description": "파이썬 레거시 프롬프트를 그대로 사용하고, 문서 채우기 지시를 한국어로 덧붙여 구매 품의 문서 필드 초안을 생성합니다.",
                    "requestBody": {
                        "required": True,
                        "content": {
                            "application/json": {
                                "schema": {
                                    "type": "object",
                                    "required": ["query", "context"],
                                    "properties": {
                                        "query": {
                                            "type": "string",
                                            "example": "구매 품의 문서 작성을 위해 구매사유와 담당자 정보를 채워줘",
                                        },
                                        "context": {
                                            "type": "string",
                                            "example": "재고 부족: SCR DRIVER FUSE 0EA / 필요수량 2EA / 최근 사용 증가",
                                        },
                                        "chat_history": {
                                            "type": "array",
                                            "items": {
                                                "type": "object",
                                                "properties": {
                                                    "role": {"type": "string"},
                                                    "content": {"type": "string"},
                                                },
                                            },
                                        },
                                        "guided_fields": {
                                            "type": "array",
                                            "items": {
                                                "type": "object",
                                                "properties": {
                                                    "field": {"type": "string"},
                                                    "label": {"type": "string"},
                                                    "prompt": {"type": "string"},
                                                },
                                                "required": ["field", "prompt"],
                                            },
                                        },
                                        "part_name": {"type": "string"},
                                        "part_number": {"type": "string"},
                                    },
                                }
                            }
                        },
                    },
                    "responses": {
                        "200": {
                            "description": "Fields suggested",
                            "content": {
                                "application/json": {
                                    "schema": {
                                        "type": "object",
                                        "properties": {
                                            "status": {"type": "string"},
                                            "tool": {"type": "string"},
                                            "message": {"type": "string"},
                                            "suggested_fields": {
                                                "type": "object",
                                                "additionalProperties": {"type": "string"},
                                            },
                                            "assistant_summary": {"type": "string"},
                                        },
                                    }
                                }
                            },
                        }
                    },
                }
            },
            "/search/query": {
                "post": {
                    "operationId": "search_documents_by_rank",
                    "summary": "레거시 문서 검색",
                    "description": "사내 운영 문서, 비용 문서, 인원 문서, 물동량 문서처럼 내부 문서를 찾아달라는 요청에는 이 도구를 우선 사용합니다. Open WebUI 로그인 사용자 정보가 헤더로 전달되면 그 정보로 hi_rank / low_rank 권한을 자동 해석하고, 그렇지 않으면 legacy username/password 인증을 사용합니다. 후속 질문이면 chat_history를 함께 보내 이전 대화 맥락을 유지합니다.",
                    "requestBody": {
                        "required": True,
                        "content": {
                            "application/json": {
                                "schema": {
                                    "type": "object",
                                    "required": ["query"],
                                    "properties": {
                                        "user_email": {
                                            "type": "string",
                                            "description": "Open WebUI 로그인 이메일. 비워도 X-OpenWebUI-User-Email 헤더가 있으면 그 값을 사용합니다.",
                                        },
                                        "username": {
                                            "type": "string",
                                            "description": "레거시 검색 계정",
                                        },
                                        "password": {
                                            "type": "string",
                                            "description": "레거시 검색 비밀번호",
                                        },
                                        "query": {
                                            "type": "string",
                                            "description": "찾고 싶은 사내 문서 또는 문서 내용 질문",
                                        },
                                        "chat_history": {
                                            "type": "array",
                                            "items": {
                                                "type": "object",
                                                "properties": {
                                                    "role": {"type": "string"},
                                                    "content": {"type": "string"},
                                                },
                                            },
                                        },
                                    },
                                }
                            }
                        },
                    },
                    "responses": {
                        "200": {
                            "description": "Search answer generated"
                        }
                    },
                }
            },
            "/render/markdown-pdf": {
                "post": {
                    "operationId": "render_markdown_pdf",
                    "summary": "Markdown 보고서를 PDF 파일로 생성",
                    "description": "사용자가 수리 완료 보고서, 업무 보고, 회의록, 분석 결과, 요약문을 문서 파일로 요청하면 Markdown 본문을 이 도구에 전달해 PDF 다운로드 링크를 생성합니다. 필요한 근거는 search_documents_by_rank로 먼저 찾고, 이 도구는 최종 Markdown을 PDF로 렌더링합니다.",
                    "requestBody": {
                        "required": True,
                        "content": {
                            "application/json": {
                                "schema": {
                                    "type": "object",
                                    "required": ["title", "markdown"],
                                    "properties": {
                                        "title": {"type": "string", "example": "엘리베이터 주요 정비 및 개선 조치 결과 보고"},
                                        "markdown": {"type": "string", "description": "PDF로 변환할 Markdown 보고서 본문"},
                                        "file_name": {"type": ["string", "null"], "example": "elevator_repair_report.pdf"},
                                        "page_size": {"type": "string", "default": "A4", "enum": ["A4", "Letter"]},
                                        "orientation": {"type": "string", "default": "portrait", "enum": ["portrait", "landscape"]},
                                    },
                                }
                            }
                        },
                    },
                    "responses": {"200": {"description": "PDF rendered"}},
                }
            },
        },
    }


def configured_internal_token() -> str:
    token = os.getenv("PORT_PROJECT_INTERNAL_TOKEN", "").strip()
    if not token:
        raise HTTPException(status_code=500, detail="PORT_PROJECT_INTERNAL_TOKEN is not configured")
    return token


def require_internal_request(raw_request: Request) -> None:
    expected = configured_internal_token()
    supplied = (raw_request.headers.get(INTERNAL_TOKEN_HEADER) or "").strip()
    if not secrets.compare_digest(supplied, expected):
        raise HTTPException(status_code=403, detail="invalid internal tool token")


def require_registered_tool_user(raw_request: Request) -> dict[str, str]:
    require_internal_request(raw_request)
    email = (raw_request.headers.get(OPEN_WEBUI_USER_EMAIL_HEADER) or "").strip().lower()
    user_id = (raw_request.headers.get(OPEN_WEBUI_USER_ID_HEADER) or "").strip()
    name = (raw_request.headers.get(OPEN_WEBUI_USER_NAME_HEADER) or "").strip()
    if not email or not user_id:
        raise HTTPException(status_code=401, detail="registered Open WebUI account is required")
    return {"email": email, "user_id": user_id, "name": name or email}


def purge_expired_download_grants(now: float | None = None) -> None:
    current = now or time.time()
    expired = [
        token
        for token, grant in _DOWNLOAD_GRANTS.items()
        if float(grant.get("expires_at", 0)) <= current
    ]
    for token in expired:
        _DOWNLOAD_GRANTS.pop(token, None)


def issue_download_token(path: str, user: dict[str, str]) -> str:
    purge_expired_download_grants()
    token = secrets.token_urlsafe(32)
    _DOWNLOAD_GRANTS[token] = {
        "path": path,
        "user_id": user["user_id"],
        "email": user["email"],
        "expires_at": time.time() + DOWNLOAD_TOKEN_TTL_SECONDS,
    }
    return token


def validate_download_token(path: str, token: str | None) -> None:
    if not token:
        raise HTTPException(status_code=401, detail="download token is required")
    purge_expired_download_grants()
    grant = _DOWNLOAD_GRANTS.get(token)
    if not grant or grant.get("path") != path:
        raise HTTPException(status_code=403, detail="invalid download token")


def format_suggested_fields_message(suggested_fields: dict[str, str]) -> str:
    if not suggested_fields:
        return "문서 필드 초안을 생성하지 못했습니다. 입력 조건이나 근거 문서를 다시 확인해 주세요."
    lines = ["문서 필드 초안을 생성했습니다."]
    for field, value in suggested_fields.items():
        lines.append(f"- {field}: {value}")
    return "\n".join(lines)


@app.get("/health")
def health() -> dict[str, str]:
    return {"status": "ok"}


@app.post("/parser/to-md", response_model=ParseResponse)
def parse_to_md(request: ParseRequest, raw_request: Request) -> ParseResponse:
    require_registered_tool_user(raw_request)
    normalized = normalize_text(request.content)
    sections = split_sections(normalized)
    markdown = to_markdown(sections)
    return ParseResponse(markdown=markdown, sections=sections)


@app.post("/render/markdown-pdf", response_model=RenderMarkdownPdfResponse)
async def render_markdown_pdf(request: RenderMarkdownPdfRequest, raw_request: Request) -> RenderMarkdownPdfResponse:
    user = require_registered_tool_user(raw_request)
    PDF_OUTPUT_DIR.mkdir(parents=True, exist_ok=True)
    file_name = sanitize_pdf_filename(request.file_name or request.title)
    relative_path = Path("reports") / file_name
    output_path = PDF_OUTPUT_DIR / relative_path
    output_path.parent.mkdir(parents=True, exist_ok=True)

    html_content = build_report_html(request.title, request.markdown)
    await render_pdf_with_chromium(
        html_content,
        output_path,
        page_size=request.page_size,
        landscape=request.orientation == "landscape",
    )

    token = issue_download_token(relative_path.as_posix(), user)
    download_path = f"/render/download?path={quote(relative_path.as_posix())}&token={quote(token)}"
    download_url = f"{PDF_PUBLIC_BASE_URL}{download_path}"
    return RenderMarkdownPdfResponse(
        output_path=relative_path.as_posix(),
        download_path=download_path,
        download_url=download_url,
        file_name=file_name,
        title=request.title,
        assistant_summary=f"{request.title} PDF 파일을 생성했습니다. 다운로드 링크는 {download_url} 입니다.",
    )


@app.get("/render/download")
def download_rendered_pdf(
    path: str = Query(..., description="PDF_OUTPUT_DIR 기준 상대 경로"),
    token: str | None = Query(default=None, description="등록 계정에 발급된 다운로드 토큰"),
) -> FileResponse:
    validate_download_token(path, token)
    requested = PDF_OUTPUT_DIR / path
    try:
        canonical_output = PDF_OUTPUT_DIR.resolve()
        canonical_file = requested.resolve()
    except FileNotFoundError as exc:
        raise HTTPException(status_code=404, detail="file not found") from exc

    if not canonical_file.is_file() or not canonical_file.is_relative_to(canonical_output):
        raise HTTPException(status_code=404, detail="file not found")

    return FileResponse(
        canonical_file,
        media_type="application/pdf",
        filename=canonical_file.name,
    )


@app.post("/document/fill-fields", response_model=FillFieldsResponse)
def fill_document_fields(request: FillFieldsRequest, raw_request: Request) -> FillFieldsResponse:
    require_registered_tool_user(raw_request)
    legacy_prompt = build_legacy_prompt(
        query=request.query,
        context=request.context,
        chat_history=request.chat_history,
    )


    fill_prompt_ko = build_fill_prompt_ko(
        guided_fields=request.guided_fields,
        part_name=request.part_name,
        part_number=request.part_number,
    )
    final_prompt = f"{legacy_prompt}\n\n{fill_prompt_ko}".strip()

    api_url = os.getenv(
        "DOCUMENT_FILLER_API_URL",
        "http://host.docker.internal:8000/v1/chat/completions",
    )
    model = os.getenv("DOCUMENT_FILLER_MODEL_ID", "gemma-4-31b-it")
    payload = {
        "model": model,
        "messages": [{"role": "user", "content": final_prompt}],
        "stream": False,
        "temperature": 0.2,
    }

    raw_response = call_chat_completions(api_url=api_url, payload=payload)
    message_content = extract_message_content(raw_response)
    suggested_fields = extract_suggested_fields(message_content)
    if not suggested_fields and request.guided_fields:
        suggested_fields = fallback_field_mapping(message_content, request.guided_fields)

    assistant_summary = build_assistant_summary(suggested_fields)
    return FillFieldsResponse(
        status="ok",
        tool="문서 필드 초안 생성 도구",
        message=format_suggested_fields_message(suggested_fields),
        suggested_fields=suggested_fields,
        assistant_summary=assistant_summary,
    )


@app.post("/search/query", response_model=SearchDocumentResponse)
def search_documents(payload: SearchDocumentRequest, raw_request: Request) -> SearchDocumentResponse:
    require_registered_tool_user(raw_request)
    identity = resolve_search_identity(payload, raw_request)
    if identity.rank == "low_rank" and is_sensitive_low_rank_query(payload.query):
        return SearchDocumentResponse(
            status="ok",
            username=identity.username,
            rank=identity.rank,
            auth_source=identity.auth_source,
            query=payload.query,
            parameters={"blocked_reason": "low_rank_sensitive_topic"},
            filtered_files_count=0,
            references=[],
            answer="권한상 해당 주제의 문서는 확인할 수 없습니다.",
            assistant_summary=f"{identity.auth_source} 경로에서 {identity.rank} 권한으로 민감 주제 검색을 차단했습니다.",
        )
    result = run_original_legacy_search(
        query=payload.query,
        rank=identity.rank,
        chat_history=payload.chat_history,
    )
    refs = [
        SearchReference(file_path=item["file_path"], score=float(item["score"]))
        for item in result["references"]
    ]
    return SearchDocumentResponse(
        status="ok",
        username=identity.username,
        rank=identity.rank,
        auth_source=identity.auth_source,
        query=payload.query,
        parameters=result["parameters"],
        filtered_files_count=result["filtered_files_count"],
        references=refs,
        answer=result["answer"],
        assistant_summary=f"{identity.auth_source} 경로에서 {identity.rank} 권한으로 문서 검색을 완료했습니다. {result['filtered_files_count']}건 후보 중 {len(refs)}건을 근거로 답변했습니다.",
    )


def is_sensitive_low_rank_query(query: str) -> bool:
    normalized = re.sub(r"\s+", "", query.lower())
    sensitive_terms = [
        "운영현황",
        "현황비용",
        "수익구조",
        "원가구조",
        "bep",
        "손익",
        "매출",
        "영업이익",
        "비용",
        "인원",
        "물동량",
    ]
    if any(term in normalized for term in sensitive_terms):
        return True

    return "운영" in normalized and any(
        term in normalized for term in ["현황", "비용", "인원", "물동량", "수익", "원가"]
    )


def get_legacy_rag_generator(api_url: str) -> Any:
    global _LEGACY_RAG_GENERATOR

    with _LEGACY_RAG_GENERATOR_LOCK:
        if _LEGACY_RAG_GENERATOR is None:
            cwd = Path.cwd()
            try:
                os.chdir(LEGACY_MD_INDEXER_DIR)
                from rag_generator import RAGGenerator

                _LEGACY_RAG_GENERATOR = RAGGenerator(str(LEGACY_PROCESSED_DIR))
            finally:
                os.chdir(cwd)
        _LEGACY_RAG_GENERATOR.api_url = api_url
        return _LEGACY_RAG_GENERATOR


def run_original_legacy_search(
    query: str,
    rank: str,
    chat_history: list[ChatTurn],
) -> dict[str, Any]:
    cwd = Path.cwd()
    try:
        os.chdir(LEGACY_MD_INDEXER_DIR)
        from agentic_router import AgenticRouter

        api_url = os.getenv(
            "DOCUMENT_FILLER_API_URL",
            "http://host.docker.internal:8000/v1/chat/completions",
        )
        router = AgenticRouter(str(LEGACY_CATALOG_FILE))
        generator = get_legacy_rag_generator(api_url)
        router.api_url = api_url
        generator.api_url = api_url

        routed = router.route_query(query, user_rank=rank)
        references: list[dict[str, Any]] = []
        answer = "조건에 부합하는 문서가 없어 답변할 수 없습니다."
        history = [{"role": turn.role, "content": turn.content} for turn in chat_history]
        for chunk in generator.generate_stream(
            query=routed["query"],
            target_files=routed["target_files"],
            search_query=routed.get("search_query"),
            catalog=router.catalog,
            params=routed.get("parameters"),
            chat_history=history,
        ):
            answer = chunk.get("answer", answer)
            references = chunk.get("references", references) or references

        return {
            "parameters": routed.get("parameters") or {},
            "filtered_files_count": len(routed.get("target_files") or []),
            "references": references,
            "answer": answer,
        }
    except HTTPException:
        raise
    except Exception as exc:
        raise HTTPException(status_code=502, detail=f"원본 레거시 검색 호출 실패: {exc}") from exc
    finally:
        os.chdir(cwd)


def normalize_text(content: str) -> str:
    text = content.replace("\r\n", "\n").replace("\r", "\n").strip()
    text = re.sub(r"\n{3,}", "\n\n", text)
    text = re.sub(r"[ \t]+", " ", text)
    return text


def split_sections(content: str) -> List[str]:
    if not content:
        return []
    return [chunk.strip() for chunk in content.split("\n\n") if chunk.strip()]


def to_markdown(sections: List[str]) -> str:
    if not sections:
        return "# Parsed Document\n\n_No content_"

    lines = ["# Parsed Document", ""]
    for index, section in enumerate(sections, start=1):
        lines.append(f"## Section {index}")
        lines.append("")
        if looks_like_list(section):
            for item in section.split("\n"):
                item = item.strip("-*• ").strip()
                if item:
                    lines.append(f"- {item}")
        else:
            lines.append(section)
        lines.append("")
    return "\n".join(lines).strip()


def looks_like_list(section: str) -> bool:
    lines = [line.strip() for line in section.split("\n") if line.strip()]
    if len(lines) < 2:
        return False
    return sum(1 for line in lines if re.match(r"^[-*•]|\d+\.", line)) >= max(2, len(lines) // 2)


def build_report_html(title: str, markdown: str) -> str:
    renderer = MarkdownIt("commonmark", {"html": False}).enable("table").enable("strikethrough")
    body = renderer.render(markdown)
    generated_at = datetime.now(timezone.utc).astimezone().strftime("%Y-%m-%d %H:%M")
    return f"""<!doctype html>
<html lang="ko">
<head>
  <meta charset="utf-8">
  <title>{html.escape(title)}</title>
  <style>{REPORT_CSS}</style>
</head>
<body>
  <main>
    <h1>{html.escape(title)}</h1>
    <p class="meta">생성 시각: {html.escape(generated_at)}</p>
    {body}
  </main>
</body>
</html>"""


async def render_pdf_with_chromium(
    html_content: str,
    output_path: Path,
    page_size: str,
    landscape: bool,
) -> None:
    try:
        async with async_playwright() as playwright:
            chromium_args = ["--disable-dev-shm-usage"]
            if os.getenv("CHROMIUM_DISABLE_SANDBOX", "false").lower() == "true":
                chromium_args.append("--no-sandbox")
            browser = await playwright.chromium.launch(
                headless=True,
                executable_path=CHROMIUM_EXECUTABLE_PATH,
                args=chromium_args,
            )
            page = await browser.new_page()
            await page.set_content(html_content, wait_until="networkidle")
            await page.pdf(
                path=str(output_path),
                format=page_size,
                landscape=landscape,
                print_background=True,
                margin={"top": "0", "right": "0", "bottom": "0", "left": "0"},
            )
            await browser.close()
    except Exception as exc:
        raise HTTPException(status_code=500, detail=f"failed to render PDF: {exc}") from exc


def sanitize_pdf_filename(raw: str) -> str:
    base = raw.rsplit("/", 1)[-1].rsplit("\\", 1)[-1].strip()
    if base.lower().endswith(".pdf"):
        base = base[:-4]
    base = re.sub(r"[^0-9A-Za-z가-힣._ -]+", "_", base)
    base = re.sub(r"\s+", "_", base).strip("._- ")
    if not base:
        base = "report"
    stamp = datetime.now().strftime("%Y%m%d_%H%M%S")
    return f"{base}_{stamp}.pdf"


def build_legacy_prompt(query: str, context: str, chat_history: list[ChatTurn]) -> str:
    history_block = ""
    if chat_history:
        lines = []
        for turn in chat_history[-10:]:
            role_label = "사용자" if turn.role == "user" else "시스템"
            lines.append(f"{role_label}: {turn.content}")
        history_block = "\n[이전 대화]\n" + "\n".join(lines) + "\n"
    return LEGACY_RAG_PROMPT_TEMPLATE.format(
        context=context.strip(),
        history_block=history_block,
        query=query.strip(),
    )


def build_fill_prompt_ko(
    guided_fields: list[GuidedField],
    part_name: str | None,
    part_number: str | None,
) -> str:
    fields = guided_fields or [
        GuidedField(
            field="구매사유",
            label="구매 사유",
            prompt="이 부품이 왜 지금 필요한지 현장 맥락과 위험을 포함해 한국어로 정리해줘.",
        )
    ]

    lines = [
        "[문서 채우기 추가 지시]",
        "아래 필드들은 문서에 바로 넣을 수 있게 반드시 한국어로만 작성한다.",
        "각 필드 값은 추측을 줄이고, 제공된 근거에서 확인 가능한 내용 중심으로 간결하게 작성한다.",
        "출력은 반드시 JSON 객체 하나만 반환한다.",
        '형식 예시: {"fields":{"구매사유":"...","담당자 직접입력":"..."}}',
        "",
        "[채워야 할 필드]",
    ]
    for item in fields:
        label = item.label or item.field
        lines.append(f"- 필드명: {item.field}")
        lines.append(f"  라벨: {label}")
        lines.append(f"  작성지시: {item.prompt}")

    if part_name and part_number:
        lines.extend(
            [
                "",
                "[부품 설명 작성 참고]",
                LEGACY_PART_FUNCTION_PROMPT_TEMPLATE.format(
                    part_name=part_name.strip(),
                    part_number=part_number.strip(),
                ),
                "위 부품 설명은 필요한 경우 관련 필드의 본문 재료로만 사용하고, 전체 출력 형식은 반드시 JSON 객체를 유지한다.",
            ]
        )

    return "\n".join(lines).strip()


def call_chat_completions(api_url: str, payload: dict[str, Any]) -> str:
    request = urllib.request.Request(
        api_url,
        data=json.dumps(payload).encode("utf-8"),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=180) as response:
            return response.read().decode("utf-8")
    except urllib.error.HTTPError as error:
        detail = error.read().decode("utf-8", errors="replace")
        raise HTTPException(status_code=502, detail=f"LLM 호출 실패: HTTP {error.code} {detail}") from error
    except urllib.error.URLError as error:
        raise HTTPException(status_code=502, detail=f"LLM 호출 실패: {error.reason}") from error


def extract_message_content(raw_response: str) -> str:
    try:
        data = json.loads(raw_response)
    except json.JSONDecodeError:
        return raw_response.strip()

    choices = data.get("choices") or []
    if not choices:
        return raw_response.strip()

    message = choices[0].get("message") or {}
    content = message.get("content")
    if isinstance(content, str):
        return content.strip()
    return raw_response.strip()


def extract_suggested_fields(content: str) -> dict[str, str]:
    candidate = extract_json_block(content)
    if not candidate:
        return {}
    try:
        data = json.loads(candidate)
    except json.JSONDecodeError:
        return {}

    fields = data.get("fields")
    if not isinstance(fields, dict):
        return {}
    return {
        str(key): str(value).strip()
        for key, value in fields.items()
        if str(value).strip()
    }


def extract_json_block(content: str) -> str | None:
    fenced = re.search(r"```(?:json)?\s*(\{.*\})\s*```", content, flags=re.DOTALL)
    if fenced:
        return fenced.group(1)

    start = content.find("{")
    end = content.rfind("}")
    if start == -1 or end == -1 or end <= start:
        return None
    return content[start : end + 1]


def fallback_field_mapping(content: str, guided_fields: list[GuidedField]) -> dict[str, str]:
    text = content.strip()
    if not text:
        return {}
    if len(guided_fields) == 1:
        return {guided_fields[0].field: text}
    return {}


def build_assistant_summary(suggested_fields: dict[str, str]) -> str:
    if not suggested_fields:
        return "문서 채우기 초안을 만들지 못했습니다. 응답 원문을 확인해 주세요."
    field_names = ", ".join(suggested_fields.keys())
    return f"문서 채우기 초안을 생성했습니다. 채워진 필드는 {field_names} 입니다."


def hash_password(password: str) -> str:
    return hashlib.sha256(password.encode()).hexdigest()


def load_legacy_users() -> dict[str, Any]:
    if not LEGACY_USERS_FILE.exists():
        return {}
    try:
        return json.loads(LEGACY_USERS_FILE.read_text(encoding="utf-8"))
    except Exception:
        return {}


def normalize_document_rank(rank: str | None) -> str | None:
    normalized_rank = (rank or "").strip().lower()
    if normalized_rank in {"hi_rank", "low_rank"}:
        return normalized_rank
    return None


def parse_openwebui_groups(raw_value: str | None) -> list[str]:
    if not raw_value:
        return []
    return [group.strip() for group in raw_value.split(",") if group.strip()]


def infer_rank_from_openwebui_groups(group_names: list[str]) -> str | None:
    normalized_groups = [group.lower() for group in group_names]
    if any("개발자 그룹" in group or "회사 그룹 - 팀장" in group for group in normalized_groups):
        return "hi_rank"
    if any("회사 그룹 - 사원" in group for group in normalized_groups):
        return "low_rank"
    return None


def resolve_openwebui_identity(
    user_email: str | None,
    explicit_rank: str | None = None,
    group_names: list[str] | None = None,
) -> ResolvedSearchIdentity | None:
    normalized_email = (user_email or "").strip().lower()
    resolved_rank = normalize_document_rank(explicit_rank) or infer_rank_from_openwebui_groups(
        group_names or []
    )

    if not normalized_email and not resolved_rank:
        return None

    if not resolved_rank:
        users = load_legacy_users()
        resolved_rank = (users.get(normalized_email) or {}).get("rank") or OPEN_WEBUI_EMAIL_RANK_MAP.get(
            normalized_email
        )
        if not resolved_rank:
            return None

    return ResolvedSearchIdentity(
        username=normalized_email or "openwebui_forwarded",
        rank=resolved_rank,
        auth_source="openwebui_userinfo" if explicit_rank or group_names else "openwebui_email",
    )


def authenticate_legacy_user(username: str, password: str) -> str:
    if not LEGACY_USERS_FILE.exists():
        raise HTTPException(status_code=500, detail="레거시 users.json 파일을 찾지 못했습니다.")
    users = json.loads(LEGACY_USERS_FILE.read_text(encoding="utf-8"))
    if username not in users:
        raise HTTPException(status_code=401, detail="존재하지 않는 아이디입니다.")
    if users[username]["password"] != hash_password(password):
        raise HTTPException(status_code=401, detail="비밀번호가 일치하지 않습니다.")
    return users[username].get("rank", "low_rank")


def resolve_legacy_identity(username: str | None, password: str | None) -> ResolvedSearchIdentity | None:
    normalized_username = (username or "").strip()
    normalized_password = password or ""
    if not normalized_username and not normalized_password:
        return None
    if not normalized_username or not normalized_password:
        raise HTTPException(
            status_code=400,
            detail="레거시 인증은 username과 password를 함께 제공해야 합니다.",
        )

    return ResolvedSearchIdentity(
        username=normalized_username,
        rank=authenticate_legacy_user(normalized_username, normalized_password),
        auth_source="legacy_credentials",
    )


def resolve_forwarded_openwebui_identity(raw_request: Request) -> ResolvedSearchIdentity | None:
    forwarded_email = raw_request.headers.get(OPEN_WEBUI_USER_EMAIL_HEADER)
    forwarded_rank = raw_request.headers.get(OPEN_WEBUI_USER_RANK_HEADER)
    forwarded_groups = parse_openwebui_groups(raw_request.headers.get(OPEN_WEBUI_USER_GROUPS_HEADER))
    return resolve_openwebui_identity(
        forwarded_email,
        explicit_rank=forwarded_rank,
        group_names=forwarded_groups,
    )


def resolve_search_identity(payload: SearchDocumentRequest, raw_request: Request) -> ResolvedSearchIdentity:
    forwarded_identity = resolve_forwarded_openwebui_identity(raw_request)
    if forwarded_identity is not None:
        return forwarded_identity

    openwebui_identity = resolve_openwebui_identity(payload.user_email)
    if openwebui_identity is not None:
        return openwebui_identity

    legacy_identity = resolve_legacy_identity(payload.username, payload.password)
    if legacy_identity is not None:
        return legacy_identity

    return ResolvedSearchIdentity(
        username="anonymous",
        rank="low_rank",
        auth_source="anonymous_default",
    )


def load_legacy_catalog() -> dict[str, Any]:
    if not LEGACY_CATALOG_FILE.exists():
        raise HTTPException(status_code=500, detail="레거시 file_catalog.json 파일을 찾지 못했습니다.")
    return json.loads(LEGACY_CATALOG_FILE.read_text(encoding="utf-8"))


def extract_search_parameters(query: str) -> dict[str, Any]:
    prompt = LEGACY_ROUTER_PROMPT_TEMPLATE.format(query=query)
    payload = {
        "model": os.getenv("DOCUMENT_FILLER_MODEL_ID", "gemma-4-31b-it"),
        "messages": [{"role": "user", "content": prompt}],
        "stream": False,
        "temperature": 0.0,
    }
    api_url = os.getenv(
        "DOCUMENT_FILLER_API_URL",
        "http://host.docker.internal:8000/v1/chat/completions",
    )
    raw_response = call_chat_completions(api_url=api_url, payload=payload)
    content = extract_message_content(raw_response)
    block = extract_json_block(content)
    if not block:
        return {"years": [], "months": [], "search_query": query}
    try:
        params = json.loads(block)
    except json.JSONDecodeError:
        return {"years": [], "months": [], "search_query": query}
    years = [year for year in (params.get("years") or []) if isinstance(year, int) and 1990 <= year <= 2030]
    months = [month for month in (params.get("months") or []) if isinstance(month, int) and 1 <= month <= 12]
    search_query = str(params.get("search_query") or query).strip()
    return {"years": years, "months": months, "search_query": search_query}


def filter_catalog_by_rank(catalog: dict[str, Any], params: dict[str, Any], rank: str) -> list[str]:
    target_years = params.get("years") or []
    target_months = params.get("months") or []
    filtered_files: list[str] = []

    for file_path, meta in catalog.items():
        if meta.get("status") != "COMPLETED":
            continue
        if rank == "low_rank":
            if not file_path.startswith("low_rank/"):
                continue
        elif rank == "hi_rank":
            if not (file_path.startswith("hi_rank/") or file_path.startswith("low_rank/")):
                continue
        else:
            continue
        if not target_years and not target_months:
            filtered_files.append(file_path)
            continue
        matched = False
        for entry in meta.get("dates", []):
            year = entry.get("year")
            month = entry.get("month")
            if year is not None and (year < 1990 or year > 2030):
                continue
            if month is not None and (month < 1 or month > 12):
                continue
            year_ok = (not target_years) or (year in target_years)
            month_ok = (not target_months) or (month in target_months)
            if year_ok and month_ok:
                matched = True
                break
        if matched:
            filtered_files.append(file_path)

    return filtered_files


def get_kiwi() -> Kiwi:
    global _KIWI
    if _KIWI is None:
        _KIWI = Kiwi()
    return _KIWI


def tokenize_for_bm25(text: str) -> list[str]:
    if not text.strip():
        return []
    kiwi = get_kiwi()
    tokens = kiwi.tokenize(text)
    return [token.form for token in tokens if not token.tag.startswith("J") and not token.tag.startswith("S")]


def retrieve_bm25_targets(
    query: str,
    file_paths: list[str],
    catalog: dict[str, Any],
    params: dict[str, Any],
) -> list[dict[str, Any]]:
    docs: list[str] = []
    valid_paths: list[str] = []
    for path in file_paths:
        fpath = LEGACY_PROCESSED_DIR / path
        if not fpath.exists():
            continue
        content = fpath.read_text(encoding="utf-8")
        if not content.strip():
            continue
        docs.append(content)
        valid_paths.append(path)

    if not docs:
        return []

    tokenized_docs = [tokenize_for_bm25(doc) for doc in docs]
    bm25 = BM25Okapi(tokenized_docs)
    tokenized_query = tokenize_for_bm25(query)
    scores = bm25.get_scores(tokenized_query)

    target_years = set(params.get("years") or [])
    target_months = set(params.get("months") or [])
    if target_years or target_months:
        for idx, path in enumerate(valid_paths):
            meta = catalog.get(path, {})
            for entry in meta.get("dates", []):
                year = entry.get("year")
                month = entry.get("month")
                if (target_years and year in target_years) or (target_months and month in target_months):
                    scores[idx] *= 1.3
                    break

    top_indices = sorted(range(len(scores)), key=lambda i: scores[i], reverse=True)[:5]
    results: list[dict[str, Any]] = []
    for idx in top_indices:
        if scores[idx] > 0.0:
            results.append({"file_path": valid_paths[idx], "score": round(float(scores[idx]), 4)})
    return results


def load_context_for_targets(targets: list[dict[str, Any]], catalog: dict[str, Any]) -> str:
    blocks: list[str] = []
    current_len = 0
    max_char_limit = 160000

    for item in targets:
        file_path = item["file_path"]
        fpath = LEGACY_PROCESSED_DIR / file_path
        if not fpath.exists():
            continue
        text = fpath.read_text(encoding="utf-8")
        meta = catalog.get(file_path, {})
        valid_dates = []
        for entry in meta.get("dates", []):
            year = entry.get("year")
            month = entry.get("month")
            if isinstance(year, int) and isinstance(month, int) and 1990 <= year <= 2030 and 1 <= month <= 12:
                valid_dates.append(f"{year}-{month:02d}")
        date_str = f" | Dates: {', '.join(valid_dates)}" if valid_dates else ""
        block = f"--- [Doc: {file_path}{date_str}] ---\n{text}\n\n"
        if current_len + len(block) > max_char_limit:
            allowed_len = max_char_limit - current_len
            if allowed_len > 100:
                blocks.append(block[:allowed_len] + "\n...[Truncated]...")
            break
        blocks.append(block)
        current_len += len(block)
    return "".join(blocks)


def generate_search_answer(
    query: str,
    targets: list[dict[str, Any]],
    catalog: dict[str, Any],
    chat_history: list[ChatTurn],
) -> str:
    context = load_context_for_targets(targets, catalog)
    history_block = ""
    if chat_history:
        lines = []
        for turn in chat_history[-10:]:
            role_label = "사용자" if turn.role == "user" else "시스템"
            lines.append(f"{role_label}: {turn.content}")
        history_block = "\n[이전 대화]\n" + "\n".join(lines) + "\n"

    prompt = LEGACY_RAG_PROMPT_TEMPLATE.format(
        context=context,
        history_block=history_block,
        query=query,
    )
    payload = {
        "model": os.getenv("DOCUMENT_FILLER_MODEL_ID", "gemma-4-31b-it"),
        "messages": [{"role": "user", "content": prompt}],
        "stream": False,
        "temperature": 0.6,
    }
    api_url = os.getenv(
        "DOCUMENT_FILLER_API_URL",
        "http://host.docker.internal:8000/v1/chat/completions",
    )
    raw_response = call_chat_completions(api_url=api_url, payload=payload)
    return extract_message_content(raw_response)
