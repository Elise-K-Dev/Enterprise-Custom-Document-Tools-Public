#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PROJECT_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
ENV_FILE="$PROJECT_DIR/.env"

if [ -f "$ENV_FILE" ]; then
  set -a
  # shellcheck disable=SC1090
  . "$ENV_FILE"
  set +a
fi

if [ -z "${PORT_PROJECT_INTERNAL_TOKEN:-}" ]; then
  echo "[ERROR] PORT_PROJECT_INTERNAL_TOKEN is required. Run scripts/start_openwebui_with_vllm.sh once or add it to $ENV_FILE."
  exit 1
fi

echo "[INFO] Starting Markdown PDF service on :8003"
cd "$PROJECT_DIR/markdown-pdf-service"
MARKDOWN_PDF_SERVICE_HOST=0.0.0.0 \
MARKDOWN_PDF_SERVICE_PORT=8003 \
MARKDOWN_PDF_PUBLIC_BASE_URL=http://127.0.0.1:8003 \
PORT_PROJECT_INTERNAL_TOKEN="$PORT_PROJECT_INTERNAL_TOKEN" \
uvicorn app.main:app --host 0.0.0.0 --port 8003
