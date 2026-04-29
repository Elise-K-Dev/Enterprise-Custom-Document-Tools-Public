from __future__ import annotations

import os
import secrets
from datetime import datetime, timezone
from typing import Any
from urllib.parse import urlparse

from crawl4ai import AsyncWebCrawler, BrowserConfig, CrawlerRunConfig
from crawl4ai.content_filter_strategy import PruningContentFilter
from crawl4ai.markdown_generation_strategy import DefaultMarkdownGenerator
from fastapi import FastAPI, HTTPException, Request
from fastapi.middleware.cors import CORSMiddleware
from pydantic import BaseModel, Field


app = FastAPI(title="web-service", version="0.2.0", openapi_url=None)
app.add_middleware(
    CORSMiddleware,
    allow_origins=["*"],
    allow_credentials=False,
    allow_methods=["*"],
    allow_headers=["*"],
)


PUBLIC_BASE_URL = os.getenv("WEB_SERVICE_PUBLIC_BASE_URL", "http://127.0.0.1:8004").rstrip("/")
INTERNAL_TOKEN_HEADER = "x-port-project-internal-token"
OPEN_WEBUI_USER_EMAIL_HEADER = "x-openwebui-user-email"
OPEN_WEBUI_USER_ID_HEADER = "x-openwebui-user-id"

# Crawler defaults. delay_before_return_html lets SPA pages finish hydration.
SPA_HYDRATION_DELAY_SECONDS = float(os.getenv("WEB_SPA_HYDRATION_DELAY", "2.0"))
PRUNING_THRESHOLD = float(os.getenv("WEB_PRUNING_THRESHOLD", "0.48"))


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
    if not email or not user_id:
        raise HTTPException(status_code=401, detail="registered Open WebUI account is required")
    return {"email": email, "user_id": user_id}


def validate_url(url: str) -> str:
    cleaned = (url or "").strip()
    if not cleaned:
        raise HTTPException(status_code=400, detail="url is required")
    parsed = urlparse(cleaned)
    if parsed.scheme not in ("http", "https"):
        raise HTTPException(status_code=400, detail="only http/https URLs are supported")
    if not parsed.netloc:
        raise HTTPException(status_code=400, detail="url is missing host")
    return cleaned


def build_run_config() -> CrawlerRunConfig:
    return CrawlerRunConfig(
        markdown_generator=DefaultMarkdownGenerator(
            content_filter=PruningContentFilter(
                threshold=PRUNING_THRESHOLD,
                threshold_type="dynamic",
            ),
        ),
        delay_before_return_html=SPA_HYDRATION_DELAY_SECONDS,
    )


def select_markdown(result: Any) -> str:
    """Prefer fit_markdown (after pruning) over raw_markdown.
    Falls back to raw if pruning removed everything."""
    md = getattr(result, "markdown", None)
    if md is None:
        return ""
    fit = (getattr(md, "fit_markdown", "") or "").strip()
    if fit:
        return fit
    raw = (getattr(md, "raw_markdown", "") or "").strip()
    if raw:
        return raw
    return str(md).strip()


async def crawl_once(url: str) -> dict[str, Any]:
    browser = BrowserConfig(headless=True, verbose=False)
    config = build_run_config()
    async with AsyncWebCrawler(config=browser) as crawler:
        result = await crawler.arun(url=url, config=config)

    if not result.success and not getattr(result, "html", ""):
        detail = getattr(result, "error_message", None) or "fetch failed"
        raise HTTPException(status_code=502, detail=f"fetch failed for {url}: {detail}")

    markdown = select_markdown(result)
    metadata = result.metadata or {}
    title = (metadata.get("title") or "").strip()
    author = (metadata.get("author") or "").strip()
    published = (metadata.get("published_time") or metadata.get("date") or "").strip()

    if not markdown and not title:
        raise HTTPException(
            status_code=422,
            detail=f"could not extract any readable content from {url}",
        )

    return {
        "url": result.url or url,
        "title": title,
        "author": author,
        "published": published,
        "markdown": markdown,
        "status_code": result.status_code or 0,
    }


