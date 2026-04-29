#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="/home/elise/Desktop/2026 Dev"
OPEN_WEBUI_DIR="${ROOT_DIR}/open-webui-main"
ENV_FILE="${ROOT_DIR}/Port-Project/open-webui/.env.openwebui"
PROJECT_ENV_FILE="${ROOT_DIR}/Port-Project/.env"
BUILD_DIR="${ROOT_DIR}/Port-Project/.openwebui-build"

HOST_PORT="${HOST_PORT:-3000}"
IMAGE_NAME="${IMAGE_NAME:-open-webui-vllm-local}"
DOCKER_BIN="${DOCKER_BIN:-$(command -v docker || true)}"
COMPOSE_ARGS=(-f docker-compose.yml -f docker-compose.host.yml)

if [[ ! -d "${OPEN_WEBUI_DIR}" ]]; then
  echo "[ERROR] Open WebUI source not found: ${OPEN_WEBUI_DIR}"
  exit 1
fi

if [[ ! -f "${ENV_FILE}" ]]; then
  echo "[ERROR] Open WebUI env file not found: ${ENV_FILE}"
  exit 1
fi

if [[ -z "${DOCKER_BIN}" ]]; then
  echo "[ERROR] docker command not found in PATH"
  echo "[HINT] Run this script from a shell where Docker works, or set DOCKER_BIN=/path/to/docker"
  exit 1
fi

ensure_local_image() {
  local source_image="$1"
  local local_image="$2"

  if ! "${DOCKER_BIN}" image inspect "${source_image}" >/dev/null 2>&1; then
    "${DOCKER_BIN}" pull "${source_image}"
  fi

  "${DOCKER_BIN}" tag "${source_image}" "${local_image}"
}

build_service_image() {
  local context_dir="$1"
  local image_name="$2"
  shift 2

  "${DOCKER_BIN}" build \
    --network=host \
    --pull=false \
    "$@" \
    -t "${image_name}" \
    "${context_dir}"
}

prepare_output_dir() {
  local output_dir="$1"

  mkdir -p "${output_dir}"
  if [[ ! -w "${output_dir}" ]]; then
    local backup_dir
    backup_dir="${output_dir}.bak.$(date +%Y%m%d%H%M%S)"
    mv "${output_dir}" "${backup_dir}"
    mkdir -p "${output_dir}"
    echo "[INFO] Output directory was not writable; moved it to ${backup_dir}"
  fi
  chmod 0775 "${output_dir}" 2>/dev/null || true
}

repair_bind_mount_permissions() {
  local target_dir="$1"

  mkdir -p "${target_dir}"
  "${DOCKER_BIN}" run --rm \
    -v "${target_dir}:/target" \
    "port-project-base-debian:bookworm-slim" \
    sh -lc 'chown -R 1000:1000 /target && chmod -R u+rwX,g+rwX /target' \
    >/dev/null
}

detect_public_host() {
  python3 - <<'PY'
import socket

sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
try:
    try:
        sock.connect(("1.1.1.1", 80))
        print(sock.getsockname()[0])
    except OSError:
        print("127.0.0.1")
finally:
    sock.close()
PY
}

if [[ ! -f "${PROJECT_ENV_FILE}" ]] || ! grep -q '^PORT_PROJECT_INTERNAL_TOKEN=' "${PROJECT_ENV_FILE}"; then
  mkdir -p "$(dirname "${PROJECT_ENV_FILE}")"
  TOKEN="$(python3 - <<'PY'
import secrets
print(secrets.token_urlsafe(32))
PY
)"
  {
    echo ""
    echo "PORT_PROJECT_INTERNAL_TOKEN=${TOKEN}"
    echo "PORT_PROJECT_DOWNLOAD_TOKEN_TTL_SECONDS=3600"
  } >> "${PROJECT_ENV_FILE}"
  echo "[INFO] Created PORT_PROJECT_INTERNAL_TOKEN in ${PROJECT_ENV_FILE}"
fi

set -a
# shellcheck disable=SC1090
source "${PROJECT_ENV_FILE}"
set +a

PORT_PROJECT_PUBLIC_HOST="${PORT_PROJECT_PUBLIC_HOST:-$(detect_public_host)}"
export DOCUMENT_SERVICE_PUBLIC_BASE_URL="http://${PORT_PROJECT_PUBLIC_HOST}:8001"
export PARSER_PDF_PUBLIC_BASE_URL="http://${PORT_PROJECT_PUBLIC_HOST}:8002"
export MARKDOWN_PDF_PUBLIC_BASE_URL="http://${PORT_PROJECT_PUBLIC_HOST}:8003"

echo "[INFO] Tool server URLs remain local to Open WebUI containers/host networking"
echo "[INFO] Download public host: ${PORT_PROJECT_PUBLIC_HOST}"
echo "[INFO] Document downloads: ${DOCUMENT_SERVICE_PUBLIC_BASE_URL}"
echo "[INFO] Parser PDF downloads: ${PARSER_PDF_PUBLIC_BASE_URL}"
echo "[INFO] Markdown renderer downloads: ${MARKDOWN_PDF_PUBLIC_BASE_URL}"

