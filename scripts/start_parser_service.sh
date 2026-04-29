#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="/home/elise/Desktop/2026 Dev/Port-Project"
ENV_FILE="${ROOT_DIR}/.env"

if [[ -f "${ENV_FILE}" ]]; then
  set -a
  # shellcheck disable=SC1090
  source "${ENV_FILE}"
  set +a
fi

if [[ -z "${PORT_PROJECT_INTERNAL_TOKEN:-}" ]]; then
  echo "[ERROR] PORT_PROJECT_INTERNAL_TOKEN is required. Run scripts/start_openwebui_with_vllm.sh once or add it to ${ENV_FILE}."
  exit 1
fi

cd "${ROOT_DIR}/python-service"

if [[ ! -d .venv ]]; then
  python3 -m venv .venv
fi

source .venv/bin/activate
pip install -e .
PARSER_SERVICE_HOST=0.0.0.0 PARSER_SERVICE_PORT=8002 PORT_PROJECT_INTERNAL_TOKEN="${PORT_PROJECT_INTERNAL_TOKEN}" uvicorn app.main:app --host 0.0.0.0 --port 8002
