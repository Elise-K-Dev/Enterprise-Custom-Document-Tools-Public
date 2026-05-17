"""SQLite-backed download grant store with per-artifact policy.

Replaces the previous in-memory _DOWNLOAD_GRANTS dict so that:
  - tokens survive service restarts,
  - retention policy can be hot-swapped per artifact via admin endpoints,
  - a background worker can sweep expired artifacts off disk on a poll
    independent of any request traffic.

The index DB lives under MARKDOWN_PDF_OUTPUT_DIR so it shares the same
host volume as the artifacts it tracks.
"""
from __future__ import annotations

import asyncio
import logging
import os
import secrets
import sqlite3
import threading
import time
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterator

import yaml


log = logging.getLogger("markdown_pdf.grants")


@dataclass
class RetentionPolicy:
    default_ttl_seconds: int = 86400
    default_max_downloads: int | None = None
    delete_on_download: bool = False
    cleanup_interval_seconds: int = 60
    delete_grace_seconds: int = 30

    @classmethod
    def from_file(cls, path: Path) -> "RetentionPolicy":
        if not path.is_file():
            log.warning("retention config %s missing — using defaults", path)
            return cls()
        try:
            data = yaml.safe_load(path.read_text(encoding="utf-8")) or {}
        except Exception as exc:
            log.error("failed to parse %s: %s — using defaults", path, exc)
            return cls()
        merged = cls().__dict__ | {k: v for k, v in data.items() if k in cls().__dict__}
        return cls(**merged)  # type: ignore[arg-type]

    def to_dict(self) -> dict[str, Any]:
        return self.__dict__.copy()


SCHEMA = """
CREATE TABLE IF NOT EXISTS grants (
  token TEXT PRIMARY KEY,
  path TEXT NOT NULL,
  user_id TEXT NOT NULL,
  email TEXT NOT NULL,
  rank TEXT NOT NULL,
  created_at REAL NOT NULL,
  expires_at REAL NOT NULL,
  max_downloads INTEGER,
  downloads INTEGER NOT NULL DEFAULT 0,
  delete_on_download INTEGER NOT NULL DEFAULT 0,
  status TEXT NOT NULL DEFAULT 'active'
);
CREATE INDEX IF NOT EXISTS idx_grants_expires_at ON grants(expires_at);
CREATE INDEX IF NOT EXISTS idx_grants_path ON grants(path);

CREATE TABLE IF NOT EXISTS policy_overrides (
  key TEXT PRIMARY KEY,
  value TEXT
);
"""


