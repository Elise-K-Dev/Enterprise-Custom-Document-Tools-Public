#!/usr/bin/env bash
# CDXVI overlay installer — native Open WebUI (systemd) frontend.
# Copies runtime/owui-overlay.{js,css} into the OWUI build dir and patches
# index.html to load them. Idempotent; safe to re-run.
#
# Usage:
#   bash runtime/install_overlay.sh
#
# Overrides:
#   OWUI_BUILD_DIR   target build dir (default: $FRONTEND_BUILD_DIR from
#                    the systemd unit, falls back to /opt/open-webui/build)
set -euo pipefail
RUNTIME_DIR="$(cd "$(dirname "$0")" && pwd)"
DEFAULT_BUILD_DIR="/opt/open-webui/build"
BUILD_DIR="${OWUI_BUILD_DIR:-${FRONTEND_BUILD_DIR:-$DEFAULT_BUILD_DIR}}"

if [ ! -f "${RUNTIME_DIR}/owui-overlay.js" ] || [ ! -f "${RUNTIME_DIR}/owui-overlay.css" ]; then
  echo "[ERR] overlay 자산이 없습니다: ${RUNTIME_DIR}" >&2
  exit 1
fi
if [ ! -d "${BUILD_DIR}" ]; then
  echo "[ERR] OWUI build dir 없음: ${BUILD_DIR}" >&2
  exit 1
fi

echo "[*] overlay 자산 복사 -> ${BUILD_DIR}"
cp "${RUNTIME_DIR}/owui-overlay.js"  "${BUILD_DIR}/cdxvi-overlay.js"
cp "${RUNTIME_DIR}/owui-overlay.css" "${BUILD_DIR}/cdxvi-overlay.css"

INDEX="${BUILD_DIR}/index.html"
if grep -q "cdxvi-overlay.js" "${INDEX}"; then
  echo "[=] index.html 이미 overlay 포함 (cache-busting v= 만 갱신)"
  STAMP=$(date +%s)
  sed -i -E \
    -e "s|/cdxvi-overlay\\.css\\?v=[0-9]+|/cdxvi-overlay.css?v=${STAMP}|" \
    -e "s|/cdxvi-overlay\\.js\\?v=[0-9]+|/cdxvi-overlay.js?v=${STAMP}|" \
    "${INDEX}"
else
  STAMP=$(date +%s)
  sed -i "s#</head>#<link rel=\"stylesheet\" href=\"/cdxvi-overlay.css?v=${STAMP}\">\n<script defer src=\"/cdxvi-overlay.js?v=${STAMP}\"></script>\n</head>#" "${INDEX}"
  echo "[+] index.html 에 overlay 링크 추가"
fi

echo "[OK] 설치 완료. 브라우저 강력 새로고침 (Ctrl+Shift+R) 필요."
