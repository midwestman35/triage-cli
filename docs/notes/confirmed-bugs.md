# Confirmed Bugs & Tech Debt

From adversarial review of `docs/2026-05-09-codebase-evaluation.md` against actual code.
Items marked ~~struck~~ were found to be inaccurate.

---

## Bugs (wrong output or broken contract)

### `no_llm` path re-reads files redundantly — `cli.py:269–271`

The `no_llm` branch iterates `local_files_evidence` (already-built `LocalFileEvidence` objects with
extracted text) and calls `_afl(session, lf.path)`, which re-opens and re-classifies each file
from disk. The already-extracted text is discarded and re-read.

Not user-visible today because the output is the same, but wastes I/O and will silently diverge if
a file changes between the initial ingest and the `no_llm` call.

**Fix:** Pass the existing evidence objects into the session rather than re-reading by path.

---

## Tech Debt (layering / convention violations)

### `no_llm` inline assembly — `cli.py:266–276`

Ten lines of `investigation.py` workflow (`create_session → add_local_file × N → add_pasted × N →
build_timeline → session_to_report`) are executed inline in `cli.py`. This is the wrong
abstraction layer.

**Fix:** Add `investigation.build_offline_report(ticket, local_files, pasted_logs) → TriageReport`
and call that single function from `cli.py`.

### Bare `print` in `interactive.py` — 12+ violations

CLAUDE.md: "No `print` outside `cli.py`, `render.py`, `pipeline.py`, and `watcher.py`."
`interactive.py` has bare `print(file=sys.stderr)` at lines 65, 141–148, 176–179, 188–189,
194–198, 207, 210, 234–241, 253.

**Fix:** Replace with `typer.echo(..., err=True)`.

### `interactive.py` imports private helpers from `investigation.py` — line 19

`_detect_file_type` and `_read_text_if_supported` are leading-underscore private functions
imported across the module boundary. Couples `interactive.py` to `investigation.py` internals.

**Fix:** Make them public (drop the underscore) or move them to a shared `_file_utils.py`.

---

## Testing Gaps

### `datadog.py` — zero dedicated tests

187-line module with query construction, response parsing, and retry logic. No `test_datadog.py`
exists. The `get_logs` method is the highest-risk untested code outside `zendesk.py`.

---

## Retracted Claims (evaluation was wrong)

### ~~Session recreation "bug" in the file loop — `cli.py:244–250`~~

The evaluation claimed evidence was lost because `session` was recreated each iteration.
In fact, `add_local_file` returns the `LocalFileEvidence` object directly; `extra_local.append()`
captures it every iteration. All files accumulate correctly. The sessions are throwaway factories,
which is a smell but not a bug.

### ~~`_run_pipeline` "verbatim duplicate"~~

The two `_run_pipeline` closures differ by three kwargs (`downloaded_attachments`, `local_files`,
`pasted_logs`) present only in the `investigate` version. They're parallel patterns, not verbatim
copies. Merging them requires parameterizing the evidence bundle — a real design decision, not a
simple dedup.
