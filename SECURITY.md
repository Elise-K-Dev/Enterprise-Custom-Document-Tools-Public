# Security Notes

This repository is sanitized for public reporting.

Before using it in a real environment:

- Provide secrets only through environment variables or a secret manager.
- Do not commit `.env` files or Open WebUI runtime databases.
- Keep document corpora and generated outputs outside version control.
- Rotate internal tool tokens regularly.
- Review tool access grants before exposing tools to end users.
