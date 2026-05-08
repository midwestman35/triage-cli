"""Read-only Zendesk client for fetching a ticket and its full comment thread."""
from __future__ import annotations

import logging
import os
from datetime import datetime
from types import TracebackType
from typing import Any

import httpx

from triage_cli.models import AttachmentEvidence, Comment, Ticket

logger = logging.getLogger(__name__)

_USER_AGENT = "triage-cli/0.1"
_PAGE_SIZE = 100
_MAX_PAGES = 1000  # 100k comments at page[size]=100 - far past any real ticket


class ZendeskClient:
    """Read-only Zendesk client for fetching tickets and their comment thread."""

    def __init__(
        self,
        subdomain: str,
        email: str,
        api_token: str,
        timeout: float = 15.0,
    ) -> None:
        """Construct a client bound to a Zendesk subdomain with basic-auth credentials."""
        self._base_url = f"https://{subdomain}.zendesk.com/api/v2"
        # Zendesk basic-auth: username is "{email}/token", password is the api token.
        self._client = httpx.Client(
            auth=(f"{email}/token", api_token),
            timeout=timeout,
            headers={"User-Agent": _USER_AGENT, "Accept": "application/json"},
        )

    @classmethod
    def from_env(cls) -> ZendeskClient:
        """Construct from ZENDESK_SUBDOMAIN, ZENDESK_EMAIL, ZENDESK_API_TOKEN env vars."""
        required = ("ZENDESK_SUBDOMAIN", "ZENDESK_EMAIL", "ZENDESK_API_TOKEN")
        values = {name: os.environ.get(name) for name in required}
        missing = [name for name, value in values.items() if not value]
        if missing:
            raise RuntimeError(
                f"Missing required environment variables: {', '.join(missing)}"
            )
        return cls(
            subdomain=values["ZENDESK_SUBDOMAIN"],  # type: ignore[arg-type]
            email=values["ZENDESK_EMAIL"],  # type: ignore[arg-type]
            api_token=values["ZENDESK_API_TOKEN"],  # type: ignore[arg-type]
        )

    def close(self) -> None:
        """Close the underlying HTTP client."""
        self._client.close()

    def __enter__(self) -> ZendeskClient:
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        tb: TracebackType | None,
    ) -> None:
        self.close()

    def get_ticket(self, ticket_id: int) -> Ticket:
        """Fetch a Zendesk ticket plus its full comment thread and return a Ticket model."""
        payload = self._get(
            f"/tickets/{ticket_id}.json",
            params={"include": "users,organizations"},
            ticket_id=ticket_id,
        )
        ticket_obj = payload.get("ticket") or {}
        users_by_id = {u["id"]: u for u in payload.get("users", []) if "id" in u}
        orgs_by_id = {o["id"]: o for o in payload.get("organizations", []) if "id" in o}

        org_id = ticket_obj.get("organization_id")
        if org_id is None:
            requester = users_by_id.get(ticket_obj.get("requester_id"))
            if requester:
                org_id = requester.get("organization_id")
        org = orgs_by_id.get(org_id) if org_id is not None else None
        requester_org = org.get("name") if org else None

        return Ticket(
            id=int(ticket_obj["id"]),
            subject=ticket_obj.get("subject") or "",
            description=ticket_obj.get("description") or "",
            requester_org=requester_org,
            tags=list(ticket_obj.get("tags") or []),
            created_at=_parse_iso(ticket_obj["created_at"]),
            updated_at=_parse_iso(ticket_obj["updated_at"]),
            comments=self._fetch_comments(ticket_id),
        )

    def list_view_ticket_ids(self, view_id: int) -> list[int]:
        """Return ticket IDs in the given Zendesk view, in the order returned.

        Paginates via cursor (meta.has_more + links.next) with legacy next_page
        fallback. Raises RuntimeError on transport failure or non-2xx status;
        a 404 surfaces a view-flavored message.
        """
        path: str | None = f"/views/{view_id}/tickets.json"
        params: dict[str, Any] | None = {"page[size]": _PAGE_SIZE}
        ids: list[int] = []

        for _ in range(_MAX_PAGES):
            if path is None:
                break
            try:
                payload = self._get(path, params=params, ticket_id=view_id)
            except RuntimeError as e:
                if str(e).startswith(f"Ticket {view_id} not found"):
                    raise RuntimeError(f"View {view_id} not found") from e
                raise
            for t in payload.get("tickets") or []:
                if "id" in t:
                    ids.append(int(t["id"]))

            meta = payload.get("meta") or {}
            links = payload.get("links") or {}
            if meta.get("has_more") and links.get("next"):
                path = links["next"]
            else:
                path = payload.get("next_page")
            params = None
        else:
            raise RuntimeError(
                f"Zendesk view pagination exceeded {_MAX_PAGES} pages - possible loop"
            )
        return ids

    def list_my_ticket_ids(self) -> list[int]:
        """Return IDs of tickets assigned to the authenticated user.

        Fetches the current user via /users/me.json, then pages through
        /users/{id}/tickets/assigned.json. Returns all assignment statuses;
        callers use should_triage to decide what to act on.
        """
        me = self._get("/users/me.json", params=None, ticket_id=0)
        user_id = (me.get("user") or {}).get("id")
        if user_id is None:
            raise RuntimeError("Could not determine current Zendesk user ID from /users/me.json")

        path: str | None = f"/users/{user_id}/tickets/assigned.json"
        params: dict[str, Any] | None = {"page[size]": _PAGE_SIZE}
        ids: list[int] = []

        for _ in range(_MAX_PAGES):
            if path is None:
                break
            payload = self._get(path, params=params, ticket_id=user_id)
            for t in payload.get("tickets") or []:
                if "id" in t:
                    ids.append(int(t["id"]))

            meta = payload.get("meta") or {}
            links = payload.get("links") or {}
            if meta.get("has_more") and links.get("next"):
                path = links["next"]
            else:
                path = payload.get("next_page")
            params = None
        else:
            raise RuntimeError("Zendesk assigned-tickets pagination exceeded limit")

        return ids

    def _fetch_comments(self, ticket_id: int) -> list[Comment]:
        """Page through /comments.json (with sideloaded users) and return Comment models."""
        path: str | None = f"/tickets/{ticket_id}/comments.json"
        params: dict[str, Any] | None = {
            "include": "users",
            "page[size]": _PAGE_SIZE,
            "sort": "created_at",
        }
        users_by_id: dict[int, dict[str, Any]] = {}
        raw: list[dict[str, Any]] = []

        for _ in range(_MAX_PAGES):
            if path is None:
                break
            payload = self._get(path, params=params, ticket_id=ticket_id)
            for u in payload.get("users") or []:
                if "id" in u:
                    users_by_id[u["id"]] = u
            raw.extend(payload.get("comments") or [])

            # Cursor pagination (newer): meta.has_more + links.next. Legacy: next_page.
            meta = payload.get("meta") or {}
            links = payload.get("links") or {}
            if meta.get("has_more") and links.get("next"):
                path = links["next"]
            else:
                path = payload.get("next_page")
            params = None  # the follow-up URL already carries query params
        else:
            raise RuntimeError(
                f"Zendesk comments pagination exceeded {_MAX_PAGES} pages - possible loop"
            )

        comments = [_to_comment(rc, users_by_id) for rc in raw]
        comments.sort(key=lambda c: c.created_at)
        return comments

    def _get(
        self,
        path: str,
        params: dict[str, Any] | None,
        ticket_id: int,
    ) -> dict[str, Any]:
        """Issue a GET (relative or absolute URL) and map non-2xx responses to RuntimeError."""
        url = path if path.startswith("http") else f"{self._base_url}{path}"
        logger.debug("GET %s", url)
        try:
            resp = self._client.get(url, params=params)
        except httpx.HTTPError as e:
            raise RuntimeError(f"Zendesk request failed: {e}") from e

        if resp.is_success:
            try:
                return resp.json()
            except ValueError as e:
                raise RuntimeError(
                    f"Zendesk returned non-JSON response: {e}"
                ) from e

        status = resp.status_code
        if status == 404:
            raise RuntimeError(f"Ticket {ticket_id} not found")
        if status in (401, 403):
            raise RuntimeError(
                "Zendesk auth failed - check ZENDESK_EMAIL and ZENDESK_API_TOKEN"
            )
        if status == 429:
            retry_after = resp.headers.get("Retry-After", "unknown")
            raise RuntimeError(f"Zendesk rate-limited; retry after {retry_after} seconds")
        raise RuntimeError(f"Zendesk error {status}: {(resp.text or '')[:200]}")


