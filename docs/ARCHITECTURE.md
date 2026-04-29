# Architecture

The project is split into small OpenAPI tool servers so Open WebUI can expose only the capabilities needed by a given user or workspace.

## Services

- `document-service`: document workflow API, download grants, and report packaging.
- `parser-service`: document parsing, keyword search, and retrieval helper API.
- `markdown-pdf-service`: Markdown report rendering to PDF, DOCX, and XLSX.
- `web-service`: user-supplied URL fetch and Markdown extraction.

## Runtime Data

The public repository intentionally does not contain real runtime data. Mount these paths locally when testing:

- `runtime/document-db`
- `runtime/processed-md`
- `runtime/file_catalog.json`
- `runtime/users.json`
- `runtime/parser-output`
- `runtime/markdown-output`

## Private Modules Removed

Private review tools, music-generation integrations, account bootstrapping scripts, and production-specific Open WebUI synchronization have been removed from this public snapshot.
