"""User identity + rank resolution for markdown-pdf-service.

Reuses the same header contract as search-service so that rank assignments
managed in OWUI (via the `개발자` / `관리자` / `팀장` / `사원` groups seeded by
scripts/admin/apply_groups.py) flow through to audit metadata
without duplicating policy.

OWUI forwards these headers on every tool call:
  x-openwebui-user-email   required
  x-openwebui-user-id      required
  x-openwebui-user-name    optional
  x-openwebui-user-groups  optional, comma-separated group names
  x-openwebui-user-rank    optional, explicit rank override (hi_rank|low_rank)

Internal callers must additionally supply the shared
x-port-project-internal-token header.
"""
from __future__ import annotations

import os
import secrets
from dataclasses import dataclass

import logging

from fastapi import HTTPException, Request

_log = logging.getLogger("markdown_pdf.identity")


INTERNAL_TOKEN_HEADER = "x-port-project-internal-token"
OPEN_WEBUI_USER_EMAIL_HEADER = "x-openwebui-user-email"
OPEN_WEBUI_USER_ID_HEADER = "x-openwebui-user-id"
OPEN_WEBUI_USER_NAME_HEADER = "x-openwebui-user-name"
OPEN_WEBUI_USER_GROUPS_HEADER = "x-openwebui-user-groups"
OPEN_WEBUI_USER_RANK_HEADER = "x-openwebui-user-rank"
OPEN_WEBUI_USER_ROLE_HEADER = "x-openwebui-user-role"


@dataclass(frozen=True)
class ResolvedUser:
    user_id: str
    email: str
    name: str
    # Rank kept for audit/logging only — no longer used to gate tool usage.
    # Tool-level rank gating was removed because sensitivity is enforced
    # at the data/search layer; gating output format added complexity
    # without real protection (same data still accessible via search).
    rank: str  # "hi_rank" | "low_rank"
    rank_source: str
    role: str  # OWUI native role: "admin" | "user" | "pending"

    def as_dict(self) -> dict[str, str]:
        return {
            "user_id": self.user_id,
            "email": self.email,
            "name": self.name,
            "rank": self.rank,
            "rank_source": self.rank_source,
            "role": self.role,
        }


def configured_internal_token() -> str:
    token = os.getenv("PORT_PROJECT_INTERNAL_TOKEN", "").strip()
    if not token:
        raise HTTPException(status_code=500, detail="PORT_PROJECT_INTERNAL_TOKEN is not configured")
    return token


def require_internal_request(raw_request: Request) -> None:
    expected = configured_internal_token()
    supplied = (raw_request.headers.get(INTERNAL_TOKEN_HEADER) or "").strip()
    if not secrets.compare_digest(supplied, expected):
        raise HTTPException(status_code=403, detail="invalid internal tool token")


def _normalize_rank(raw: str | None) -> str | None:
    value = (raw or "").strip().lower()
    if value in {"hi_rank", "low_rank"}:
        return value
    return None


def _parse_groups(raw: str | None) -> list[str]:
    if not raw:
        return []
    return [g.strip() for g in raw.split(",") if g.strip()]


def _infer_rank_from_groups(groups: list[str]) -> str | None:
    canonical = {g.strip() for g in groups}
    if canonical & {"개발자", "관리자", "팀장"}:
        return "hi_rank"
    if "사원" in canonical:
        return "low_rank"
    return None


def resolve_user(raw_request: Request) -> ResolvedUser:
    """Validate internal token + extract OWUI identity.

    Rank is captured for audit/logging only — tool usage is NOT gated on
    rank. Admin endpoints gate on OWUI role=admin (see require_admin_role).
    """
    require_internal_request(raw_request)

    headers = raw_request.headers
    email = (headers.get(OPEN_WEBUI_USER_EMAIL_HEADER) or "").strip().lower()
    user_id = (headers.get(OPEN_WEBUI_USER_ID_HEADER) or "").strip()
    name = (headers.get(OPEN_WEBUI_USER_NAME_HEADER) or "").strip()
    role = (headers.get(OPEN_WEBUI_USER_ROLE_HEADER) or "").strip().lower() or "user"

    if not email or not user_id:
        raise HTTPException(
            status_code=401,
            detail="registered Open WebUI account is required",
        )

    raw_rank = headers.get(OPEN_WEBUI_USER_RANK_HEADER)
    raw_groups = headers.get(OPEN_WEBUI_USER_GROUPS_HEADER)
    explicit = _normalize_rank(raw_rank)
    if explicit:
        rank, source = explicit, "explicit_header"
    else:
        groups = _parse_groups(raw_groups)
        inferred = _infer_rank_from_groups(groups)
        if inferred:
            rank, source = inferred, "group_header"
        else:
            rank, source = "low_rank", "default_low"
    resolved = ResolvedUser(user_id, email, name or email, rank, source, role)
    _log.info(
        "resolve_user: email=%s rank=%s source=%s role=%s",
        email, resolved.rank, resolved.rank_source, resolved.role,
    )
    return resolved


def require_admin_role(user: ResolvedUser) -> None:
    """Admin endpoints (policy edits, grant revocation, force sweep) are
    restricted to OWUI's native admin role — not rank. Role is more
    natural for ops operations and is reliably populated by OWUI."""
    if user.role != "admin":
        raise HTTPException(
            status_code=403,
            detail=f"admin operation requires OWUI role=admin (current: {user.role})",
        )
