//! LLM dispatch + system prompts. Mirrors Python `triage_cli.llm`.

use std::env;

use chrono::{DateTime, Utc};
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;
use thiserror::Error;

use crate::models::{
    fmt_ts, indent_continuations, SiteEntry, StructuredTriageReport, Ticket, TriageBundle,
    ValidationOutcome,
};
use crate::playbook::Rubric;
use crate::providers::{get_provider, ProviderError};
use crate::redact::{redact, RedactionCounts};

pub const SITE_EXTRACTION_PROMPT: &str = "You identify which Carbyne APEX customer site a\nZendesk support ticket is about. A list of known sites is provided. Return JSON\nwith a single field:\n\n{\"site_name\": \"<site_name from the list>\" or null}\n\nRules:\n- You MUST only return a site_name that appears verbatim in the provided list.\n- Return null if no site clearly matches — do not guess.\n- Geographic, agency name, and abbreviation cues in the subject/description\n  matter more than exact wording. \"Roswell PD GA\" → look for a Georgia/Roswell site.";

pub const ANCHOR_EXTRACTION_PROMPT: &str = "You extract the most likely incident timestamp\nfrom a Zendesk ticket. Read the subject, description, and comments. Return JSON\nwith a single field:\n\n{\"timestamp\": \"<ISO 8601 in UTC>\" or null}\n\nReturn null if there is no clear timestamp in the content. Do not guess. A\ngeneric \"this morning\" with no date is null. An explicit \"2026-05-06 14:32 PT\"\nis a timestamp. When in doubt, return null.";

#[derive(Debug, Error)]
pub enum LlmError {
    #[error(transparent)]
    Provider(#[from] ProviderError),
    /// Structured (v1 reframe) emission failed parse or validation after one retry.
    /// `raw_response` carries the second attempt's body so the caller can stash it
    /// at `Tickets/<id>/.debug/llm-response-<ts>.json` (spec section 6, decision 6).
    #[error("structured triage report failed after retry: {message}")]
    StructuredAfterRetry {
        message: String,
        raw_response: String,
        validation_errors: Vec<String>,
    },
}

/// Successful outcome of a `triage_structured` call.
///
/// `validator_warnings` are soft-warn issues (e.g. rubric-row miss) that were
/// accepted but should be surfaced in `STATE.md` and on stderr.
#[derive(Debug, Clone)]
pub struct StructuredOutcome {
    pub report: StructuredTriageReport,
    pub redaction_counts: Option<RedactionCounts>,
    pub validator_warnings: Vec<String>,
}

static FENCE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?is)^\s*```(?:json)?\s*(.*?)\s*```\s*$").unwrap());

static FENCE_RE_EVIDENCE_ID: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"E-\d{3}").unwrap());

/// Extract a JSON object payload from an LLM response.
///
/// Handles four shapes seen in the wild:
///   1. Pure JSON: `{...}`
///   2. ` ```json\n{...}\n``` ` fenced
///   3. Prose preamble + fenced JSON + optional epilogue
///   4. Prose preamble + bare JSON + optional epilogue
///
/// We try `(1)`, then `(2)` via the FENCE_RE anchor regex. If neither works,
/// we fall back to a balanced-brace scan that finds the first `{...}` block
/// at depth 0 (skipping `{` inside strings and escapes). This mirrors Python's
/// `_strip_code_fence` and tolerates assistant-text-block separation common in
/// older provider responses.
fn extract_json_object(s: &str) -> &str {
    let trimmed = s.trim();
    if trimmed.starts_with('{') {
        return trimmed;
    }
    if let Some(c) = FENCE_RE.captures(trimmed) {
        if let Some(m) = c.get(1) {
            return m.as_str();
        }
    }
    // Find a fenced block anywhere in the response.
    if let Some(start) = trimmed.find("```") {
        let after = &trimmed[start + 3..];
        let after = after.strip_prefix("json").unwrap_or(after);
        let after = after.trim_start_matches([' ', '\n', '\r', '\t']);
        if let Some(end) = after.find("```") {
            return after[..end].trim_end_matches(['\n', ' ', '\r', '\t']);
        }
    }
    // Last resort: scan for the first balanced `{...}` block.
    balanced_object(trimmed).unwrap_or(trimmed)
}

