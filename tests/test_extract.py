"""Tests for triage_cli.extract -- pure-function helpers."""
from __future__ import annotations

from datetime import datetime, timedelta, timezone

import pytest

from triage_cli.extract import (
    build_window,
    lookup_site,
    parse_ticket_id,
    resolve_anchor,
)
from triage_cli.models import AnchorSource, SiteEntry, Ticket


# ---------- shared fixtures ----------


def _ticket(
    *,
    subject: str = "",
    description: str = "",
    requester_org: str | None = None,
    created_at: datetime | None = None,
) -> Ticket:
    return Ticket(
        id=1,
        subject=subject,
        description=description,
        requester_org=requester_org,
        tags=[],
        created_at=created_at
        or datetime(2026, 5, 1, 12, 0, 0, tzinfo=timezone.utc),
        comments=[],
    )


@pytest.fixture
def sites() -> list[SiteEntry]:
    return [
        SiteEntry(
            friendly_name="Nevada Department of Public Safety",
            site_name="us-nv-nvdps-apex",
            cnc="de9ee414-da5a-471d-bac2-10643190da0b",
        ),
        SiteEntry(
            friendly_name="Aurora 911, CO",
            site_name="us-co-aurora-apex",
            cnc="921d7c53-e815-4566-9692-6cbce589e1d3",
        ),
        SiteEntry(
            friendly_name="Fairfax Pine Ridge",
            site_name="us-va-fairfax-pine-ridge-apex",
            cnc="00000000-0000-0000-0000-000000000003",
        ),
    ]


# ---------- parse_ticket_id ----------


def test_parse_ticket_id_raw_numeric() -> None:
    assert parse_ticket_id("12345") == 12345


def test_parse_ticket_id_agent_url() -> None:
    assert (
        parse_ticket_id("https://example.zendesk.com/agent/tickets/12345") == 12345
    )


def test_parse_ticket_id_agent_url_trailing_slash() -> None:
    assert (
        parse_ticket_id("https://example.zendesk.com/agent/tickets/12345/")
        == 12345
    )


def test_parse_ticket_id_agent_url_with_query() -> None:
    assert (
        parse_ticket_id("https://example.zendesk.com/agent/tickets/12345?something")
        == 12345
    )


def test_parse_ticket_id_short_url() -> None:
    assert parse_ticket_id("https://example.zendesk.com/tickets/12345") == 12345


def test_parse_ticket_id_empty_raises() -> None:
    with pytest.raises(ValueError):
        parse_ticket_id("")


def test_parse_ticket_id_garbage_raises() -> None:
    with pytest.raises(ValueError):
        parse_ticket_id("abc")


def test_parse_ticket_id_url_without_numeric_tail_raises() -> None:
    with pytest.raises(ValueError):
        parse_ticket_id("https://example.zendesk.com/agent/tickets/")


# ---------- lookup_site ----------


def test_lookup_site_org_match_wins_over_substrings(
    sites: list[SiteEntry],
) -> None:
    # subject mentions Aurora's site_name AND Fairfax's friendly_name,
    # but requester_org exactly matches Nevada -> org wins.
    ticket = _ticket(
        subject="us-co-aurora-apex outage; Fairfax Pine Ridge also paged",
        description="",
        requester_org="Nevada Department of Public Safety",
    )
    entry, strategy = lookup_site(ticket, sites)
    assert strategy == "org_match"
    assert entry is not None and entry.site_name == "us-nv-nvdps-apex"


def test_lookup_site_site_substring_wins_over_friendly_substring(
    sites: list[SiteEntry],
) -> None:
    # subject contains both a site_name (aurora) and a different friendly_name
    # (Fairfax Pine Ridge). site_substring should win because it is checked first.
    ticket = _ticket(
        subject="us-co-aurora-apex error from Fairfax Pine Ridge user",
        description="",
        requester_org=None,
    )
    entry, strategy = lookup_site(ticket, sites)
    assert strategy == "site_substring"
    assert entry is not None and entry.site_name == "us-co-aurora-apex"


def test_lookup_site_friendly_substring_when_no_site_name_match(
    sites: list[SiteEntry],
) -> None:
    ticket = _ticket(
        subject="Issue at Fairfax Pine Ridge",
        description="",
        requester_org=None,
    )
    entry, strategy = lookup_site(ticket, sites)
    assert strategy == "friendly_substring"
    assert entry is not None and entry.friendly_name == "Fairfax Pine Ridge"


def test_lookup_site_no_match_returns_none(sites: list[SiteEntry]) -> None:
    ticket = _ticket(
        subject="Something totally unrelated",
        description="No customer mentioned",
        requester_org="Some Other Org",
    )
    entry, strategy = lookup_site(ticket, sites)
    assert entry is None
    assert strategy == "no_match"


def test_lookup_site_cnc_override_match(sites: list[SiteEntry]) -> None:
    ticket = _ticket()
    entry, strategy = lookup_site(
        ticket,
        sites,
        cnc_override="921d7c53-e815-4566-9692-6cbce589e1d3",
    )
    assert strategy == "cnc_flag"
    assert entry is not None and entry.site_name == "us-co-aurora-apex"


def test_lookup_site_cnc_override_case_insensitive(sites: list[SiteEntry]) -> None:
    ticket = _ticket()
    entry, strategy = lookup_site(
        ticket,
        sites,
        cnc_override="921D7C53-E815-4566-9692-6CBCE589E1D3",
    )
    assert strategy == "cnc_flag"
    assert entry is not None and entry.cnc.lower().startswith("921d7c53")


