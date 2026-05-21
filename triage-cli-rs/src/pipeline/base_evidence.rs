use crate::models::TriageBundle;

/// Per-entry cap on base-evidence body snapshots. Kept in sync with
/// ZIP_ENTRY_CAP_BYTES in investigation.rs so attached zip entries and
/// snapshot bodies share the same size budget.
pub const BODY_SNAPSHOT_CAP_BYTES: usize = 256 * 1024;

/// Truncate `body` to at most `BODY_SNAPSHOT_CAP_BYTES`, respecting UTF-8
/// char boundaries. Appends a `"\n\n[truncated]"` marker when truncation
/// occurs — the returned string may exceed the cap by ~14 bytes (the
/// marker length). Returns `None` for empty input (including the
/// pathological case where the entire cap is one codepoint).
pub(crate) fn cap_body_snapshot(body: String) -> Option<String> {
    if body.is_empty() {
        return None;
    }
    if body.len() <= BODY_SNAPSHOT_CAP_BYTES {
        return Some(body);
    }
    // Find the last char boundary at or below the cap so we never slice
    // mid-codepoint.
    let mut cut = BODY_SNAPSHOT_CAP_BYTES;
    while cut > 0 && !body.is_char_boundary(cut) {
        cut -= 1;
    }
    // Pathological case: the entire cap window is occupied by a single
    // multi-byte codepoint — no valid boundary found. Return None rather
    // than Some("\n\n[truncated]"), which would violate the contract.
    if cut == 0 {
        return None;
    }
    let mut truncated = body[..cut].to_string();
    truncated.push_str("\n\n[truncated]");
    Some(truncated)
}