fn balanced_object(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let start = bytes.iter().position(|&b| b == b'{')?;
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escape = false;
    for (i, &b) in bytes[start..].iter().enumerate() {
        if escape {
            escape = false;
            continue;
        }
        if b == b'\\' {
            escape = true;
            continue;
        }
        if b == b'"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start..start + i + 1]);
                }
            }
            _ => {}
        }
    }
    None
}

fn model_for_provider(name: &str) -> String {
    // Two providers are valid (see `providers::get_provider`):
    //   * "codex"   — passes the result to `codex exec --model` (subprocess).
    //   * "unleash" — ignores the model string entirely; the assistant is
    //                 selected server-side via `UNLEASH_ASSISTANT_ID`. The
    //                 `UNLEASH_MODEL` env var has no effect today and is read
    //                 only so a future server-side surface can pick it up
    //                 without another code change.
    // Any other `name` here would indicate `get_provider` and this function
    // disagree about the valid set; the catchall returns an empty string
    // rather than panicking so triage still attempts to run.
    match name {
        "codex" => env::var("CODEX_MODEL")
            .unwrap_or_else(|_| crate::providers::codex::DEFAULT_CODEX_MODEL.to_string()),
        _ => env::var("UNLEASH_MODEL").unwrap_or_default(),
    }
}

fn response_preview(s: &str) -> String {
    const HEAD: usize = 400;
    const TAIL: usize = 200;
    let bytes = s.as_bytes();
    if bytes.len() <= HEAD + TAIL {
        return s.to_string();
    }
    let head = String::from_utf8_lossy(&bytes[..HEAD]);
    let tail = String::from_utf8_lossy(&bytes[bytes.len() - TAIL..]);
    format!(
        "{head}\n…[{} bytes elided]…\n{tail}",
        bytes.len() - HEAD - TAIL
    )
}

//
// ──────────────────────────────────────────────────────────────────────
//   v1 reframe — structured StructuredTriageReport emission
//   (spec/v1-reframe.md sections 5 and 6)
// ──────────────────────────────────────────────────────────────────────
//

const STRUCTURED_PROMPT_PREAMBLE: &str =
    "You are a triage assistant for a NOC engineer working on the Carbyne APEX \
NG911/E911 platform at Axon. You receive a Zendesk ticket, customer history, \
prior memory hits, optional Datadog logs, and analyst-supplied evidence.\n\n\
Your job is to produce a SINGLE JSON object (a StructuredTriageReport) that \
drives a five-markdown ticket folder (INTAKE / EVIDENCE_PREFLIGHT / FORK_PACKET \
/ DRAFTS / STATE). Do NOT emit prose, commentary, or anything outside the JSON \
object. A ```json fence is acceptable; the parser strips it.\n";

