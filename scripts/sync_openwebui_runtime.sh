#!/usr/bin/env bash
set -euo pipefail

cat <<'MSG'
This public snapshot does not include the private Open WebUI account bootstrapper.

Import the sanitized tool definitions manually from:
  - open-webui/openwebui-rust-tools.json
  - open-webui/openwebui-python-tools.json
  - open-webui/openwebui-markdown-pdf-tools.json
  - open-webui/openwebui-web-tools.json

Runtime secrets, user passwords, access grants, and private tool routing should be
configured in your own Open WebUI deployment.
MSG