/// Build `BaseEvidenceEntry` list from the catalog (`bundle.evidence_index`)
/// plus the bundle's content fields. Populates `body` per kind; returns
/// `None` for kinds that can't be matched or that yield an empty body.
///
/// Extracted as a free function so the mapping is unit-testable in
/// isolation from the rest of `investigate_one_structured`.
pub fn collect_base_evidence_entries(bundle: &TriageBundle) -> Vec<crate::models::BaseEvidenceEntry> {
    use crate::models::BaseEvidenceEntry;
    bundle
        .evidence_index
        .iter()
        .map(|item| {
            let body = match item.kind.as_str() {
                // Note: matches by label only. If multiple pasted_logs share the
                // same label, only the first match's body is captured — a
                // pre-existing ambiguity in assign_evidence_ids. Tracked in
                // ADR-0003.
                "pasted_note" => bundle
                    .pasted_logs
                    .iter()
                    .find(|p| p.label == item.label)
                    .and_then(|p| cap_body_snapshot(p.text.clone())),
                // Note: matches by basename only. Two local files with the same
                // basename in different directories collide on the first match.
                // Same pre-existing ambiguity as the pasted_note arm.
                "local_file" => bundle
                    .local_files
                    .iter()
                    .find(|lf| {
                        lf.path
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_else(|| lf.path.display().to_string())
                            == item.label
                    })
                    .and_then(|lf| lf.extracted_text.clone())
                    .and_then(cap_body_snapshot),
                "datadog_log_window" => {
                    if bundle.log_lines.is_empty() {
                        None
                    } else {
                        let rendered = bundle
                            .log_lines
                            .iter()
                            .map(|l| {
                                format!(
                                    "{} [{}] {}",
                                    crate::models::fmt_ts(&l.timestamp),
                                    l.level,
                                    l.message
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        cap_body_snapshot(rendered)
                    }
                }
                "zendesk_comment" => bundle
                    .ticket
                    .comments
                    .iter()
                    .find(|c| {
                        format!("comment:{}", crate::models::fmt_ts(&c.created_at))
                            == item.source_path
                    })
                    .and_then(|c| cap_body_snapshot(c.body.clone())),
                "attachment" => bundle
                    .downloaded_attachments
                    .iter()
                    .find(|a| a.filename == item.label)
                    .and_then(|a| a.extracted_text.clone())
                    .and_then(cap_body_snapshot),
                "customer_history" => bundle.customer_history.as_ref().and_then(|h| {
                    if h.tickets.is_empty() {
                        return None;
                    }
                    let mut lines = vec![format!("{} prior ticket(s):", h.tickets.len())];
                    for t in &h.tickets {
                        lines.push(format!(
                            "- #{} [{}] {} (created {})",
                            t.id,
                            t.status,
                            t.subject,
                            crate::models::fmt_ts(&t.created_at)
                        ));
                    }
                    cap_body_snapshot(lines.join("\n"))
                }),
                "memory_hit" => bundle.memory_context.as_ref().and_then(|ctx| {
                    let needle = item
                        .source_path
                        .strip_prefix("memory:")
                        .unwrap_or(&item.source_path);
                    ctx.entries
                        .iter()
                        .find(|e| e.ticket_id == needle)
                        .and_then(|e| {
                            cap_body_snapshot(format!(
                                "ticket_id: {}\nsubject: {}\nassessment: {}",
                                e.ticket_id, e.subject, e.assessment
                            ))
                        })
                }),
                _ => None,
            };
            BaseEvidenceEntry {
                item: item.clone(),
                body,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_base_evidence_entries_copies_paste_body() {
        // collect_base_evidence_entries must copy a pasted_note's text into
        // the entry's `body` field — the central guarantee of ADR-0003 for
        // the pasted_note kind.
        use chrono::TimeZone;
        let ticket = crate::models::Ticket {
            id: 1,
            subject: "t".into(),
            description: "d".into(),
            requester_org: None,
            requester_email: None,
            tags: vec![],
            created_at: chrono::Utc.with_ymd_and_hms(2026, 5, 12, 0, 0, 0).unwrap(),
            updated_at: None,
            comments: vec![],
        };
        let mut bundle = crate::models::TriageBundle {
            ticket,
            site_entry: None,
            log_lines: vec![],
            log_truncated: false,
            anchor: None,
            anchor_source: None,
            window_start: None,
            window_end: None,
            downloaded_attachments: vec![],
            local_files: vec![],
            pasted_logs: vec![crate::models::PastedEvidence {
                label: "customer-note".into(),
                text: "PASTE_BODY_SENTINEL_42".into(),
            }],
            customer_history: None,
            memory_context: None,
            evidence_index: vec![],
        };
        bundle.evidence_index = crate::models::assign_evidence_ids(&bundle);
        // Assign deterministic IDs (E-NNN) as the production pipeline does.
        // The lookup in `collect_base_evidence_entries` is by label, not id,
        // but we still set ids for realism.
        for (counter, it) in (1..).zip(bundle.evidence_index.iter_mut()) {
            it.id = format!("E-{counter:03}");
        }

        let entries = collect_base_evidence_entries(&bundle);
        let paste_entry = entries
            .iter()
            .find(|e| e.item.kind == "pasted_note")
            .expect("pasted_note entry missing");
        assert_eq!(
            paste_entry.body.as_deref(),
            Some("PASTE_BODY_SENTINEL_42"),
            "pasted_note body was not captured into BaseEvidenceEntry"
        );
    }

    #[test]
    fn cap_body_snapshot_empty_returns_none() {
        assert!(cap_body_snapshot(String::new()).is_none());
    }

    #[test]
    fn cap_body_snapshot_short_returns_unchanged() {
        let s = "short content".to_string();
        let result = cap_body_snapshot(s.clone()).unwrap();
        assert_eq!(result, s);
    }

    #[test]
    fn cap_body_snapshot_long_truncates_with_marker() {
        let s = "a".repeat(BODY_SNAPSHOT_CAP_BYTES + 100);
        let result = cap_body_snapshot(s).unwrap();
        assert!(result.contains("[truncated]"));
        // Marker overage is documented; allow ~14 bytes slack.
        assert!(result.len() <= BODY_SNAPSHOT_CAP_BYTES + 32);
    }

    #[test]
    fn base_evidence_legacy_v1_manifest_parses_with_none_bodies() {
        // Old v1 manifests on disk have no `body` field per entry. They must
        // deserialize cleanly into the v2 BaseEvidenceEntry shape, with
        // `body == None` everywhere (serde flatten + default).
        let dir = tempfile::tempdir().unwrap();
        let session_dir = dir.path().join(".session");
        std::fs::create_dir_all(&session_dir).unwrap();
        let manifest_path = session_dir.join("base-evidence-manifest.json");
        // Hand-rolled v1 JSON: evidence entries are flat EvidenceItem
        // objects with no `body` field.
        let v1_json = r#"{
            "schema": "triage-cli/base-evidence",
            "schema_version": 1,
            "ticket_id": "12345",
            "captured_at": "2026-05-12T00:00:00Z",
            "evidence": [
                {
                    "id": "E-001",
                    "kind": "datadog_log_window",
                    "label": "site log window",
                    "source_path": "datadog:log_window"
                },
                {
                    "id": "E-002",
                    "kind": "local_file",
                    "label": "apex.log",
                    "source_path": "local:apex.log"
                }
            ]
        }"#;
        std::fs::write(&manifest_path, v1_json).unwrap();
        let bem =
            crate::chat::read_base_evidence_manifest(dir.path()).expect("v1 manifest must parse");
        assert_eq!(bem.evidence.len(), 2);
        for entry in &bem.evidence {
            assert!(
                entry.body.is_none(),
                "v1 entry {} unexpectedly carries a body: {:?}",
                entry.item.id,
                entry.body
            );
        }
    }
}