const STRUCTURED_PROMPT_SCHEMA: &str = "## Output schema\n\n\
```json\n\
{\n  \"intake\": {\n    \"housekeeping_complete\": true,\n    \"ticket\": {\n      \"zendesk_id\": <int>,\n      \"url\": \"...\",\n      \"status\": \"...\",\n      \"priority\": \"...\",\n      \"tags\": [\"...\"],\n      \"requester\": \"...\",\n      \"organization\": \"...\",\n      \"site\": \"...\" | null,\n      \"cnc\": \"...\" | null,\n      \"region\": \"...\" | null,\n      \"affected_stations\": [\"...\"],\n      \"affected_agents\": [\"...\"],\n      \"call_id\": \"...\" | null,\n      \"incident_window\": \"...\",\n      \"reported_symptom\": \"...\"\n    },\n    \"one_line_fingerprint\": \"<customer> / <site> / <symptom-class> / <window> / <prior pattern>\",\n    \"ticket_summary\": [\"3-6 prose bullets\"],\n    \"context_pulls\": [{\"pull\":\"<name>\",\"result\":\"<short>\",\"source\":\"<tool/system>\"}],\n    \"initial_route\": {\"hypothesis\":\"<pre-LLM guess>\",\"justification\":\"<one sentence>\"},\n    \"intake_decision\": \"ready_for_evidence_preflight\" | \"known_issue\" | \"needs_clarification\" | \"cannot_proceed\"\n  },\n  \"evidence_preflight\": {\n    \"gathered\": [{\"id\":\"E-001\",\"evidence_type\":\"<type>\",\"source\":\"<src>\",\"time_window\":\"<window>\",\"summary\":\"<terse>\"}],\n    \"decisive_evidence\": [\"bullets that moved the fork\"],\n    \"missing_or_non_decisive\": [\"bullets that would have helped\"]\n  },\n  \"fork_packet\": {\n    \"commitment\": {\n      \"fork_letter\": \"A\" | \"B\" | \"C\" | \"D\",\n      \"confidence\":  \"low\" | \"medium\" | \"high\",\n      \"quoted_rubric_row\": \"<VERBATIM substring from a Fork signals table row above>\",\n      \"rubric_class\":      \"<Symptom Class N — name>\",\n      \"reasoning\":         \"<one sentence; why this signal commits the fork>\"\n    },\n    \"evidence_summary\": [\"strongest evidence bullets\"],\n    \"missing_evidence\": [\"REQUIRED non-empty when fork_letter is D\"],\n    \"related\": {\"zendesk\":[<ids>],\"jira\":[\"REP-...\"],\"master\":null|<id>,\"cluster\":null|\"<key>\"},\n    \"handoff\": {\n      \"engineering_jira_needed\": {\"needed\":true|false,\"reason\":\"<one line>\"},\n      \"vendor_or_it_needed\":     {\"needed\":true|false,\"reason\":\"<one line>\"},\n      \"customer_note_needed\":    {\"needed\":true|false,\"reason\":\"<one line>\"},\n      \"internal_note_needed\":    {\"needed\":true|false,\"reason\":\"<one line>\"}\n    }\n  },\n  \"drafts\": {\n    \"customer_reply\":        \"<plain-language reply; no jargon; no rubric refs>\",\n    \"internal_zendesk_note\": \"<full triage context for next NOC shift>\",\n    \"jira_draft\": null | {\"title\":\"...\",\"description\":\"...\",\"affected_component\":\"...\"|null,\"suspected_area\":\"...\"|null,\"repro_steps\":[\"...\"],\"project\":\"REP\"}\n  },\n  \"rubric_version\": \"<copy the rubric_version from the rubric above>\"\n}\n```\n";

const STRUCTURED_PROMPT_RULES: &str = "## Forks\n\n\
- **A** = Engineering Jira (Carbyne-controlled code or infra defect)\n\
- **B** = Vendor / Internal IT (carrier, customer ISP/LAN/switch/SDWAN, PSTN)\n\
- **C** = NOC self-resolve (config error, training, working-as-designed)\n\
- **D** = Cannot fork yet (required evidence is missing — populate missing_evidence)\n\n\
## Rules\n\n\
- Output ONLY the JSON object. No prose preamble or epilogue.\n\
- `quoted_rubric_row` MUST be a verbatim substring of a row from one of the rubric's \"Fork signals\" tables.\n\
- `fork_letter` = \"D\" MUST have non-empty `missing_evidence` and MUST NOT have `confidence` = \"high\".\n\
- `intake_decision` = \"known_issue\" should be paired with `related.master` or `related.jira` populated.\n\
- `drafts.jira_draft` should be populated when `fork_letter` is \"A\", null otherwise.\n\
- Do NOT invent ticket IDs, Jira keys, error codes, or past incidents.\n\
- Use empty arrays for fields with no content — do not pad with filler.\n\
- If you would hedge three times in `commitment.reasoning`, the right `confidence` is \"low\".\n\
- `gathered[*].id` MUST be the `E-NNN` ID from the Evidence Index in the user message that best matches this row; use `\"\"` when no Evidence Index was provided.\n\
- `evidence_summary` and `decisive_evidence` bullets SHOULD lead with the `E-NNN` ID of the cited evidence item when the Evidence Index is present.\n";

