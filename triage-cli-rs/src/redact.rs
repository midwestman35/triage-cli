//! PII redactor applied at the LLM boundary.
//!
//! Scope (locked by Python spec `2026-05-10-final-phase-design.md`):
//! - Caller PII only: phones, addresses, GPS coords.
//! - Names: explicit gap (regex unreliable).
//! - Operational IDs (Call-IDs, ticket #s, station codes, CNCs, sites): preserved.

use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};

static PHONE_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?:^|[^A-Za-z0-9])((?:\+?1[-.\s]?)?\(?\d{3}\)?[-.\s]?\d{3}[-.\s]?\d{4})(?:$|[^A-Za-z0-9])",
    )
    .expect("phone regex")
});

static STREET_SUFFIXES: &str = concat!(
    "Ave(?:nue)?",
    "|Blvd|Boulevard",
    "|Cir(?:cle)?",
    "|Ct|Court",
    "|Dr(?:ive)?",
    "|Expy|Expressway",
    "|Fwy|Freeway",
    "|Hwy|Highway",
    "|Ln|Lane",
    "|Loop",
    "|Pkwy|Parkway",
    "|Pl(?:ace)?",
    "|Rd|Road",
    "|Route|Rte",
    "|Sq|Square",
    "|St(?:reet)?",
    "|Ter(?:race)?",
    "|Trl|Trail",
    "|Way",
);

static ADDRESS_PATTERN: Lazy<Regex> = Lazy::new(|| {
    let pattern = format!(r"\b\d+\s+(?:[A-Z][A-Za-z'-]*\s+)+(?:{})\b", STREET_SUFFIXES);
    Regex::new(&pattern).expect("address regex")
});

static COORD_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"-?\d{1,2}\.\d{4,}\s*[,;\s]\s*-?\d{1,3}\.\d{4,}").expect("coord regex")
});

/// Per-call redaction tally surfaced via verbose stderr and saved JSON.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RedactionCounts {
    pub phones: u32,
    pub addresses: u32,
    pub coords: u32,
    pub enabled: bool,
}

impl RedactionCounts {
    pub fn enabled() -> Self {
        Self {
            phones: 0,
            addresses: 0,
            coords: 0,
            enabled: true,
        }
    }
}

/// Loose coordinate shape: decimal pair with only 2-3 fractional digits.
/// Sits *below* `COORD_PATTERN`'s `\d{4,}` floor, so a match here is a
/// plausible GPS pair the strict scrubber would not have caught.
static RESIDUAL_COORD_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?:^|[^\d.])(-?\d{1,3}\.\d{2,3}\s*[,;\s]\s*-?\d{1,3}\.\d{2,3})(?:$|[^\d.])")
        .expect("residual coord regex")
});

/// Loose 7-digit local-number shape (`nnn-nnnn`). The strict `PHONE_PATTERN`
/// only matches the 10/11-digit form, so bare local numbers slip past it.
/// Boundaries are restricted to whitespace/punctuation (not alphanumeric
/// characters or hyphens) so hyphenated operational IDs - Call-IDs, ticket
/// numbers, CNC UUIDs - do not register as residue.
static RESIDUAL_LOCAL_PHONE_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?:^|[^A-Za-z0-9_-])(\d{3}[-.\s]\d{4})(?:$|[^A-Za-z0-9_-])")
        .expect("residual local phone regex")
});

/// Density floor before a residual soft-warn is raised. Deliberately > 1: a
/// single loose match is more likely an operational identifier or a version
/// string than a real caller-PII leak, and this validator is non-blocking by
/// design (same soft-warn philosophy as the rubric validator). Raising the
/// floor trades a little leak sensitivity for far fewer false positives;
/// tightening it later is backward-compatible.
pub const RESIDUAL_PII_WARN_THRESHOLD: usize = 3;

/// Best-effort residual-PII density check, run on the *already-redacted* text.
/// Counts caller-PII-shaped tokens the strict scrubbers are known to miss
/// (sub-`\d{4,}` coord pairs, 7-digit local numbers). Returns a soft-warn
/// string when the count reaches `RESIDUAL_PII_WARN_THRESHOLD`, else `None`.
///
/// This never mutates the payload and never blocks the LLM call — the caller
/// folds the string into the existing `validator_warnings` channel (stderr +
/// `STATE.md`). The redaction sentinels carry no digits, so they cannot
/// self-trigger this scan.
pub fn residual_pii_warning(redacted: &str, counts: &RedactionCounts) -> Option<String> {
    if !counts.enabled {
        return None;
    }
    let residual = count_capture_group_matches(&RESIDUAL_COORD_PATTERN, redacted, 1)
        + count_capture_group_matches(&RESIDUAL_LOCAL_PHONE_PATTERN, redacted, 1);
    if residual >= RESIDUAL_PII_WARN_THRESHOLD {
        Some(format!(
            "redaction: {residual} residual caller-PII-shaped token(s) survived scrub \
             (soft-warn; payload not blocked)"
        ))
    } else {
        None
    }
}

fn count_capture_group_matches(re: &Regex, text: &str, group: usize) -> usize {
    let mut count = 0;
    let mut start = 0;
    while start <= text.len() {
        let Some(caps) = re.captures_at(text, start) else {
            break;
        };
        let Some(m) = caps.get(group) else {
            break;
        };
        count += 1;

        // The regex consumes the trailing delimiter to enforce a right boundary.
        // Advance to the end of the captured PII token, not the whole match, so
        // that delimiter can also serve as the left boundary of the next token.
        let next = m.end();
        if next > start {
            start = next;
        } else {
            start += text[start..]
                .chars()
                .next()
                .map(char::len_utf8)
                .unwrap_or(1);
        }
    }
    count
}

