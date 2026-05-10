"""Tests for triage_cli.redact (PII redactor at the LLM boundary)."""
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
