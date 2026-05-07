"""Read-only Datadog Logs client for the triage window query."""
from __future__ import annotations

import logging
import os
import re
from datetime import UTC, datetime
from types import TracebackType
from typing import Any

from datadog_api_client import ApiClient, Configuration
from datadog_api_client.exceptions import ApiException
from datadog_api_client.v2.api.logs_api import LogsApi
from datadog_api_client.v2.model.logs_sort import LogsSort

from triage_cli.models import LogLine

logger = logging.getLogger(__name__)

_VALID_LEVELS = {"error", "warn", "info", "debug"}
_SAFE_SITE_RE = re.compile(r"^[a-zA-Z0-9._-]+$")


class DatadogClient:
    """Read-only Datadog Logs client."""

    def __init__(
        self,
        api_key: str,
        app_key: str,
        site: str = "datadoghq.com",
        call_center_tag: str = "@log.machineData.callCenterName",
        max_lines: int = 200,
    ) -> None:
        """Construct a Datadog v2 LogsApi client bound to a site and call-center tag key."""
        config = Configuration()
        config.api_key["apiKeyAuth"] = api_key
        config.api_key["appKeyAuth"] = app_key
        config.server_variables["site"] = site
        self._api_client = ApiClient(config)
        self._logs_api = LogsApi(self._api_client)
        self._call_center_tag = call_center_tag
        self._max_lines = max_lines

    @classmethod
    def from_env(cls) -> DatadogClient:
        """Construct from DD_API_KEY, DD_APP_KEY, DD_SITE, DD_CALL_CENTER_TAG env vars."""
        api_key = os.environ.get("DD_API_KEY")
        app_key = os.environ.get("DD_APP_KEY")
        missing = [n for n, v in (("DD_API_KEY", api_key), ("DD_APP_KEY", app_key)) if not v]
        if missing:
            raise RuntimeError(f"Missing required environment variables: {', '.join(missing)}")
        return cls(
            api_key=api_key,  # type: ignore[arg-type]
            app_key=app_key,  # type: ignore[arg-type]
            site=os.environ.get("DD_SITE") or "datadoghq.com",
            call_center_tag=(
                os.environ.get("DD_CALL_CENTER_TAG") or "@log.machineData.callCenterName"
            ),
        )

    def close(self) -> None:
        """Close the underlying SDK ApiClient."""
        self._api_client.close()

    def __enter__(self) -> DatadogClient:
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        tb: TracebackType | None,
    ) -> None:
        self.close()

    def get_logs(
        self,
        site_name: str,
        levels: list[str],
        start: datetime,
        end: datetime,
    ) -> tuple[list[LogLine], bool]:
        """Fetch logs in the window. Returns (chronological logs, truncated_bool)."""
        if not levels:
            raise ValueError("levels list cannot be empty")
        clean_site = (site_name or "").strip()
        if not clean_site:
            raise ValueError("site_name cannot be empty")
        if not _SAFE_SITE_RE.match(clean_site):
            raise ValueError(
                f"site_name {clean_site!r} contains characters that are unsafe for "
                "Datadog query interpolation; expected only [a-zA-Z0-9._-]"
            )
        if start >= end:
            raise ValueError("start must be strictly before end")
        norm_levels = [lvl.strip().lower() for lvl in levels]
        invalid = [lvl for lvl in norm_levels if lvl not in _VALID_LEVELS]
        if invalid:
            raise ValueError(f"Invalid log levels: {invalid}. Valid: {sorted(_VALID_LEVELS)}")

        query = f"{self._call_center_tag}:{clean_site} status:({' OR '.join(norm_levels)})"
        logger.debug("datadog query=%s from=%s to=%s", query, start, end)

        try:
            resp = self._logs_api.list_logs_get(
                filter_query=query,
                filter_from=_ensure_aware(start),
                filter_to=_ensure_aware(end),
                sort=LogsSort.TIMESTAMP_ASCENDING,
                page_limit=self._max_lines,
            )
        except ApiException as e:
            if e.status in (401, 403):
                raise RuntimeError(
                    "Datadog auth failed — check DD_API_KEY and DD_APP_KEY"
                ) from e
            body = getattr(e, "body", None) or ""
            body_str = body.decode(errors="replace") if isinstance(body, bytes) else str(body)
            raise RuntimeError(f"Datadog API error {e.status}: {body_str[:200]}") from e
        except Exception as e:  # pragma: no cover - urllib3/transport fallback
            raise RuntimeError(f"Datadog request failed: {e}") from e

        raw = list(getattr(resp, "data", None) or [])
        logs = [_to_log_line(item) for item in raw]
        logs.sort(key=lambda log: log.timestamp)
        return logs, len(logs) >= self._max_lines


def _ensure_aware(dt: datetime) -> datetime:
    """Return a timezone-aware datetime; treat naive inputs as UTC."""
    return dt if dt.tzinfo is not None else dt.replace(tzinfo=UTC)


def _get(obj: Any, key: str) -> Any:
    """Read key from a dict or attribute from an SDK object."""
    if obj is None:
        return None
    return obj.get(key) if isinstance(obj, dict) else getattr(obj, key, None)


def _coerce_dict(obj: Any) -> dict[str, Any]:
    """Best-effort conversion of an SDK model object (or dict) to a plain dict."""
    if obj is None:
        return {}
    if isinstance(obj, dict):
        return dict(obj)
    if hasattr(obj, "to_dict"):
        try:
            result = obj.to_dict()
            if isinstance(result, dict):
                return result
        except Exception:  # pragma: no cover - defensive
            pass
    if hasattr(obj, "__dict__"):
        return {k: v for k, v in vars(obj).items() if not k.startswith("_")}
    return {}


def _to_log_line(item: Any) -> LogLine:
    """Map a Datadog v2 Log SDK object to a LogLine, tolerating nested attribute shape."""
    outer = getattr(item, "attributes", None)
    inner = getattr(outer, "attributes", None) if outer is not None else None

    ts = getattr(outer, "timestamp", None) or _get(inner, "timestamp")
    if isinstance(ts, str):
        ts = datetime.fromisoformat(ts.replace("Z", "+00:00"))
    if ts is None:
        ts = datetime.now(UTC)
    elif isinstance(ts, datetime) and ts.tzinfo is None:
        ts = ts.replace(tzinfo=UTC)

    level = _get(inner, "status") or getattr(outer, "status", None) or "info"
    message = getattr(outer, "message", None)
    if message is None:
        message = _get(inner, "message")

    return LogLine(
        timestamp=ts,
        level=str(level).lower(),
        message=str(message) if message is not None else "",
        attributes=_coerce_dict(inner),
    )