def test_lookup_site_cnc_override_no_match_raises(sites: list[SiteEntry]) -> None:
    ticket = _ticket()
    with pytest.raises(ValueError, match="not found in site map"):
        lookup_site(ticket, sites, cnc_override="ffffffff-ffff-ffff-ffff-ffffffffffff")


def test_lookup_site_site_override_matching_map(sites: list[SiteEntry]) -> None:
    ticket = _ticket()
    entry, strategy = lookup_site(
        ticket, sites, site_override="us-nv-nvdps-apex"
    )
    assert strategy == "site_flag"
    assert entry is not None
    assert entry.friendly_name == "Nevada Department of Public Safety"


def test_lookup_site_site_override_not_in_map_returns_synthetic(
    sites: list[SiteEntry],
) -> None:
    ticket = _ticket()
    entry, strategy = lookup_site(
        ticket, sites, site_override="us-xx-not-real-apex"
    )
    assert strategy == "site_flag"
    assert entry is not None
    assert entry.friendly_name == "(manual)"
    assert entry.site_name == "us-xx-not-real-apex"
    assert entry.cnc == ""


def test_lookup_site_org_match_case_insensitive(sites: list[SiteEntry]) -> None:
    ticket = _ticket(
        requester_org="NEVADA DEPARTMENT OF PUBLIC SAFETY",
    )
    entry, strategy = lookup_site(ticket, sites)
    assert strategy == "org_match"
    assert entry is not None and entry.site_name == "us-nv-nvdps-apex"


# ---------- build_window ----------


def test_build_window_basic_aware_utc() -> None:
    anchor = datetime(2026, 5, 1, 12, 0, 0, tzinfo=timezone.utc)
    start, end = build_window(anchor, 30)
    assert start == datetime(2026, 5, 1, 11, 30, 0, tzinfo=timezone.utc)
    assert end == datetime(2026, 5, 1, 12, 30, 0, tzinfo=timezone.utc)
    assert start.tzinfo is not None
    assert end.tzinfo is not None


def test_build_window_naive_treated_as_utc() -> None:
    anchor = datetime(2026, 5, 1, 12, 0, 0)  # naive
    start, end = build_window(anchor, 30)
    assert start == datetime(2026, 5, 1, 11, 30, 0, tzinfo=timezone.utc)
    assert end == datetime(2026, 5, 1, 12, 30, 0, tzinfo=timezone.utc)


def test_build_window_non_utc_aware_converted() -> None:
    pacific = timezone(timedelta(hours=-7))
    anchor = datetime(2026, 5, 1, 5, 0, 0, tzinfo=pacific)  # 12:00 UTC
    start, end = build_window(anchor, 15)
    assert start == datetime(2026, 5, 1, 11, 45, 0, tzinfo=timezone.utc)
    assert end == datetime(2026, 5, 1, 12, 15, 0, tzinfo=timezone.utc)


def test_build_window_zero_minutes_raises() -> None:
    anchor = datetime(2026, 5, 1, 12, 0, 0, tzinfo=timezone.utc)
    with pytest.raises(ValueError):
        build_window(anchor, 0)


def test_build_window_negative_minutes_raises() -> None:
    anchor = datetime(2026, 5, 1, 12, 0, 0, tzinfo=timezone.utc)
    with pytest.raises(ValueError):
        build_window(anchor, -5)


# ---------- resolve_anchor ----------


def test_resolve_anchor_flag_wins() -> None:
    flag = datetime(2026, 5, 1, 10, 0, 0, tzinfo=timezone.utc)
    extracted = datetime(2026, 5, 1, 11, 0, 0, tzinfo=timezone.utc)
    created = datetime(2026, 5, 1, 12, 0, 0, tzinfo=timezone.utc)
    ticket = _ticket(created_at=created)
    dt, src = resolve_anchor(ticket, at_flag=flag, extracted=extracted)
    assert dt == flag
    assert src == AnchorSource.FLAG


def test_resolve_anchor_extracted_wins_over_created() -> None:
    extracted = datetime(2026, 5, 1, 11, 0, 0, tzinfo=timezone.utc)
    created = datetime(2026, 5, 1, 12, 0, 0, tzinfo=timezone.utc)
    ticket = _ticket(created_at=created)
    dt, src = resolve_anchor(ticket, at_flag=None, extracted=extracted)
    assert dt == extracted
    assert src == AnchorSource.EXTRACTED


def test_resolve_anchor_falls_back_to_created_at() -> None:
    created = datetime(2026, 5, 1, 12, 0, 0, tzinfo=timezone.utc)
    ticket = _ticket(created_at=created)
    dt, src = resolve_anchor(ticket, at_flag=None, extracted=None)
    assert dt == created
    assert src == AnchorSource.CREATED_AT


def test_resolve_anchor_normalizes_naive_to_utc() -> None:
    naive_flag = datetime(2026, 5, 1, 10, 0, 0)  # naive
    ticket = _ticket()
    dt, src = resolve_anchor(ticket, at_flag=naive_flag, extracted=None)
    assert dt.tzinfo is not None
    assert dt == datetime(2026, 5, 1, 10, 0, 0, tzinfo=timezone.utc)
    assert src == AnchorSource.FLAG