/// Compose the structured system prompt: preamble + rubric + schema + rules.
pub fn build_structured_system_prompt(rubric: &Rubric) -> String {
    format!(
        "{preamble}\n## Fork rubric (authoritative)\n\n\
         The rubric below is the team's decision logic. Your `quoted_rubric_row` \
         field MUST be a verbatim substring of a row from one of its \"Fork signals\" \
         tables. Paraphrasing is a soft-warn miss; rows from outside the rubric \
         are a soft-warn miss. Forks are committed on rubric signals, not vibes.\n\n\
         <<<RUBRIC version={version}>>>\n{rubric_text}\n<<<END RUBRIC>>>\n\n\
         {schema}\n{rules}",
        preamble = STRUCTURED_PROMPT_PREAMBLE,
        version = rubric.version(),
        rubric_text = rubric.text(),
        schema = STRUCTURED_PROMPT_SCHEMA,
        rules = STRUCTURED_PROMPT_RULES,
    )
}

/// Append a corrective note to the user message on retry. Includes the
/// specific failure (parse error message or validation errors) so the LLM
/// knows what to fix, rather than a generic "try again."
fn build_corrective_user_message(
    original_prompt: &str,
    parse_error: Option<&str>,
    validation_errors: &[String],
) -> String {
    let mut note = String::from(
        "\n\n## RETRY — your previous response failed validation. Fix and resubmit.\n",
    );
    if let Some(e) = parse_error {
        note.push_str(&format!(
            "\nParse error: {e}\n\nYour first response could not be deserialized as a \
             StructuredTriageReport. Return ONLY the JSON object — no prose, no fences \
             except an optional ```json wrapper.\n",
        ));
    }
    if !validation_errors.is_empty() {
        note.push_str("\nValidation errors:\n");
        for e in validation_errors {
            note.push_str(&format!("  - {e}\n"));
        }
        note.push_str(
            "\nFix the listed errors. The rubric, schema, and rules from the system \
             prompt still apply.\n",
        );
    }
    format!("{original_prompt}{note}")
}

/// Soft-warn: every `E-NNN` cited in the report must exist in the bundle's
/// evidence index. Runs only when the bundle has a non-empty evidence_index.
fn validate_evidence_citations(
    report: &StructuredTriageReport,
    valid_ids: &std::collections::HashSet<String>,
) -> Vec<String> {
    let mut warnings = Vec::new();

    // gathered[*].id — direct field check.
    for g in &report.evidence_preflight.gathered {
        if !g.id.is_empty() && !valid_ids.contains(&g.id) {
            warnings.push(format!(
                "evidence id {} in gathered table not found in bundle index",
                g.id
            ));
        }
    }

    // Scan evidence_summary and decisive_evidence bullets for E-NNN patterns.
    let bullets = report
        .fork_packet
        .evidence_summary
        .iter()
        .chain(report.evidence_preflight.decisive_evidence.iter());
    for bullet in bullets {
        for m in FENCE_RE_EVIDENCE_ID.find_iter(bullet) {
            let cited = m.as_str().to_string();
            if !valid_ids.contains(&cited) {
                warnings.push(format!(
                    "evidence id {cited} cited in report not found in bundle index"
                ));
            }
        }
    }

    warnings
}

