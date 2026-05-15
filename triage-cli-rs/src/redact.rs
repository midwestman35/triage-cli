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
}
