//! JSON Schema builders for Codex app-server `outputSchema` turns.
//!
//! The provider only supplies schemas here. `llm.rs` remains responsible for
//! parsing, validation, and retry ownership after the assistant text returns.

use serde_json::{json, Value};

pub(crate) fn structured_triage_output_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "triage-cli/structured-triage/v1",
        "title": "StructuredTriageReport",
        "description": "triage-cli/structured-triage/v1 schema for Codex app-server structured turns",
        "type": "object",
        "additionalProperties": false,
        "required": [
            "intake",
            "evidence_preflight",
            "fork_packet",
            "drafts",
            "rubric_version"
        ],
        "properties": {
            "intake": intake_schema(),
            "evidence_preflight": evidence_preflight_schema(),
            "fork_packet": fork_packet_schema(),
            "drafts": drafts_schema(),
            "rubric_version": string_schema()
        }
    })
}

pub(crate) fn site_output_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "triage-cli/site-extraction/v1",
        "title": "SiteExtraction",
        "description": "triage-cli/site-extraction/v1 schema for known-site extraction",
        "type": "object",
        "additionalProperties": false,
        "required": ["site_name"],
        "properties": {
            "site_name": nullable_string_schema()
        }
    })
}

pub(crate) fn anchor_output_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "triage-cli/anchor-extraction/v1",
        "title": "AnchorExtraction",
        "description": "triage-cli/anchor-extraction/v1 schema for incident timestamp extraction",
        "type": "object",
        "additionalProperties": false,
        "required": ["timestamp"],
        "properties": {
            "timestamp": nullable_string_schema()
        }
    })
}

fn intake_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": [
            "housekeeping_complete",
            "ticket",
            "one_line_fingerprint",
            "initial_route",
            "intake_decision"
        ],
        "properties": {
            "housekeeping_complete": bool_schema(),
            "ticket": intake_ticket_schema(),
            "one_line_fingerprint": string_schema(),
            "ticket_summary": string_array_schema(),
            "context_pulls": {
                "type": "array",
                "items": context_pull_schema()
            },
            "initial_route": initial_route_schema(),
            "intake_decision": {
                "type": "string",
                "enum": [
                    "ready_for_evidence_preflight",
                    "known_issue",
                    "needs_clarification",
                    "cannot_proceed"
                ]
            }
        }
    })
}

fn intake_ticket_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": [
            "zendesk_id",
            "url",
            "status",
            "priority",
            "requester",
            "organization",
            "incident_window",
            "reported_symptom"
        ],
        "properties": {
            "zendesk_id": {
                "type": "integer",
                "minimum": 0
            },
            "url": string_schema(),
            "status": string_schema(),
            "priority": string_schema(),
            "tags": string_array_schema(),
            "requester": string_schema(),
            "organization": string_schema(),
            "site": nullable_string_schema(),
            "cnc": nullable_string_schema(),
            "region": nullable_string_schema(),
            "affected_stations": string_array_schema(),
            "affected_agents": string_array_schema(),
            "call_id": nullable_string_schema(),
            "incident_window": string_schema(),
            "reported_symptom": string_schema()
        }
    })
}

fn context_pull_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["pull", "result", "source"],
        "properties": {
            "pull": string_schema(),
            "result": string_schema(),
            "source": string_schema()
        }
    })
}

fn initial_route_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["hypothesis", "justification"],
        "properties": {
            "hypothesis": string_schema(),
            "justification": string_schema()
        }
    })
}

fn evidence_preflight_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": [
            "gathered",
            "decisive_evidence",
            "missing_or_non_decisive"
        ],
        "properties": {
            "gathered": {
                "type": "array",
                "items": gathered_evidence_schema()
            },
            "decisive_evidence": string_array_schema(),
            "missing_or_non_decisive": string_array_schema()
        }
    })
}

fn gathered_evidence_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": [
            "id",
            "evidence_type",
            "source",
            "time_window",
            "summary"
        ],
        "properties": {
            "id": string_schema(),
            "evidence_type": string_schema(),
            "source": string_schema(),
            "time_window": string_schema(),
            "summary": string_schema()
        }
    })
}

fn fork_packet_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": [
            "commitment",
            "evidence_summary",
            "missing_evidence",
            "related",
            "handoff"
        ],
        "properties": {
            "commitment": fork_commitment_schema(),
            "evidence_summary": string_array_schema(),
            "missing_evidence": string_array_schema(),
            "related": related_work_schema(),
            "handoff": handoff_schema()
        }
    })
}

