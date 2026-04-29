# legacy-md-indexer

## Overview
A high-throughput **Hierarchical RAG Pipeline** designed for metadata extraction and intelligent querying across 9,000+ unstructured legacy documents. To resolve the inference bottlenecks of 32B+ VLM architectures, this system utilizes a lightweight text-LLM setup with pre-parsed Markdown (`processed_md/`), maximizing tokens per second (tk/s) while maintaining high retrieval precision.

---

## Core Pipeline

### Phase 0: Document ETL (Pre-requisite)
Transforms raw, multi-format legacy documents (PDF, DOCX, XLSX, etc.) into **structured Markdown** optimized for LLM ingestion.
- **Strict Layout Preservation:** Minimizes data loss by preserving table structures and document hierarchies.
- **OCR Integration:** Handles scanned documents via `EasyOCR` and `docling` with multi-language support (KO/EN).
- **Resource Isolation:** Prevents memory leaks during massive batch conversions using `multiprocessing.Pool` with isolated worker limits.

### Phase 1: Metadata Cataloging
Extracts metadata from all documents to generate a `file_catalog.json` for high-speed pre-filtering.
- **Hybrid Extraction:** A 2-track system combining **Regex-based fast-track** for patterns (dates, IDs) and **LLM-based dynamic taxonomy** for complex category classification.

### Phase 2: Agentic Router (Pre-filtering)
Analyzes natural language queries to dynamically extract search parameters.
- **Parameter Extraction:** Isolates specific timeframes (`years`, `months`) and the core `search_query` from the user's input.
- **Metadata Filtering:** Drastically narrows the search space from 9,000+ files to a handful of target documents before the heavy retrieval phase.

### Phase 3: Integrated Retrieval & RAG
Executes full-text search on filtered documents and generates the final response.
- **Lexical Search (BM25):** Utilizes `rank_bm25` for keyword-based ranking across the target document subset.
- **Morphological Analysis:** Integrated **Kiwi (kiwipiepy)** tokenizer to remove Korean particles (Josa) and symbols, focusing on substantive morphemes to boost retrieval accuracy.
- **128k Context Window:** Supports up to 131,072 tokens with intelligent truncation logic to prevent OOM.
- **Streaming Inference:** Real-time response generation via Ollama API for an optimized user experience.

---

## Setup

### Prerequisites
- Python 3.10+
- [Ollama](https://ollama.com/) running locally (`http://localhost:11434`)
- 모델 사전 다운로드: `ollama pull gpt-oss:120b` (또는 `setting.conf`의 `MODEL_ID` 수정)

### Installation
```bash
python -m venv .venv
source .venv/bin/activate
pip install -r requirements.txt
```

### 1. ETL — 원본 문서 변환
`files/` 디렉토리에 원본 문서(PDF, DOCX, XLSX 등)를 배치한 후 실행:
```bash
python document_etl_pipeline.py
```
`processed_md/` 디렉토리에 Markdown 변환본이 생성됩니다.

### 2. 메타데이터 카탈로그 빌드 (웹 UI 실행 전 필수)
```bash
python md_catalog_builder.py
```
프로젝트 루트에 `file_catalog.json`이 생성됩니다. **이 파일이 없으면 웹 UI가 정상 동작하지 않습니다.**

### 3. 웹 UI 실행
```bash
python web_gui.py
```
`http://localhost:7860`에서 접속하거나, 콘솔에 출력되는 공개 URL(share 링크)을 사용합니다.

---

## System Engineering & Stability

* **Crash-safe State Management:** Executes synchronous state saves immediately after processing each file, ensuring zero data loss and instant resume capability.
* **Context Defense:** Hard-coded 160,000-character truncation guardrail to prevent context overflow and memory exhaustion.
* **Reasoning Model (CoT) Defense:** Pre-emptive regex stripping of `<think>...</think>` tags to ensure consistent JSON parsing when using reasoning models like `qwen2.5`.
* **Gradio 5.x Integration:** Full support for the latest Gradio message formats, including streaming outputs and detailed interaction logging.

---

## Tech Stack
- **Language:** Python 3.10+
- **LLM Engine:** Ollama (GPT-OSS 120B)
- **NLP / Retrieval:** Kiwipiepy (Kiwi), Rank-BM25
- **ETL:** Docling, EasyOCR, Pandas
- **Frontend:** Gradio 5.x

---

## Usage

### 질의하기
입력창에 한국어로 자연어 질문을 입력하고 Enter 또는 **실행** 버튼을 누릅니다. 예시 질문 버튼을 클릭하면 자동으로 입력됩니다.

파이프라인은 두 단계로 동작합니다:
1. **Stage 1 — 아젠틱 라우터:** 질문에서 연도·월 파라미터를 추출하고, 9,000개+ 문서 카탈로그를 대상 서브셋으로 필터링합니다.
2. **Stage 2 — BM25 + RAG:** 필터링된 서브셋에 형태소 분석 기반 BM25를 적용해 Top-K 문서를 선별하고, LLM 답변을 스트리밍으로 생성합니다.

우측 패널에서 라우팅 파라미터, 소요 시간, 참조 문서 목록을 확인할 수 있습니다.

### 생성 중단
**중지** 버튼을 클릭하면 진행 중인 LLM 생성이 즉시 중단됩니다.

### 세션 초기화
**세션 초기화** 버튼으로 채팅 히스토리를 지우고 새 세션을 시작합니다.

---

## Future Scope
- **Multi-modal Re-ranking:** Implementing a VLM-based secondary verification for documents containing complex diagrams or blueprints.
- **Hybrid Vector Search:** Combining BM25 lexical search with Dense Vector embeddings to capture semantic nuances in unstructured text.
