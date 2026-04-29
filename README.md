# Enterprise Custom Document Tools

Open WebUI를 프런트로 두고, 도구는 `Python 파서/검색 도구`와 `통합 문서 제작기` 두 축으로 구성한 사내 문서 자동화 워크스페이스입니다. 통합 문서 제작기는 Rust 기반 구매 품의문 작성기와 Markdown PDF/Word/Excel 렌더러를 묶어 재고 조회, 품의서 작성, 보고서 파일 생성을 처리합니다.

## Overview

```text
LAN User -> Open WebUI (:3000) -> vLLM Model API
                              -> Tool Calls on the same host
                                 -> Python 파서/검색 도구
                                    -> http://127.0.0.1:8002 Python Search/Parser
                                 -> 통합 문서 제작기
                                    -> http://127.0.0.1:8001 Rust 품의문/재고 API
                                    -> http://127.0.0.1:8003 Markdown PDF/Word/Excel Renderer
                                 -> 웹 페이지 가져오기 도구
                                    -> http://127.0.0.1:8004 Web Fetch
```

개발 표준 실행은 `docker-compose.host.yml` 오버레이를 함께 사용합니다. 따라서 Open WebUI가 등록해 호출하는 도구 URL은 컨테이너 서비스명(`document-service`, `parser-service`)이 아니라 실행 기기 기준 `127.0.0.1`입니다. 기본 `docker-compose.yml`만 단독 실행할 때만 Docker bridge 네트워크의 서비스명을 사용할 수 있습니다.

구성 요소:

- `open-webui`: 사용자 UI, 툴 서버 연결 설정, 실행 환경 변수
- `python-service`: Python 파서/검색 도구. RAW 문서 Markdown 변환, 레거시 문서 검색, 문서 필드 보조 API
- `rust-service`: 통합 문서 제작기 진입점. 구매 품의문 작성, 재고 조회, 구매/재고 보고서 내보내기, 렌더러 프록시 API
- `markdown-pdf-service`: 통합 문서 제작기 내부 렌더러. Markdown 보고서를 PDF/Word/Excel 파일로 렌더링하는 API
- `web-service`: 외부 웹 페이지 본문 가져오기 도구. 사용자가 제공한 URL을 받아 본문을 Markdown으로 추출 (검색 기능은 제공하지 않음)
- `scripts`: 로컬 실행, Docker 실행, Open WebUI 동기화 스크립트
- `docs`: 운영 메모와 연결 문서

## Open WebUI Tool Layout

Open WebUI에는 다음 두 도구 서버를 기본으로 등록합니다.

- `Python 파서/검색 도구` (`document_search`): 내부 문서, 업무보고 원문, 수리 이력, 날짜별 작업 기록, 기존 근거 검색 전용입니다. PDF 생성이나 구매 품의문 작성에는 사용하지 않습니다.
- `통합 문서 제작기` (`document_generation_tools`): Rust 품의문 작성기와 Markdown 렌더러를 하나의 도구로 묶습니다. 재고/품목 조회, 구매 품의서 DOCX/ZIP 생성, 재고 보고서 파일 생성, Markdown PDF 생성, 보고서/채팅 기록 Word/Excel 내보내기와 다운로드 링크 생성을 처리합니다.

## Key APIs

- `POST /document/create`
- `POST /document/fill`
- `POST /document/export`
- `GET /document/legacy/shortages`
- `GET /document/legacy/item-context`
- `POST /document/legacy/item-export`
- `POST /document/legacy/item-approve`
- `POST /document/legacy/run`
- `POST /parser/to-md`
- `POST /document/fill-fields`
- `POST /search/query`
- `POST /render/markdown-pdf`
- `POST /render/chat-docx`
- `POST /render/chat-xlsx`
- `POST /web/fetch`

Open WebUI에서는 `통합 문서 제작기`(`document_generation_tools`)가 구매 품의서, 재고 조회, 보고서 파일 생성, Markdown PDF, Word, Excel 내보내기를 단일 도구 서버로 노출합니다. `Python 파서/검색 도구`(`document_search`)는 내부 문서 검색과 근거 조회 전용으로 사용합니다.

## Legacy Stock Snapshot Rules

Rust 레거시 문서 작성기는 `rust-service/DB/output/stock_in_out_monthly.json` 스냅샷을 기준으로 동작합니다.

- 원천 Excel(`입고/재고/출고`)은 배치 시 JSON 스냅샷 생성에만 사용합니다.
- 문서 `create/fill/export`와 품목 컨텍스트 조회는 원천 Excel을 직접 현재 조회 기준으로 사용하지 않습니다.
- `현재고`는 재고 파일 원값(`current_stock_before`)만 사용합니다.
- `movement_net_qty`와 `current_stock_updated`는 이동 이력/추정 잔량용 보조 값으로 유지되며, 실제 현재고 표시값으로 쓰지 않습니다.
- 재고 행이 없는 품목은 `inventory_confirmed=false`와 `inventory_match_status`로 분리되어 `재고 미확인`으로 취급합니다.

부족재고 조회 응답 기준:

- 확인된 부족 품목은 `현재고`, `필수재고`, `부족수량(shortage_quantity)` 기준으로 정렬/설명합니다.
- `shortage_gap`는 내부 계산 필드로 남기되, 사용자 응답은 `현재고 X개, 필수재고 Y개로 Z개 부족` 형식으로 설명합니다.
- `/document/legacy/shortages` 응답에는 Open WebUI가 그대로 사용할 수 있는 `markdown_table`와 `unverified_markdown_table`가 포함됩니다.