fn fork_commitment_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": [
            "fork_letter",
            "confidence",
            "quoted_rubric_row",
            "rubric_class",
            "reasoning"
        ],
        "properties": {
            "fork_letter": {
                "type": "string",
                "enum": ["A", "B", "C", "D"]
            },
            "confidence": {
                "type": "string",
                "enum": ["low", "medium", "high"]
            },
            "quoted_rubric_row": string_schema(),
            "rubric_class": string_schema(),
            "reasoning": string_schema()
        }
    })
}

fn related_work_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["zendesk", "jira", "master", "cluster"],
        "properties": {
            "zendesk": {
                "type": "array",
                "items": {
                    "type": "integer",
                    "minimum": 0
                }
            },
            "jira": string_array_schema(),
            "master": {
                "type": ["integer", "null"],
                "minimum": 0
            },
            "cluster": nullable_string_schema()
        }
    })
}

fn handoff_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": [
            "engineering_jira_needed",
            "vendor_or_it_needed",
            "customer_note_needed",
            "internal_note_needed"
        ],
        "properties": {
            "engineering_jira_needed": handoff_item_schema(),
            "vendor_or_it_needed": handoff_item_schema(),
            "customer_note_needed": handoff_item_schema(),
            "internal_note_needed": handoff_item_schema()
        }
    })
}

fn handoff_item_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["needed", "reason"],
        "properties": {
            "needed": bool_schema(),
            "reason": string_schema()
        }
    })
}

fn drafts_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": [
            "customer_reply",
            "internal_zendesk_note",
            "jira_draft"
        ],
        "properties": {
            "customer_reply": string_schema(),
            "internal_zendesk_note": string_schema(),
            "jira_draft": {
                "type": ["object", "null"],
                "additionalProperties": false,
                "required": [
                    "title",
                    "description",
                    "repro_steps",
                    "project"
                ],
                "properties": {
                    "title": string_schema(),
                    "description": string_schema(),
                    "affected_component": nullable_string_schema(),
                    "suspected_area": nullable_string_schema(),
                    "repro_steps": string_array_schema(),
                    "project": string_schema()
                }
            }
        }
    })
}

fn string_schema() -> Value {
    json!({ "type": "string" })
}

fn nullable_string_schema() -> Value {
    json!({ "type": ["string", "null"] })
}

fn string_array_schema() -> Value {
    json!({
        "type": "array",
        "items": string_schema()
    })
}

fn bool_schema() -> Value {
    json!({ "type": "boolean" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_schema_requires_validator_top_level_fields() {
        let schema = structured_triage_output_schema();
        assert_eq!(schema.get("type").and_then(Value::as_str), Some("object"));
        assert_eq!(
            schema.get("$id").and_then(Value::as_str),
            Some("triage-cli/structured-triage/v1")
        );

        let required = required_fields(&schema);
        assert_eq!(
            required,
            vec![
                "intake",
                "evidence_preflight",
                "fork_packet",
                "drafts",
                "rubric_version"
            ]
        );
    }

    #[test]
    fn extraction_schemas_require_expected_fields() {
        let site = site_output_schema();
        assert_eq!(required_fields(&site), vec!["site_name"]);
        assert_eq!(
            site.pointer("/properties/site_name/type"),
            Some(&json!(["string", "null"]))
        );

        let anchor = anchor_output_schema();
        assert_eq!(required_fields(&anchor), vec!["timestamp"]);
        assert_eq!(
            anchor.pointer("/properties/timestamp/type"),
            Some(&json!(["string", "null"]))
        );
    }

    #[test]
    fn schemas_are_json_object_shapes() {
        for schema in [
            structured_triage_output_schema(),
            site_output_schema(),
            anchor_output_schema(),
        ] {
            assert_eq!(schema.get("type").and_then(Value::as_str), Some("object"));
            assert!(
                schema
                    .get("properties")
                    .and_then(Value::as_object)
                    .is_some(),
                "schema must expose object properties: {schema:?}"
            );

            let properties = schema
                .get("properties")
                .and_then(Value::as_object)
                .expect("properties object");
            for field in required_fields(&schema) {
                assert!(
                    properties.contains_key(field),
                    "required field {field} must have a property schema"
                );
            }
        }
    }

    fn required_fields(schema: &Value) -> Vec<&str> {
        schema
            .get("required")
            .and_then(Value::as_array)
            .expect("required array")
            .iter()
            .map(|v| v.as_str().expect("required field name"))
            .collect()
    }
}
