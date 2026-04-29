# Enterprise Custom Document Tools

Open WebUI tool server collection for enterprise document search, document generation, Markdown report rendering, and controlled web page fetching.

This public version is a sanitized portfolio/reporting snapshot. It keeps the service architecture and implementation overview, but removes private customer data, generated documents, local account setup, private tool modules, and environment-specific secrets.

## What Is Included

- `rust-service`: document generation and inventory-style workflow API design.
- `python-service`: document parsing/search API and legacy indexing helpers design.
- `markdown-pdf-service`: Markdown to PDF, DOCX, and XLSX rendering API design.
- `web-service`: URL-to-Markdown fetching API design for user-provided pages.
- `open-webui`: safe example Open WebUI tool import JSON files.
- `docs`: public architecture notes.

## What Is Excluded

- Private review tools.
- Music-generation integrations and account-specific routing.
- Real inventory data, Excel files, processed internal Markdown files, generated DOCX/ZIP/PDF outputs.
- Open WebUI runtime database, local users, passwords, tokens, and deployment secrets.
- Customer-specific document templates.

## Architecture

```text
Open WebUI
  -> document_generation_tools  -> document-service:8001
  -> document_search            -> parser-service:8002
  -> markdown_pdf_tools         -> markdown-pdf-service:8003
  -> web_tools                  -> web-service:8004
```

Each tool server exposes an OpenAPI schema and is intended to be registered in Open WebUI as an OpenAPI tool server.

## Local Configuration

Copy `.env.example` to `.env` and set your own values.

```bash
cp .env.example .env
docker compose up -d --build
```

The public compose file does not include private Open WebUI bootstrap automation. Register the tool JSON files in `open-webui/` manually, or adapt them for your own Open WebUI instance.

## Security Notes

- Do not commit real `.env` files.
- Rotate `PORT_PROJECT_INTERNAL_TOKEN` per environment.
- Mount real document corpora and templates at runtime only.
- Review generated OpenAPI imports before giving tools to end users.