/// Run the v1 reframe structured triage call.
///
/// Pipeline:
///   1. Build user message from the bundle (existing `as_user_message`).
///   2. Redact PII at the LLM boundary (if `redact_enabled`).
///   3. Send with the structured system prompt (rubric included).
///   4. Parse `StructuredTriageReport`; validate against rubric.
///   5. On parse error OR hard validation error: retry once with a corrective
///      user message that names the specific failure.
///   6. On second failure: return `LlmError::StructuredAfterRetry` carrying
///      the raw response so the caller can stash it for debug.
///
/// Soft-warn validator misses (rubric-row not found, rubric_version drift)
/// are returned in `StructuredOutcome::validator_warnings`, not as errors.
pub async fn triage_structured(
    bundle: &TriageBundle,
    rubric: &Rubric,
    model: Option<&str>,
    verbose: bool,
    redact_enabled: bool,
) -> Result<StructuredOutcome, LlmError> {
    let provider = get_provider()?;
    let resolved_model = model
        .map(str::to_string)
        .unwrap_or_else(|| model_for_provider(provider.name()));
    let system_prompt = build_structured_system_prompt(rubric);

    let raw_user = bundle.as_user_message();
    let (user_prompt, redaction_counts) = if redact_enabled {
        let (r, c) = redact(&raw_user);
        (r, Some(c))
    } else {
        (raw_user, None)
    };

    // First attempt.
    let first_raw = provider
        .complete(&user_prompt, &system_prompt, &resolved_model)
        .await?;
    let first_attempt = try_parse_and_validate(&first_raw, rubric);

    if let TryOutcome::Ok { report, warnings } = first_attempt {
        let mut all_warnings = warnings;
        if !bundle.evidence_index.is_empty() {
            let valid_ids: std::collections::HashSet<String> =
                bundle.evidence_index.iter().map(|e| e.id.clone()).collect();
            all_warnings.extend(validate_evidence_citations(&report, &valid_ids));
        }
        return Ok(StructuredOutcome {
            report,
            redaction_counts,
            validator_warnings: all_warnings,
        });
    }

    if verbose {
        eprintln!(
            "triage_structured: first attempt failed from {}; retrying.",
            provider.name()
        );
        eprintln!(
            "triage_structured: raw response (truncated):\n{}",
            response_preview(&first_raw)
        );
    }

    let (parse_err, val_errs) = match &first_attempt {
        TryOutcome::ParseError(e) => (Some(e.as_str()), &[][..]),
        TryOutcome::ValidationError(errs) => (None, errs.as_slice()),
        TryOutcome::Ok { .. } => unreachable!(),
    };
    let retry_prompt = build_corrective_user_message(&user_prompt, parse_err, val_errs);

    // Second attempt.
    let second_raw = provider
        .complete(&retry_prompt, &system_prompt, &resolved_model)
        .await?;
    match try_parse_and_validate(&second_raw, rubric) {
        TryOutcome::Ok { report, warnings } => {
            let mut all_warnings = warnings;
            if !bundle.evidence_index.is_empty() {
                let valid_ids: std::collections::HashSet<String> =
                    bundle.evidence_index.iter().map(|e| e.id.clone()).collect();
                all_warnings.extend(validate_evidence_citations(&report, &valid_ids));
            }
            Ok(StructuredOutcome {
                report,
                redaction_counts,
                validator_warnings: all_warnings,
            })
        }
        TryOutcome::ParseError(e) => {
            if verbose {
                eprintln!(
                    "triage_structured: retry response (truncated):\n{}",
                    response_preview(&second_raw)
                );
            }
            Err(LlmError::StructuredAfterRetry {
                message: format!("parse error after retry: {e}"),
                raw_response: second_raw,
                validation_errors: Vec::new(),
            })
        }
        TryOutcome::ValidationError(errs) => {
            if verbose {
                eprintln!(
                    "triage_structured: retry validation failed: {errs:?}\n{}",
                    response_preview(&second_raw)
                );
            }
            Err(LlmError::StructuredAfterRetry {
                message: "validation failed after retry".into(),
                raw_response: second_raw,
                validation_errors: errs,
            })
        }
    }
}

/// Result of a single parse-and-validate attempt against the rubric.
///
/// `large_enum_variant` is allowed: this enum is internal and constructed
/// at most twice per LLM call; the ~1 KB size difference is irrelevant
/// next to the network round-trip cost.
#[allow(clippy::large_enum_variant)]
enum TryOutcome {
    Ok {
        report: StructuredTriageReport,
        warnings: Vec<String>,
    },
    ParseError(String),
    ValidationError(Vec<String>),
}

fn try_parse_and_validate(raw: &str, rubric: &Rubric) -> TryOutcome {
    let json = extract_json_object(raw);
    let report: StructuredTriageReport = match serde_json::from_str(json) {
        Ok(r) => r,
        Err(e) => return TryOutcome::ParseError(e.to_string()),
    };
    let ValidationOutcome { warnings, errors } = report.validate(rubric);
    if errors.is_empty() {
        TryOutcome::Ok { report, warnings }
    } else {
        TryOutcome::ValidationError(errors)
    }
}