class FetchRequest(BaseModel):
    url: str = Field(..., description="대상 페이지 URL (http 또는 https)")


class FetchResponse(BaseModel):
    url: str
    title: str
    author: str
    published: str
    markdown: str
    status_code: int
    fetched_at: str


@app.get("/health")
def health() -> dict[str, str]:
    return {"status": "ok"}


@app.post("/web/fetch", response_model=FetchResponse)
async def web_fetch(req: FetchRequest, raw_request: Request) -> FetchResponse:
    require_registered_tool_user(raw_request)
    url = validate_url(req.url)
    extracted = await crawl_once(url)
    return FetchResponse(
        url=extracted["url"],
        title=extracted["title"],
        author=extracted["author"],
        published=extracted["published"],
        markdown=extracted["markdown"],
        status_code=extracted["status_code"],
        fetched_at=datetime.now(timezone.utc).isoformat(),
    )


@app.get("/openapi.json")
def openapi_spec() -> dict[str, Any]:
    return {
        "openapi": "3.0.0",
        "info": {
            "title": "Web Fetch",
            "version": "0.2.0",
            "description": (
                "사용자가 제공한 외부 웹 URL의 본문을 가져오는 도구. "
                "이 도구는 검색 기능을 제공하지 않는다. 사용자가 키워드 검색을 요청하면 "
                "'현재 인터넷 검색 기능은 제공하지 않습니다. 확인하실 페이지의 URL을 직접 알려주시면 내용을 가져와 드리겠습니다.'라고 안내한다. "
                "내부 사내 문서 검색에는 사용하지 않는다 (그 용도는 document_search)."
            ),
        },
        "servers": [{"url": PUBLIC_BASE_URL}],
        "paths": {
            "/health": {
                "get": {
                    "operationId": "web_health_check",
                    "summary": "Health check",
                    "responses": {"200": {"description": "Healthy"}},
                }
            },
            "/web/fetch": {
                "post": {
                    "operationId": "fetch_web_page",
                    "summary": "단일 웹페이지 본문을 Markdown으로 가져오기",
                    "description": (
                        "사용자가 URL을 직접 제공하면 이 도구로 페이지를 가져와 본문 Markdown과 제목, 작성자, 작성일을 추출한다. "
                        "내부적으로 헤드리스 Chromium으로 렌더링하므로 SPA, 쇼핑몰, 포럼 등 JavaScript 페이지도 처리된다. "
                        "사용자가 키워드로 인터넷 검색을 요청한 경우에는 이 도구를 호출하지 말고, "
                        "'현재 인터넷 검색 기능은 제공하지 않습니다. 확인하실 페이지의 URL을 직접 알려주시면 내용을 가져와 드리겠습니다.'라고 안내한다. "
                        "사내 문서나 레거시 자료에는 사용하지 않는다."
                    ),
                    "requestBody": {
                        "required": True,
                        "content": {
                            "application/json": {
                                "schema": {
                                    "type": "object",
                                    "required": ["url"],
                                    "properties": {
                                        "url": {
                                            "type": "string",
                                            "description": "가져올 페이지의 http/https URL",
                                        },
                                    },
                                }
                            }
                        },
                    },
                    "responses": {
                        "200": {
                            "description": "추출된 본문",
                            "content": {
                                "application/json": {
                                    "schema": {
                                        "type": "object",
                                        "properties": {
                                            "url": {"type": "string"},
                                            "title": {"type": "string"},
                                            "author": {"type": "string"},
                                            "published": {"type": "string"},
                                            "markdown": {"type": "string"},
                                            "status_code": {"type": "integer"},
                                            "fetched_at": {"type": "string"},
                                        },
                                    }
                                }
                            },
                        }
                    },
                }
            },
        },
    }