class GrantStore:
    def __init__(self, db_path: Path, output_dir: Path, config_path: Path):
        self.db_path = db_path
        self.output_dir = output_dir
        self.config_path = config_path
        self._lock = threading.Lock()
        self._policy_cache: tuple[float, RetentionPolicy] | None = None
        db_path.parent.mkdir(parents=True, exist_ok=True)
        with self._connect() as conn:
            conn.executescript(SCHEMA)

    @contextmanager
    def _connect(self) -> Iterator[sqlite3.Connection]:
        conn = sqlite3.connect(self.db_path, isolation_level=None, timeout=10.0)
        try:
            conn.execute("PRAGMA journal_mode=WAL")
            conn.execute("PRAGMA synchronous=NORMAL")
            conn.row_factory = sqlite3.Row
            yield conn
        finally:
            conn.close()

    def policy(self) -> RetentionPolicy:
        """File-based defaults overlaid by DB policy_overrides.

        Reads config_path on each call but caches the parse for ~2s to
        avoid hammering disk when called inside a tight loop.
        """
        now = time.time()
        if self._policy_cache and now - self._policy_cache[0] < 2.0:
            return self._policy_cache[1]
        base = RetentionPolicy.from_file(self.config_path)
        with self._connect() as conn:
            rows = conn.execute("SELECT key, value FROM policy_overrides").fetchall()
        overrides: dict[str, Any] = {}
        for row in rows:
            key, raw = row["key"], row["value"]
            if key not in base.__dict__:
                continue
            current = getattr(base, key)
            try:
                if isinstance(current, bool):
                    overrides[key] = raw.lower() in {"1", "true", "yes", "on"}
                elif isinstance(current, int) or current is None and key.endswith("_seconds"):
                    overrides[key] = int(raw) if raw not in {"", "null", "None"} else None
                else:
                    overrides[key] = raw
            except Exception:
                log.warning("ignoring malformed override %s=%s", key, raw)
        if overrides:
            base = RetentionPolicy(**(base.__dict__ | overrides))
        self._policy_cache = (now, base)
        return base

    def set_policy_override(self, key: str, value: Any) -> None:
        if key not in RetentionPolicy().__dict__:
            raise ValueError(f"unknown policy key: {key}")
        with self._connect() as conn:
            if value is None:
                conn.execute("DELETE FROM policy_overrides WHERE key=?", (key,))
            else:
                conn.execute(
                    "INSERT INTO policy_overrides(key,value) VALUES(?,?) "
                    "ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                    (key, str(value)),
                )
        self._policy_cache = None

    def issue(
        self,
        *,
        path: str,
        user_id: str,
        email: str,
        rank: str,
        ttl_seconds: int | None = None,
        max_downloads: int | None = None,
        delete_on_download: bool | None = None,
    ) -> dict[str, Any]:
        policy = self.policy()
        ttl = ttl_seconds if ttl_seconds is not None else policy.default_ttl_seconds
        max_dl = max_downloads if max_downloads is not None else policy.default_max_downloads
        dod = delete_on_download if delete_on_download is not None else policy.delete_on_download
        now = time.time()
        token = secrets.token_urlsafe(32)
        with self._connect() as conn:
            conn.execute(
                "INSERT INTO grants(token,path,user_id,email,rank,created_at,expires_at,"
                "max_downloads,downloads,delete_on_download,status) "
                "VALUES(?,?,?,?,?,?,?,?,?,?, 'active')",
                (token, path, user_id, email, rank, now, now + ttl, max_dl, 0, 1 if dod else 0),
            )
        return self.get(token) or {}

    def get(self, token: str) -> dict[str, Any] | None:
        with self._connect() as conn:
            row = conn.execute("SELECT * FROM grants WHERE token=?", (token,)).fetchone()
        return dict(row) if row else None

    def validate_and_consume(self, token: str, path: str) -> dict[str, Any]:
        """Atomic check + download counter increment. Raises if invalid."""
        now = time.time()
        with self._lock, self._connect() as conn:
            row = conn.execute(
                "SELECT * FROM grants WHERE token=? AND path=?", (token, path)
            ).fetchone()
            if not row:
                raise PermissionError("invalid download token")
            if row["status"] != "active":
                raise PermissionError(f"grant is {row['status']}")
            if row["expires_at"] <= now:
                raise PermissionError("grant expired")
            new_count = row["downloads"] + 1
            new_status = row["status"]
            if row["max_downloads"] is not None and new_count >= row["max_downloads"]:
                new_status = "exhausted"
            conn.execute(
                "UPDATE grants SET downloads=?, status=? WHERE token=?",
                (new_count, new_status, token),
            )
            updated = conn.execute("SELECT * FROM grants WHERE token=?", (token,)).fetchone()
        return dict(updated)

    def mark_for_deletion(self, token: str) -> None:
        with self._connect() as conn:
            conn.execute(
                "UPDATE grants SET status='revoked', expires_at=? WHERE token=?",
                (time.time(), token),
            )

    def update_grant(
        self,
        token: str,
        *,
        ttl_extend_seconds: int | None = None,
        expires_at: float | None = None,
        max_downloads: int | None = None,
        delete_on_download: bool | None = None,
        status: str | None = None,
    ) -> dict[str, Any]:
        sets: list[str] = []
        args: list[Any] = []
        if expires_at is not None:
            sets.append("expires_at=?")
            args.append(expires_at)
        elif ttl_extend_seconds is not None:
            row = self.get(token)
            if not row:
                raise KeyError(token)
            sets.append("expires_at=?")
            args.append(float(row["expires_at"]) + ttl_extend_seconds)
        if max_downloads is not None:
            sets.append("max_downloads=?")
            args.append(max_downloads if max_downloads >= 0 else None)
        if delete_on_download is not None:
            sets.append("delete_on_download=?")
            args.append(1 if delete_on_download else 0)
        if status is not None:
            if status not in {"active", "revoked", "exhausted", "expired"}:
                raise ValueError(f"invalid status: {status}")
            sets.append("status=?")
            args.append(status)
        if not sets:
            return self.get(token) or {}
        args.append(token)
        with self._connect() as conn:
            conn.execute(f"UPDATE grants SET {', '.join(sets)} WHERE token=?", args)
        return self.get(token) or {}

    def list_grants(
        self,
        *,
        status: str | None = None,
        user_id: str | None = None,
        limit: int = 200,
    ) -> list[dict[str, Any]]:
        where: list[str] = []
        args: list[Any] = []
        if status:
            where.append("status=?")
            args.append(status)
        if user_id:
            where.append("user_id=?")
            args.append(user_id)
        clause = f" WHERE {' AND '.join(where)}" if where else ""
        with self._connect() as conn:
            rows = conn.execute(
                f"SELECT * FROM grants{clause} ORDER BY created_at DESC LIMIT ?",
                (*args, limit),
            ).fetchall()
        return [dict(r) for r in rows]

    def sweep(self) -> dict[str, int]:
        """One sweep pass: expire overdue grants and unlink stale files.

        Returns counts {expired, deleted_files, kept_files_grace}.
        """
        policy = self.policy()
        now = time.time()
        result = {"expired": 0, "deleted_files": 0, "kept_files_grace": 0}

        with self._lock, self._connect() as conn:
            cur = conn.execute(
                "UPDATE grants SET status='expired' WHERE status='active' AND expires_at <= ?",
                (now,),
            )
            result["expired"] = cur.rowcount

            # Identify files to consider unlinking: any path whose every grant
            # is non-active (expired/revoked/exhausted) and past grace window.
            cutoff = now - max(0, policy.delete_grace_seconds)
            candidates = conn.execute(
                "SELECT path, MAX(expires_at) AS last_expiry "
                "FROM grants GROUP BY path "
                "HAVING SUM(status='active') = 0 AND MAX(expires_at) <= ?",
                (cutoff,),
            ).fetchall()

            # Also include delete_on_download files whose downloads >= 1 and dod=1.
            dod_rows = conn.execute(
                "SELECT path FROM grants "
                "WHERE delete_on_download=1 AND downloads >= 1 AND status != 'expired'"
            ).fetchall()

        for r in candidates:
            path = self.output_dir / r["path"]
            if self._safe_unlink(path):
                result["deleted_files"] += 1
            else:
                result["kept_files_grace"] += 1

        for r in dod_rows:
            path = self.output_dir / r["path"]
            if self._safe_unlink(path):
                result["deleted_files"] += 1
                with self._connect() as conn:
                    conn.execute(
                        "UPDATE grants SET status='expired', expires_at=? WHERE path=?",
                        (now, r["path"]),
                    )

        return result

    def _safe_unlink(self, file_path: Path) -> bool:
        try:
            canonical_root = self.output_dir.resolve()
            target = file_path.resolve()
        except FileNotFoundError:
            return False
        if not target.is_relative_to(canonical_root):
            log.warning("refusing to unlink outside output dir: %s", file_path)
            return False
        if not target.exists():
            return False
        try:
            target.unlink()
            log.info("unlinked stale artifact: %s", target.relative_to(canonical_root))
            return True
        except OSError as exc:
            log.error("failed to unlink %s: %s", target, exc)
            return False


async def run_cleanup_worker(store: GrantStore, stop_event: asyncio.Event) -> None:
    log.info("cleanup worker started")
    while not stop_event.is_set():
        interval = max(5, store.policy().cleanup_interval_seconds)
        try:
            result = await asyncio.to_thread(store.sweep)
            if any(v for v in result.values()):
                log.info("sweep: %s", result)
        except Exception:
            log.exception("sweep failed")
        try:
            await asyncio.wait_for(stop_event.wait(), timeout=interval)
        except asyncio.TimeoutError:
            pass
    log.info("cleanup worker stopped")