/// Best-effort site identification against the known list. Returns the
/// canonical `site_name` if the model picked one we recognize; `None` otherwise.
pub async fn extract_site(
    ticket: &Ticket,
    sites: &[SiteEntry],
    model: Option<&str>,
) -> Result<Option<String>, LlmError> {
    let provider = get_provider()?;
    let resolved_model = model
        .map(str::to_string)
        .unwrap_or_else(|| model_for_provider(provider.name()));
    let known_names: std::collections::HashMap<String, String> = sites
        .iter()
        .filter(|e| !e.site_name.is_empty())
        .map(|e| (e.site_name.to_ascii_lowercase(), e.site_name.clone()))
        .collect();
    if known_names.is_empty() {
        return Ok(None);
    }
    let site_list: String = sites
        .iter()
        .filter(|e| !e.site_name.is_empty())
        .map(|e| {
            format!(
                "  site_name: {}  |  friendly_name: {}",
                e.site_name, e.friendly_name
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let desc_head: String = ticket.description.chars().take(500).collect();
    let prompt = format!(
        "Known sites:\n{site_list}\n\nTicket subject: {}\nOrg: {}\nDescription (first 500 chars): {}",
        ticket.subject,
        ticket.requester_org.as_deref().unwrap_or("(none)"),
        desc_head
    );
    let raw = provider
        .complete(&prompt, SITE_EXTRACTION_PROMPT, &resolved_model)
        .await?;
    let payload = extract_json_object(&raw);
    let Ok(data) = serde_json::from_str::<Value>(payload) else {
        return Ok(None);
    };
    let Some(sn) = data.get("site_name") else {
        return Ok(None);
    };
    if sn.is_null() {
        return Ok(None);
    }
    let Some(sn_str) = sn.as_str() else {
        return Ok(None);
    };
    Ok(known_names.get(&sn_str.to_ascii_lowercase()).cloned())
}

/// Best-effort timestamp extraction. Returns `None` on null/missing/malformed.
pub async fn extract_anchor(
    ticket: &Ticket,
    model: Option<&str>,
) -> Result<Option<DateTime<Utc>>, LlmError> {
    let provider = get_provider()?;
    let resolved_model = model
        .map(str::to_string)
        .unwrap_or_else(|| model_for_provider(provider.name()));
    let prompt = ticket_for_anchor(ticket);
    let raw = provider
        .complete(&prompt, ANCHOR_EXTRACTION_PROMPT, &resolved_model)
        .await?;
    let payload = extract_json_object(&raw);
    let Ok(data) = serde_json::from_str::<Value>(payload) else {
        return Ok(None);
    };
    let Some(ts) = data.get("timestamp") else {
        return Ok(None);
    };
    if ts.is_null() {
        return Ok(None);
    }
    let Some(ts_str) = ts.as_str() else {
        return Ok(None);
    };
    let parsed = DateTime::parse_from_rfc3339(ts_str).map(|d| d.with_timezone(&Utc));
    Ok(parsed.ok())
}

fn ticket_for_anchor(ticket: &Ticket) -> String {
    let mut lines = vec![
        format!("Subject: {}", ticket.subject),
        format!("Description: {}", indent_continuations(&ticket.description)),
        "Comments:".into(),
    ];
    if ticket.comments.is_empty() {
        lines.push("(no comments)".into());
    } else {
        for c in &ticket.comments {
            let prefix = if c.is_public { "" } else { "[internal] " };
            let body = indent_continuations(&c.body);
            lines.push(format!(
                "- {prefix}{} — {}: {body}",
                fmt_ts(&c.created_at),
                c.author
            ));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod structured_tests {
    use super::*;
    use crate::models::{
        Confidence, ContextPull, DraftsBlock, ForkCommitment, ForkLetter, ForkPacket,
        GatheredEvidence, HandoffBlock, HandoffItem, InitialRoute, IntakeBlock, IntakeDecision,
        IntakeTicketFacts, PreflightBlock, RelatedWork,
    };

    fn sample_report(rubric_version: &str) -> StructuredTriageReport {
        StructuredTriageReport {
            intake: IntakeBlock {
                housekeeping_complete: true,
                ticket: IntakeTicketFacts {
                    zendesk_id: 44671,
                    url: "https://carbyne.zendesk.com/agent/tickets/44671".into(),
                    status: "open".into(),
                    priority: "".into(),
                    tags: vec!["high".into(), "network_issue".into()],
                    requester: "Brandon Jenkins".into(),
                    organization: "JeffCom".into(),
                    site: Some("us-co-jeffcom-apex".into()),
                    cnc: Some("fcef70f9-b814-45eb-bc99-abfb59877d5c".into()),
                    region: Some("gov-west-1".into()),
                    affected_stations: vec!["Jeffcom-74".into()],
                    affected_agents: vec!["Kyler Cook".into()],
                    call_id: None,
                    incident_window: "2026-05-12 06:30:30-06:31:10 UTC".into(),
                    reported_symptom: "All consoles flickered black; Network Error Resolved popup".into(),
                },
                one_line_fingerprint: "JeffCom / us-co-jeffcom-apex / network error banner / 06:30 UTC / prior 43874".into(),
                ticket_summary: vec!["Brief multi-console outage".into()],
                context_pulls: vec![ContextPull {
                    pull: "Last related Zendesk tickets".into(),
                    result: "43874 similar symptom".into(),
                    source: "Zendesk search_tickets".into(),
                }],
                initial_route: InitialRoute {
                    hypothesis: "Fork B".into(),
                    justification: "Multi-console symptom suggests site-network instability".into(),
                },
                intake_decision: IntakeDecision::ReadyForEvidencePreflight,
            },
            evidence_preflight: PreflightBlock {
                gathered: vec![GatheredEvidence {
                    id: String::new(),
                    evidence_type: "station log".into(),
                    source: "Jeffcom-74 log bundle".into(),
                    time_window: "06:30:33-06:31:05 UTC".into(),
                    summary: "SIP OPTIONS failure + reconnect".into(),
                }],
                decisive_evidence: vec!["Multiple stations flipped ERROR within seconds".into()],
                missing_or_non_decisive: vec!["No AWS Direct Connect event captured".into()],
            },
            fork_packet: ForkPacket {
                commitment: ForkCommitment {
                    fork_letter: ForkLetter::B,
                    confidence: Confidence::Medium,
                    quoted_rubric_row: "customer LAN, switch, or SDWAN. Link to site master ticket".into(),
                    rubric_class: "Symptom Class 3 — Network error banner / WebSocket disconnect / station drops".into(),
                    reasoning: "Multi-station flip is the Class 3 (b) signal".into(),
                },
                evidence_summary: vec!["20 timeouts across multiple JeffCom machines in one minute".into()],
                missing_evidence: vec![],
                related: RelatedWork {
                    zendesk: vec![43874, 42708],
                    jira: vec![],
                    master: None,
                    cluster: Some("jeffcom-all-console-network-error".into()),
                },
                handoff: HandoffBlock {
                    engineering_jira_needed: HandoffItem { needed: false, reason: "".into() },
                    vendor_or_it_needed: HandoffItem {
                        needed: true,
                        reason: "Request network-path RCA for the 06:30:30-06:31:10 UTC window".into(),
                    },
                    customer_note_needed: HandoffItem {
                        needed: true,
                        reason: "Explain we found a short multi-console interruption".into(),
                    },
                    internal_note_needed: HandoffItem {
                        needed: true,
                        reason: "Document fork B decision for next NOC shift".into(),
                    },
                },
            },
            drafts: DraftsBlock {
                customer_reply: "Hi Brandon — we found a brief multi-console network event…".into(),
                internal_zendesk_note: "Fork B; rubric row: 'customer LAN, switch, or SDWAN…'; request RCA from network team.".into(),
                jira_draft: None,
            },
            rubric_version: rubric_version.to_string(),
        }
    }

    #[test]
    fn system_prompt_includes_rubric_text_and_version() {
        let rubric = Rubric::load().unwrap();
        let prompt = build_structured_system_prompt(&rubric);
        assert!(
            prompt.contains("Fork rubric (authoritative)"),
            "missing rubric heading"
        );
        assert!(
            prompt.contains(&format!("<<<RUBRIC version={}>>>", rubric.version())),
            "missing rubric version marker"
        );
        // A row that exists verbatim in the embedded rubric:
        assert!(
            prompt.contains("customer LAN, switch, or SDWAN. Link to site master ticket"),
            "rubric body not injected"
        );
    }

    #[test]
    fn system_prompt_states_fork_d_rule() {
        let rubric = Rubric::load().unwrap();
        let prompt = build_structured_system_prompt(&rubric);
        assert!(
            prompt.contains("fork_letter") && prompt.contains("\"D\""),
            "schema does not mention fork D"
        );
        assert!(
            prompt.contains("MUST have non-empty `missing_evidence`"),
            "rules do not enforce missing_evidence on fork D"
        );
        assert!(
            prompt.contains("MUST NOT have `confidence` = \"high\""),
            "rules do not forbid D + high confidence"
        );
    }

    #[test]
    fn corrective_message_includes_parse_error() {
        let out = build_corrective_user_message(
            "ORIGINAL",
            Some("expected `:` at line 5 column 12"),
            &[],
        );
        assert!(out.starts_with("ORIGINAL"));
        assert!(out.contains("Parse error"));
        assert!(out.contains("expected `:`"));
        assert!(out.contains("RETRY"));
    }

    #[test]
    fn corrective_message_includes_validation_errors() {
        let errs = vec![
            "fork_letter is D but missing_evidence is empty".to_string(),
            "fork_letter is D with confidence=high; incoherent".to_string(),
        ];
        let out = build_corrective_user_message("ORIGINAL", None, &errs);
        assert!(out.contains("Validation errors"));
        assert!(out.contains("missing_evidence is empty"));
        assert!(out.contains("confidence=high"));
    }

    #[test]
    fn try_parse_succeeds_on_well_formed_report() {
        let rubric = Rubric::load().unwrap();
        let report = sample_report(rubric.version());
        let json = serde_json::to_string(&report).unwrap();
        match try_parse_and_validate(&json, &rubric) {
            TryOutcome::Ok { report, warnings } => {
                assert_eq!(report.fork_packet.commitment.fork_letter, ForkLetter::B);
                assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn try_parse_extracts_from_fenced_response() {
        let rubric = Rubric::load().unwrap();
        let report = sample_report(rubric.version());
        let json = serde_json::to_string(&report).unwrap();
        let fenced = format!("```json\n{json}\n```");
        match try_parse_and_validate(&fenced, &rubric) {
            TryOutcome::Ok { .. } => {}
            other => panic!("fenced response should parse; got {other:?}"),
        }
    }

    #[test]
    fn try_parse_returns_parse_error_for_garbage() {
        let rubric = Rubric::load().unwrap();
        match try_parse_and_validate("not json at all", &rubric) {
            TryOutcome::ParseError(_) => {}
            other => panic!("expected ParseError, got {other:?}"),
        }
    }

    #[test]
    fn try_parse_returns_validation_error_for_fork_d_without_missing_evidence() {
        let rubric = Rubric::load().unwrap();
        let mut report = sample_report(rubric.version());
        report.fork_packet.commitment.fork_letter = ForkLetter::D;
        report.fork_packet.commitment.confidence = Confidence::Low;
        // missing_evidence stays empty — should trigger validation error.
        let json = serde_json::to_string(&report).unwrap();
        match try_parse_and_validate(&json, &rubric) {
            TryOutcome::ValidationError(errs) => {
                assert!(errs.iter().any(|e| e.contains("missing_evidence")));
            }
            other => panic!("expected ValidationError, got {other:?}"),
        }
    }

    #[test]
    fn try_parse_returns_ok_with_warning_on_bogus_rubric_row() {
        let rubric = Rubric::load().unwrap();
        let mut report = sample_report(rubric.version());
        report.fork_packet.commitment.quoted_rubric_row = "not actually a rubric row".into();
        let json = serde_json::to_string(&report).unwrap();
        match try_parse_and_validate(&json, &rubric) {
            TryOutcome::Ok { warnings, .. } => {
                assert!(!warnings.is_empty(), "expected a soft-warn");
                assert!(warnings.iter().any(|w| w.contains("not found verbatim")));
            }
            other => panic!("expected Ok with warnings, got {other:?}"),
        }
    }

    // Needed because TryOutcome is not Debug-derivable (it holds StructuredTriageReport
    // which is large) — provide a manual impl just for test panics.
    impl std::fmt::Debug for TryOutcome {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                TryOutcome::Ok { warnings, .. } => write!(f, "Ok(warnings={warnings:?})"),
                TryOutcome::ParseError(e) => write!(f, "ParseError({e:?})"),
                TryOutcome::ValidationError(es) => write!(f, "ValidationError({es:?})"),
            }
        }
    }
}
