# Document Service

Rust-based OpenAPI tool server for document workflow orchestration.

The private repository contains the full production implementation. This public report version documents the service boundary and excludes customer-specific templates, inventory snapshots, generated files, and operational data.

## Responsibilities

- Create and fill structured document sessions.
- Export generated documents.
- Package report outputs.
- Issue short-lived download grants.
- Call the Markdown renderer service for report exports.
