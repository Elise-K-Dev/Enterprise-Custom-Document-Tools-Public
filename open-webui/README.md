# Open WebUI Integration Notes

Open WebUI는 UI와 tool call orchestration만 맡고, 실제 비즈니스 로직은 외부 서비스로 분리합니다. 현재는 `Port-Project`의 단일 Docker Compose 안에서 `open-webui`, `document-service`, `parser-service`가 함께 실행됩니다.

실행 기준:

- 모든 런타임은 `Port-Project` 안에서 시작합니다.
- 필요한 HAI-H 자산은 `Port-Project` 루트 안으로 복사해 둔 로컬 디렉터리를 사용합니다.

## Tool Routing

- `create_document` -> `POST http://document-service:8001/document/create`
- `fill_document` -> `POST http://document-service:8001/document/fill`
- `export_document` -> `POST http://document-service:8001/document/export`
- `list_inventory_items` -> `GET http://document-service:8001/document/legacy/items`
- `export_inventory_report` -> `GET http://document-service:8001/document/legacy/items/report`
- `list_shortage_items` -> `GET http://document-service:8001/document/legacy/shortages`
- `parse_to_md` -> `POST http://parser-service:8002/parser/to-md`
- `fill_document_fields_ko` -> `POST http://parser-service:8002/document/fill-fields`
- `approve_and_generate_item_document` -> `POST http://document-service:8001/document/legacy/item-approve`
- `render_markdown_pdf` -> `POST http://markdown-pdf-service:8003/render/markdown-pdf`

## Docker Network Mode

Open WebUI는 `port-project` Docker 네트워크에 함께 붙어 있으므로, 호스트 IP 대신 서비스명으로 연결합니다.

- `document-service` -> `http://document-service:8001/openapi.json`
- `parser-service` -> `http://parser-service:8002/openapi.json`
- `markdown-pdf-service` -> `http://markdown-pdf-service:8003/openapi.json`

Import 가능한 설정 파일:

- [openwebui-rust-tools.json](/home/elise/Desktop/2026%20Dev/Port-Project/open-webui/openwebui-rust-tools.json)
- [openwebui-python-tools.json](/home/elise/Desktop/2026%20Dev/Port-Project/open-webui/openwebui-python-tools.json)
- [openwebui-markdown-pdf-tools.json](/home/elise/Desktop/2026%20Dev/Port-Project/open-webui/openwebui-markdown-pdf-tools.json)

구성 원칙:

- Rust 계열 도구는 `openwebui-rust-tools.json` 하나로 묶음
- Python 계열 도구는 `openwebui-python-tools.json` 하나로 묶음
- Markdown PDF 계열 도구는 `openwebui-markdown-pdf-tools.json` 하나로 묶음
- 포트별 세부 분리 파일은 제거

## Model Backend

- `vLLM (OpenAI-compatible)` -> `http://host.docker.internal:8000/v1`

## One Command

```bash
cd "/home/elise/Desktop/2026 Dev/Port-Project"
sudo bash scripts/start_openwebui_with_vllm.sh
```

## System Prompt Direction

LLM에는 아래 원칙을 주는 편이 안전합니다.