echo "[INFO] Preparing local Docker base image aliases..."
ensure_local_image "python:3.11-slim" "port-project-base-python:3.11-slim"
ensure_local_image "rust:1.95" "port-project-base-rust:1.95"
ensure_local_image "debian:bookworm-slim" "port-project-base-debian:bookworm-slim"

echo "[INFO] Preparing writable output directories..."
prepare_output_dir "${ROOT_DIR}/Port-Project/python-service/output"
prepare_output_dir "${ROOT_DIR}/Port-Project/markdown-pdf-service/output"
repair_bind_mount_permissions "${ROOT_DIR}/Port-Project/rust-service/DB/output"
repair_bind_mount_permissions "${ROOT_DIR}/Port-Project/python-service/output"
repair_bind_mount_permissions "${ROOT_DIR}/Port-Project/markdown-pdf-service/output"

cd "${OPEN_WEBUI_DIR}"

rm -rf "${BUILD_DIR}"
mkdir -p "${BUILD_DIR}"
cp -r . "${BUILD_DIR}/"
python3 - <<'PY'
from pathlib import Path

build_dir = Path("/home/elise/Desktop/2026 Dev/Port-Project/.openwebui-build")
dockerfile = build_dir / "Dockerfile"
text = dockerfile.read_text(encoding="utf-8")
text = text.replace(
    "FROM --platform=$BUILDPLATFORM node:22-alpine3.20 AS build",
    "FROM node:22-alpine3.20 AS build",
    1,
)
text = text.replace(
    "RUN apk add --no-cache git",
    (
        "RUN for i in 1 2 3 4 5; do "
        "apk add --no-cache git && break; "
        "echo \"apk add git failed, retrying in $((i * 3))s...\"; "
        "sleep $((i * 3)); "
        "done"
    ),
    1,
)
dockerfile.write_text(text, encoding="utf-8")

dockerignore = build_dir / ".dockerignore"
existing = dockerignore.read_text(encoding="utf-8") if dockerignore.exists() else ""
extra_ignores = [
    ".git",
    "node_modules",
    "**/node_modules",
    "backend/data",
    "backend/data/**",
    "**/.cache",
    "**/.cache/**",
    ".svelte-kit",
    ".svelte-kit/**",
]
with dockerignore.open("a", encoding="utf-8") as fh:
    for pattern in extra_ignores:
        if pattern not in existing.splitlines():
            fh.write(f"\n{pattern}")
PY

cd "${BUILD_DIR}"
"${DOCKER_BIN}" build --network=host -t "${IMAGE_NAME}" .

echo "[INFO] Starting Port-Project services..."
cd "${ROOT_DIR}/Port-Project"
echo "[INFO] Building Port-Project service images..."
build_service_image \
  "${ROOT_DIR}/Port-Project/rust-service" \
  "port-project-document-service" \
  --build-arg RUST_BASE_IMAGE=port-project-base-rust:1.95 \
  --build-arg DEBIAN_BASE_IMAGE=port-project-base-debian:bookworm-slim
build_service_image \
  "${ROOT_DIR}/Port-Project/python-service" \
  "port-project-parser-service" \
  --build-arg PYTHON_BASE_IMAGE=port-project-base-python:3.11-slim
build_service_image \
  "${ROOT_DIR}/Port-Project/markdown-pdf-service" \
  "port-project-markdown-pdf-service" \
  --build-arg PYTHON_BASE_IMAGE=port-project-base-python:3.11-slim
build_service_image \
  "${ROOT_DIR}/Port-Project/web-service" \
  "port-project-web-service" \
  --build-arg PYTHON_BASE_IMAGE=port-project-base-python:3.11-slim

OPEN_WEBUI_IMAGE="${IMAGE_NAME}" "${DOCKER_BIN}" compose "${COMPOSE_ARGS[@]}" up -d --no-build

echo "[INFO] Waiting for Open WebUI health..."
for _ in $(seq 1 90); do
  if curl -fsS "http://127.0.0.1:${HOST_PORT}/health" >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

echo "[INFO] Syncing Open WebUI runtime state..."
RUST_TOOL_SERVER_URL="${RUST_TOOL_SERVER_URL:-http://127.0.0.1:8001}" \
PARSER_TOOL_SERVER_URL="${PARSER_TOOL_SERVER_URL:-http://127.0.0.1:8002}" \
WEB_TOOL_SERVER_URL="${WEB_TOOL_SERVER_URL:-http://127.0.0.1:8004}" \
"${ROOT_DIR}/Port-Project/scripts/sync_openwebui_runtime.sh"

echo "[INFO] Open WebUI started: http://127.0.0.1:${HOST_PORT}"
echo "[INFO] vLLM backend: ${DOCUMENT_FILLER_API_URL:-http://host.docker.internal:8000/v1}"