배치 재생성:

```bash
curl -X POST http://127.0.0.1:8001/document/legacy/run \
  -H 'Content-Type: application/json' \
  -d '{"force": true}'
```

## Local Run

Rust service:

```bash
cd rust-service
DOCUMENT_SERVICE_HOST=0.0.0.0 DOCUMENT_SERVICE_PORT=8001 cargo run
```

테스트:

```bash
PATH='/home/elise/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin':$PATH \
cargo test --manifest-path Cargo.toml
```

Python service:

```bash
cd python-service
python -m venv .venv
. .venv/bin/activate
pip install -e .
PARSER_SERVICE_HOST=0.0.0.0 PARSER_SERVICE_PORT=8002 uvicorn app.main:app --host 0.0.0.0 --port 8002
```

Open WebUI:

```bash
bash scripts/start_openwebui_with_vllm.sh
```

Creator services:

```bash
bash scripts/start_creator_services.sh
```

Parser service:

https://github.com/H4RUming/legacy-md-indexer 

```bash
bash scripts/start_parser_service.sh
```

Markdown PDF service:

```bash
bash scripts/start_markdown_pdf_service.sh
```

Web service:

```bash
bash scripts/start_web_service.sh
```

## Docker Compose

권장 개발 실행:

```bash
cd /home/elise/Desktop/2026\ Dev/Port-Project
bash scripts/start_openwebui_with_vllm.sh
```

구성 요약:

- `scripts/start_openwebui_with_vllm.sh`는 Open WebUI 이미지를 빌드하고, Rust/Python/Markdown PDF 서비스 이미지를 로컬에서 빌드한 뒤 `docker-compose.yml`과 `docker-compose.host.yml`을 함께 사용해 기동합니다.
- Docker Hub metadata/DNS 지연을 줄이기 위해 서비스 이미지는 `python:3.11-slim`, `rust:1.95`, `debian:bookworm-slim`을 로컬 alias 이미지로 태그한 뒤 `--pull=false`로 빌드합니다.
- `docker-compose.host.yml`을 함께 쓰면 도구 서버들은 실행 기기 기준 `127.0.0.1:8001`, `127.0.0.1:8002`, `127.0.0.1:8003`으로 Open WebUI에 등록됩니다.
- `vLLM` 모델 API는 도구 서버가 아니며 고정 주소 `http://host.docker.internal:8000/v1`로 호출합니다.
- `document-service`는 `./rust-service/DB`를 `/app/DB`로 마운트하고 구매 품의서, 재고 조회, 다운로드 프록시를 처리합니다.
- `parser-service`는 레거시 검색용 Python 서비스를 함께 기동합니다.
- `markdown-pdf-service`는 Markdown 보고서 PDF, Word DOCX, Excel XLSX를 생성하고 `./markdown-pdf-service/output`에 저장합니다.
- Chromium 기반 PDF 렌더링은 컨테이너 sandbox 권한 문제를 피하기 위해 `CHROMIUM_DISABLE_SANDBOX=true`로 실행합니다.

기본 compose만 직접 사용할 수도 있습니다.

```bash
docker compose up -d --build
```

다만 현재 개발 표준은 host overlay를 포함한 시작 스크립트입니다. 직접 실행할 경우 Open WebUI 런타임 동기화를 별도로 수행해야 합니다.

```bash
bash scripts/sync_openwebui_runtime.sh
```

기본 compose만 단독으로 사용하는 경우에는 Open WebUI 도구 URL도 bridge 네트워크 기준으로 다시 맞춰야 합니다.

```bash
RUST_TOOL_SERVER_URL=http://document-service:8001 \
PARSER_TOOL_SERVER_URL=http://parser-service:8002 \
bash scripts/sync_openwebui_runtime.sh
```

서비스 엔드포인트:

```text
vLLM              -> http://host.docker.internal:8000/v1
Open WebUI        -> http://127.0.0.1:3000 또는 http://<서버 LAN IP>:3000
Rust creator API  -> http://127.0.0.1:8001
Python parser API -> http://127.0.0.1:8002
Markdown renderer -> http://127.0.0.1:8003
Web fetch         -> http://127.0.0.1:8004
```

Open WebUI import 예시:

- `open-webui/openwebui-rust-tools.json`
- `open-webui/openwebui-python-tools.json`
- `open-webui/openwebui-markdown-pdf-tools.json`
- `open-webui/openwebui-web-tools.json`

## Repository Notes

- 대용량 인덱스 산출물과 로컬 빌드 폴더는 저장소에서 제외합니다.
- 실제 운영용 계정 정보와 `.env` 파일은 커밋하지 않습니다.
- `python-service/legacy-md-indexer-main` 아래 문서 카탈로그와 검색 코드는 보존하되, 생성된 `processed_md` 데이터는 제외합니다.
- `markdown-pdf-service/output`, `markdown-pdf-service/output.bak.*`, Python `*.egg-info` 같은 생성 산출물은 커밋하지 않습니다.
- GitHub PAT, Open WebUI 계정 비밀번호, 내부 토큰은 README나 커밋 메시지에 기록하지 않습니다.
