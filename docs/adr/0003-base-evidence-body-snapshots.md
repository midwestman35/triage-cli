# ADR 0003: Base-Evidence Body Snapshots (Manifest Schema v2)

**Status**: Accepted
**Date**: 2026-05-17
**Branch**: feat/interactive-investigation-v1

## Context

`BaseEvidenceManifest` (`.session/base-evidence-manifest.json`) preserves
the original investigation's evidence catalog for `/revise` to consume.
Schema v1 stored only `EvidenceItem` records — id, kind, label, source
pointer — without the underlying content bodies.

`/revise` therefore could not restore the actual signal (Datadog log
lines, file contents, paste bodies) that drove the initial fork. The
synthetic session built by `build_revise_session` could surface the
catalog as a list, but the LLM re-emission never saw the bodies.

## Decision

Bump the manifest schema to v2:

- New type `BaseEvidenceEntry { item: EvidenceItem, body: Option<String> }`,
  flattened on serialization so the wire format adds a single optional
  `body` field alongside the existing `EvidenceItem` fields.
- `BaseEvidenceManifest.evidence` becomes `Vec<BaseEvidenceEntry>`.
- The pipeline populates `body` per evidence kind (see `pipeline.rs::
  collect_base_evidence_entries`): pasted_note → paste text; local_file
  → extracted text; datadog_log_window → joined log lines; etc.
- Bodies are capped at `BODY_SNAPSHOT_CAP_BYTES` (256 KB) per entry —
  consistent with the per-zip-entry cap in `investigation.rs`.
- `EvidenceItem` itself is unchanged; `bundle.evidence_index` stays a
  pure catalog for LLM context. The body snapshot lives ONLY in the
  persisted manifest.

`build_revise_session` consumes the bodies by injecting each body as a
labeled paste into the synthetic session, so the structured pipeline
sees the same signal the original investigation did.

## Backward Compatibility

Old v1 manifests deserialize cleanly into v2's `BaseEvidenceEntry`:
`#[serde(flatten)]` reads the EvidenceItem fields, and `body` defaults
to `None` via `#[serde(default)]`. `build_revise_session` skips entries
with `body == None` — degrading gracefully to the catalog-only behavior
that v1 callers experienced before this change.

No on-disk migration is needed. Old manifests remain readable; new
writes are v2.

## Tradeoffs

- **Manifest size**: bodies are inlined as JSON strings. A heavy
  investigation can produce a multi-MB manifest. The per-entry cap
  bounds growth.
- **JSON-escape overhead**: large log windows escape to ~1.5× their
  raw size in JSON. Acceptable; we have other options (sidecar files)
  if this becomes painful.
- **Schema version is informational**, not enforced at read time —
  the reader trusts serde defaults to handle missing fields. This
  keeps the read path simple and supports incremental migration.
