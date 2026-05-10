"""Tests for triage_cli.redact (PII redactor at the LLM boundary)."""
from __future__ import annotations

from triage_cli.redact import RedactionCounts, redact


def test_redacts_simple_phone() -> None:
    out, counts = redact("Call 555-123-4567 for status.")
    assert out == "Call <PHONE> for status."
    assert counts.phones == 1


def test_redacts_phone_with_parens_and_country_code() -> None:
    out, counts = redact("Reach me at +1 (555) 123-4567 today.")
    assert "<PHONE>" in out
    assert counts.phones == 1


def test_redacts_phone_with_dots() -> None:
    out, counts = redact("Number: 555.123.4567 confirmed.")
    assert "<PHONE>" in out
    assert counts.phones == 1


def test_redacts_bare_ten_digits() -> None:
    out, counts = redact("Phone 5551234567 logged.")
    assert "<PHONE>" in out
    assert counts.phones == 1


def test_redacts_multiple_phones() -> None:
    _out, counts = redact("Try 555-111-2222 or 555-333-4444.")
    assert counts.phones == 2


def test_does_not_redact_inside_alphanumeric_id() -> None:
    out, counts = redact("Call-ID: abc5551234567xyz@host")
    assert "<PHONE>" not in out
    assert counts.phones == 0


def test_does_not_redact_short_number_sequences() -> None:
    out, counts = redact("Status code 200 with 5 retries.")
    assert "<PHONE>" not in out
    assert counts.phones == 0


def test_counts_default_to_zero() -> None:
    out, counts = redact("No PII here at all.")
    assert out == "No PII here at all."
    assert counts.phones == 0
    assert counts.addresses == 0
    assert counts.coords == 0
    assert counts.enabled is True


def test_bare_numeric_call_id_is_redacted_known_gap() -> None:
    """Bare numeric Call-IDs that look like 10-digit phones are intentionally
    redacted in v1. Spec 2026-05-10-final-phase-design.md, Q1 default
    (permissive phone matching). Documented false positive."""
    out, counts = redact("Call-ID: 5551234567 initiated.")
    assert "<PHONE>" in out
    assert counts.phones == 1


# ---------------------------------------------------------------------------
# Address redaction tests (Task 1.2)
# ---------------------------------------------------------------------------


def test_redacts_simple_address() -> None:
    out, counts = redact("Caller located at 123 Main St reported incident.")
    assert "<ADDR>" in out
    assert counts.addresses == 1


def test_redacts_with_avenue_and_other_suffixes() -> None:
    """Real-world addresses always have a street name before the suffix.
    Bare ``<number> <suffix>`` shapes (e.g., "55 Highway", "9 Circle")
    are not real addresses in 911 dispatch and are not handled by design.
    """
    for s in ("789 Oak Ave", "12 First Boulevard", "1 Court Place"):
        out, counts = redact(f"Caller at {s} confirmed.")
        assert "<ADDR>" in out, f"failed for {s!r}"
        assert counts.addresses == 1


def test_address_redaction_preserves_city() -> None:
    out, counts = redact("Dispatch to 456 Elm Street, Springfield for backup.")
    assert "<ADDR>" in out
    assert "Springfield" in out
    assert counts.addresses == 1


def test_does_not_redact_bare_number_without_street() -> None:
    out, counts = redact("Ticket #1234 opened at 09:00.")
    assert "<ADDR>" not in out
    assert counts.addresses == 0


def test_redacts_address_and_phone_together() -> None:
    out, counts = redact("Caller at 321 Pine Rd called from 555-867-5309.")
    assert "<ADDR>" in out
    assert "<PHONE>" in out
    assert counts.addresses == 1
    assert counts.phones == 1


def test_redacts_decimal_coords_comma() -> None:
    out, counts = redact("Caller at 33.7490, -84.3880 reported.")
    assert "<COORDS>" in out
    assert counts.coords == 1


def test_redacts_decimal_coords_space() -> None:
    out, counts = redact("Coords: 40.7128 -74.0060 confirmed.")
    assert "<COORDS>" in out
    assert counts.coords == 1


def test_does_not_redact_low_precision_pairs() -> None:
    # Version numbers, prices, etc. — require 4+ decimals.
    out, counts = redact("Version 1.23, build 4.56 deployed.")
    assert "<COORDS>" not in out
    assert counts.coords == 0


def test_redacts_multiple_coord_pairs() -> None:
    _out, counts = redact("Pings: 33.7490, -84.3880 then 40.7128, -74.0060.")
    assert counts.coords == 2


def test_pass_through_already_starred_phone() -> None:
    out, counts = redact("Pre-redacted: ***-***-1234 in ticket.")
    assert "***-***-1234" in out
    # The pattern won't match this anyway because of the asterisks, but
    # the guard is the safety net for borderline cases.
    assert counts.phones == 0


def test_pass_through_redacted_marker_in_text() -> None:
    out, _counts = redact("Address [REDACTED] by Zendesk admin.")
    assert "[REDACTED]" in out