- 어떤 tool을 호출할지 먼저 결정할 것
- `Python 파서/검색 도구`: 특정 문서, 업무보고 원문, 날짜별 작업 이력, 기존 기록을 찾을 때만 사용할 것
- `통합 문서 제작기`: 재고, 품목, 현재고, 부족수량, 단가, 구매 우선순위, 구매 품의서 생성, 보고서 파일 생성에 사용할 것
- `통합 문서 제작기`의 렌더링 함수: Markdown 기반 수리 완료 보고서, 정비 계획, 업무 보고서, 회의록, 일반 요약 보고서, 분석 결과를 PDF/Word/Excel 파일로 만들 때 사용할 것
- 사용자가 파일 형식을 명시하면 그 형식이 최우선이다. PDF는 `render_markdown_pdf`, 워드/Word/DOCX는 `render_chat_docx`, 엑셀/Excel/XLSX는 `render_chat_xlsx`를 호출할 것
- PDF 파일 생성 요청을 받으면 "직접 PDF를 생성할 수 없다"고 답하지 말고 `render_markdown_pdf`를 호출할 것
- 직전 답변이나 현재 대화 내용을 기반으로 "이거 PDF로 작성해"라고 하면 추가 검색 없이 Markdown 본문 정리 후 `render_markdown_pdf`를 호출할 것
- 보고서, 요약문, 업무보고, 재고현황 보고서를 워드/Word/DOCX로 요청하면 보고서 본문을 `transcript`에 작성한 뒤 `render_chat_docx`를 호출할 것. `title`만 보내지 말 것
- 보고서, 요약문, 업무보고, 재고현황 보고서를 엑셀/Excel/XLSX로 요청하면 표 형식 본문을 `transcript` 또는 `messages`에 작성한 뒤 `render_chat_xlsx`를 호출할 것. `title`만 보내지 말 것
- PDF 문서 생성에 근거 문서가 필요하면 `search_documents_by_rank`로 먼저 근거를 찾고, Markdown 본문 작성 후 `render_markdown_pdf`를 호출할 것
- 구매 품의서와 보고서 PDF를 동시에 요청하면 Rust 도구로 구매 품의서/재고 결과를 만든 뒤 그 결과를 요약한 Markdown 보고서를 `render_markdown_pdf`로 렌더링할 것
- 전체 품목, 재고 충분 품목, 재고 상태 필터, 품번/품명 검색, 소모속도 빠른 순 조회는 `list_inventory_items`를 우선 사용할 것
- 현재고, 재고확인상태, 구매 우선순위, 단가가 들어간 보고서 파일 요청은 `export_inventory_report`를 우선 사용할 것
- 부족/재고 없음 품목만 묻는 경우는 `list_shortage_items`를 우선 사용할 것
- 업무 보고, 회의록, 분석 결과, 요약문을 문서 파일로 요청하면 사용자가 명시한 형식의 렌더링 도구를 호출할 것. 형식이 없으면 `render_markdown_pdf`를 호출할 것
- 수리 완료 보고서, 정비 보고서, 일반 보고서 파일 요청은 `create_document`가 아니라 형식에 맞는 렌더링 도구를 사용할 것
- `create_document`, `fill_document`, `export_document`는 구매 품의서 `purchase_request` 전용으로 사용할 것
- 누락 필드가 있으면 `fill_document`를 우선 사용할 것
- 파일/RAW 변환은 `parse_to_md`로 보낼 것
- 문서 채우기 초안은 `fill_document_fields_ko`로 보낼 것
- 사용자가 `승인해`, `진행해`처럼 긍정 의사를 보이면 `approve_and_generate_item_document`를 우선 사용할 것

## Example Flow

1. 사용자가 "구매 품의서 만들어줘, SSD 3개"라고 요청
2. Open WebUI가 `create_document` 호출
3. 응답에 `missing_fields=["납품업체"]`가 오면 모델이 사용자에게 후속 질문
4. 사용자가 업체를 답하면 `fill_document` 호출
5. 모든 필드가 채워지면 `export_document` 호출

범용 보고서 PDF 흐름:

1. 사용자가 "과장님께 보고할 수리 내역 PDF 만들어줘"라고 요청
2. 필요한 근거가 있으면 `search_documents_by_rank` 호출
3. 모델이 중요도 순으로 Markdown 보고서 작성
4. `render_markdown_pdf` 호출

구매 품의서와 PDF 보고서를 동시에 요청하는 흐름:

1. 사용자가 "부족 품목 구매문서 만들고 보고용 PDF도 만들어줘"라고 요청
2. `list_shortage_items` 또는 `list_inventory_items`로 품목 확인
3. `generate_purchase_document_package` 등 Rust 구매 품의서 도구로 구매문서 생성
4. 구매문서 생성 결과를 요약한 Markdown 보고서 작성
5. `render_markdown_pdf` 호출