fn is_pre_redacted(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.contains("***") || lower.contains("xxx") || lower.contains("[redacted]")
}

/// Redact caller PII from `text`. Returns (redacted_text, counts).
pub fn redact(text: &str) -> (String, RedactionCounts) {
    let mut counts = RedactionCounts::enabled();

    // The phone pattern wraps a capturing group so we can preserve the
    // surrounding non-alphanumeric boundary character. Replace only the
    // captured phone substring within each match.
    let phone_replaced = replace_with_group(&PHONE_PATTERN, text, 1, |_| {
        counts.phones += 1;
        "<PHONE>".to_string()
    });

    let address_replaced =
        ADDRESS_PATTERN.replace_all(&phone_replaced, |caps: &regex::Captures| {
            let matched = &caps[0];
            if is_pre_redacted(matched) {
                return matched.to_string();
            }
            counts.addresses += 1;
            "<ADDR>".to_string()
        });

    let coord_replaced = COORD_PATTERN.replace_all(&address_replaced, |caps: &regex::Captures| {
        let matched = &caps[0];
        if is_pre_redacted(matched) {
            return matched.to_string();
        }
        counts.coords += 1;
        "<COORDS>".to_string()
    });

    (coord_replaced.into_owned(), counts)
}

/// Replace only capture-group `group` within each match of `re`, preserving the
/// rest of the match. Python's regex implementation gives us negative
/// lookarounds; Rust's `regex` crate doesn't support them, so the equivalent is
/// to mark word boundaries with a capture group and replace just that group.
fn replace_with_group<F>(re: &Regex, text: &str, group: usize, mut replacer: F) -> String
where
    F: FnMut(&str) -> String,
{
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for caps in re.captures_iter(text) {
        let m = caps.get(group).expect("group must exist");
        out.push_str(&text[last..m.start()]);
        if is_pre_redacted(m.as_str()) {
            out.push_str(m.as_str());
        } else {
            out.push_str(&replacer(m.as_str()));
        }
        last = m.end();
    }
    out.push_str(&text[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_phone() {
        let (out, counts) = redact("call (555) 123-4567 now");
        assert_eq!(out, "call <PHONE> now");
        assert_eq!(counts.phones, 1);
    }

    #[test]
    fn preserves_pre_redacted_phone() {
        let (out, counts) = redact("call ***-***-1234 now");
        assert_eq!(out, "call ***-***-1234 now");
        assert_eq!(counts.phones, 0);
    }

    #[test]
    fn redacts_street_address() {
        let (out, counts) = redact("the call from 123 Main Street is bad");
        assert_eq!(out, "the call from <ADDR> is bad");
        assert_eq!(counts.addresses, 1);
    }

    #[test]
    fn redacts_coords() {
        let (out, counts) = redact("loc 36.1699, -115.1398 reported");
        assert_eq!(out, "loc <COORDS> reported");
        assert_eq!(counts.coords, 1);
    }

    #[test]
    fn ignores_phone_inside_token() {
        let (out, counts) = redact("abc5551234567xyz");
        assert_eq!(out, "abc5551234567xyz");
        assert_eq!(counts.phones, 0);
    }

    #[test]
    fn residual_warn_disabled_when_redaction_off() {
        let mut counts = RedactionCounts::enabled();
        counts.enabled = false;
        assert!(
            residual_pii_warning("36.16, -115.13 36.17, -115.14 36.18, -115.15", &counts).is_none()
        );
    }

    #[test]
    fn residual_warn_below_threshold_is_none() {
        let counts = RedactionCounts::enabled();
        // One loose coord pair only — below the density floor.
        assert!(residual_pii_warning("loc 36.16, -115.13 reported", &counts).is_none());
    }

    #[test]
    fn residual_warn_fires_on_dense_loose_coords() {
        let counts = RedactionCounts::enabled();
        let text = "a 36.16, -115.13 b 40.71, -74.00 c 34.05, -118.24 d";
        let w = residual_pii_warning(text, &counts).expect("expected residual soft-warn");
        assert!(w.contains("residual caller-PII-shaped"), "{w}");
        assert!(w.contains("payload not blocked"), "{w}");
    }

    #[test]
    fn residual_warn_counts_adjacent_loose_coords() {
        let counts = RedactionCounts::enabled();
        let text = "36.16, -115.13 36.17, -115.14 36.18, -115.15";
        assert!(residual_pii_warning(text, &counts).is_some());
    }

    #[test]
    fn residual_warn_counts_loose_local_phones() {
        let counts = RedactionCounts::enabled();
        let text = "call 555-1234 or 867-5309 then 555-0000 back";
        assert!(residual_pii_warning(text, &counts).is_some());
    }

    #[test]
    fn residual_warn_counts_adjacent_local_phones() {
        let counts = RedactionCounts::enabled();
        let text = "555-1234 867-5309 555-0000";
        assert!(residual_pii_warning(text, &counts).is_some());
    }

    #[test]
    fn residual_warn_ignores_hyphenated_operational_ids() {
        let counts = RedactionCounts::enabled();
        // Call-ID / ticket-number style: hyphenated digit groups embedded in
        // longer tokens must not register as local-phone residue.
        let text = "CB-911-2024-5551234 ref TICKET-2024-5559999 cnc 2024-5551000-aa";
        assert!(
            residual_pii_warning(text, &counts).is_none(),
            "operational IDs flagged as PII"
        );
    }

    #[test]
    fn residual_warn_does_not_self_trigger_on_sentinels() {
        let counts = RedactionCounts::enabled();
        let text = "<PHONE> <ADDR> <COORDS> <PHONE> <ADDR> <COORDS>";
        assert!(residual_pii_warning(text, &counts).is_none());
    }
}
