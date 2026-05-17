# Enterprise Custom Document Tools

Current operating snapshot for the CDXVI Open WebUI document automation workspace.
This repository is kept as a cleaned public source snapshot. Runtime data, Open WebUI databases, local credentials, generated reports, stock snapshots, and indexed document bodies are intentionally excluded.

## Current Progress

As of 2026-05-17, the workspace has been consolidated around a single Open WebUI front end and four tool services behind a Caddy gateway.

- `gemma-4-31b-it` is the main user-facing model and is displayed in Open WebUI as `Æ CDXVI Indexer`.
- The former `cdxvi-indexer` wrapper model has been folded into the base Gemma model to avoid `Model not found` failures.
- `gemma-raw-dev` is reserved as the developer-only raw Gemma preset.
- Open WebUI permissions were redesigned into four Korean groups: `개발자`, `관리자`, `팀장`, `사원`.
- General users use `Æ CDXVI Indexer`; developer-only raw Gemma visibility is separated from normal users.
- Tool availability is controlled by Open WebUI tool grants and model `toolIds`, not per-user hardcoded allow lists inside tool services.
- Search sensitivity is controlled through forwarded Open WebUI group/rank metadata.
- Email-based search rank fallback was removed from service code; `SEARCH_EMAIL_RANK_OVERRIDES` remains only as an explicit emergency override.
- Streaming reasoning/tool logs are cleaned in the Open WebUI overlay so process data is separated from the final answer.
- Download grants and retention for generated PDF/Word/Excel files are handled by the markdown renderer service.
- Scattered Markdown notes were removed; this README is now the single public progress document.

## Runtime Layout

```text
Open WebUI
  -> vLLM API: http://192.168.100.13:8000/v1/chat/completions
  -> Caddy gateway: http://192.168.100.202
       /document/* -> document-service        :8001
       /search/*   -> search-service          :8002
       /render/*   -> markdown-pdf-service    :8003
       /download/* -> markdown-pdf-service    :8003
       /admin/*    -> markdown-pdf-service    :8003
       /web/*      -> web-service             :8004
```

## Services

| Service | Path | Responsibility |
| --- | --- | --- |
| document-service | `rust-service/` | Purchase documents, stock queries, inventory reports, document download proxy |
| search-service | `search-service/` | Internal document search and rank-based sensitivity filtering |
| markdown-pdf-service | `markdown-pdf-service/` | Markdown to PDF, chat/report DOCX, XLSX export, download grants and retention |
| web-service | `web-service/` | URL fetch and readable Markdown extraction |
| Caddy gateway | `caddy/` | Single public HTTP entry point for Open WebUI tool calls |
| Open WebUI overlay | `runtime/` | UI cleanup for reasoning/tool-log rendering |

## Current Defaults

The public example defaults mirror the current server deployment.

| Setting | Default |
| --- | --- |
| Public gateway | `http://192.168.100.202` |
| vLLM chat completions API | `http://192.168.100.13:8000/v1/chat/completions` |
| Main model ID | `gemma-4-31b-it` |
| Main model display name | `Æ CDXVI Indexer` |
| Developer raw model | `gemma-raw-dev` |
| High-rank groups | `개발자,관리자,팀장` |
| Low-rank group | `사원` |

## Open WebUI Permission Model

| Group | Document rank | Current intent |
| --- | --- | --- |
| 개발자 | `hi_rank` | Full Open WebUI workspace controls, prompt/tool/model editing, developer raw model access |
| 관리자 | `hi_rank` | Sensitive document access and operational controls |
| 팀장 | `hi_rank` | Team-lead sensitive document access |
| 사원 | `low_rank` | Standard model/tool usage with sensitive search filtering |

Current bootstrap membership defaults are stored in `.env.example` as environment-overridable values. Operational scripts are kept out of the public snapshot because they directly modify the live Open WebUI SQLite database.

## Data Boundary

The repository intentionally does not include:

- `.env` or local credential files
- Open WebUI database files
- generated PDF, DOCX, XLSX, ZIP outputs
- stock/input Excel files and generated stock snapshots
- indexed internal Markdown document bodies
- build artifacts and virtual environments

Only source code, configuration templates, gateway configuration, runtime overlay files, and document templates intended for reuse are kept.

## Verification Status

Last server-side verification performed during this cleanup:

- Python syntax check passed for modified Python services and maintenance helpers.
- `docker compose config` passed.
- Placeholder/default scan found no remaining `your-model-id`, old `host.docker.internal:8000` API fallback, hardcoded email rank map, or absolute repository path in upload-target files.
- Rust formatting could not be checked on the server because `cargo` was not available in the server account PATH.