def _to_comment(rc: dict[str, Any], users_by_id: dict[int, dict[str, Any]]) -> Comment:
    """Map a raw Zendesk comment dict to a Comment model."""
    author = _resolve_author(rc.get("author_id"), users_by_id)
    body = rc.get("plain_body") or rc.get("body") or ""
    return Comment(
        author=author,
        body=body,
        created_at=_parse_iso(rc["created_at"]),
        is_public=bool(rc.get("public", False)),
        attachments=_attachments_from_raw(rc.get("attachments") or []),
    )


def _attachments_from_raw(raw_attachments: list[dict[str, Any]]) -> list[AttachmentEvidence]:
    """Map Zendesk attachment metadata without preserving downloadable URLs."""
    attachments: list[AttachmentEvidence] = []
    for raw in raw_attachments:
        filename = raw.get("file_name") or raw.get("filename") or raw.get("name")
        if not filename:
            continue
        size = raw.get("size") if raw.get("size") is not None else raw.get("size_bytes")
        attachments.append(
            AttachmentEvidence(
                filename=str(filename),
                content_type=(
                    str(raw["content_type"]) if raw.get("content_type") is not None else None
                ),
                size_bytes=int(size) if size is not None else None,
            )
        )
    return attachments


def _resolve_author(
    author_id: int | None, users_by_id: dict[int, dict[str, Any]]
) -> str:
    """Resolve a Zendesk user id to a display string. Prefer name, then email, then user-{id}."""
    if author_id is None:
        return "user-unknown"
    user = users_by_id.get(author_id)
    if user:
        if user.get("name"):
            return str(user["name"])
        if user.get("email"):
            return str(user["email"])
    return f"user-{author_id}"


def _parse_iso(value: str) -> datetime:
    """Parse an ISO 8601 timestamp from Zendesk (handles trailing Z)."""
    return datetime.fromisoformat(value.replace("Z", "+00:00"))
