# Pipeline v2 — Interactive Investigation Flow

**Date:** 2026-05-09
**Status:** Draft
**Branch:** `pipelinev2`

## Goal

Make `triage-cli investigate <id>` an interactive evidence-gathering flow that downloads Zendesk attachments, prompts the analyst to drop supplemental logs into a per-ticket workspace, and ends with a real LLM analysis. Today the command is deterministic (no LLM, no Datadog) and only knows about evidence supplied via `--file` / `--paste`. After this change, `investigate` becomes the default comprehensive command; `triage` stays as the headless single-shot path used by scripts, the watcher, and the inbox TUI.

## Decisions

| Question | Decision | Reason |
|---|---|---|
| What does `investigate` do at the end? | Calls the LLM via `pipeline.triage_one`, same as `triage` | Evidence collection without analysis is just file-shuffling; the value is the analysis informed by the bundle |
| Keep both `investigate` and `triage` CLI commands? | Yes — two CLI surfaces, one shared core | Watcher and inbox cannot prompt a human; they need the headless path |
| Where do downloaded files live? | `triage-notes/<id>/{attachments,local}/` | `triage-notes/` is already gitignored; co-location is one folder per ticket |
| Per-attachment download granularity? | All-or-nothing y/n with size cap | Common case is "yes all"; per-file picking is keystrokes for marginal gain |
| Drop-and-ready loop strictness? | Trust + summarize; empty enter = ready | Matches Unix muscle memory; clear summary covers the common errors |
| Upfront "internal notes exist" banner? | No — timeline already shows them | triage-cli is not a Zendesk replacement |
| Bundle MCP servers for Zendesk/Confluence? | Deferred — MCP is wrong tool for the current one-shot LLM call | Confluence MCP later is a legitimate path (see *Plausible follow-ups* below) |
| Per-attachment cap | 150 MB, env-overridable | Sized for real Carbyne event PDFs and log archives |
| Concurrency for downloads | Serial | 1–3 attachments per ticket; serial keeps stderr clean |
| Order of operations | Lazy site (evidence first, then site/anchor/Datadog/LLM) | Analyst sees what's there before the tool maps to a customer |

## Architecture

**No new top-level modules. One small new helper module; one extended core.**

```
triage_cli/
  cli.py             # investigate command rewritten; triage unchanged
  pipeline.py        # triage_one extended to accept optional evidence
  interactive.py     # NEW: download_attachments, prompt_drop_and_wait, summarize_workspace
  zendesk.py         # _attachments_from_raw preserves content_url; new download_attachment()
  models.py          # AttachmentEvidence keeps content_url; TriageBundle gains evidence fields
  investigation.py   # mostly retained for the --no-llm fallback path
```

### Command surface

| Command | Behavior |
|---|---|
| `triage-cli investigate <id>` | New interactive flow. Default. Prompts for downloads + drops, calls `triage_one` with full evidence bundle. |
| `triage-cli investigate <id> --no-llm` | Old deterministic flow preserved. Useful when offline or when LLM access is temporarily unavailable. |
| `triage-cli triage <id>` | Unchanged. Headless single-shot. Pipeable. |
| `triage-cli watch / inbox` | Unchanged. Both keep calling `pipeline.triage_one` directly with empty evidence fields. |

### `triage_cli/interactive.py`

Three functions. Pure where possible; I/O against terminal, filesystem, and the Zendesk client only.

| Function | Signature |
|---|---|
| `download_attachments` | `(ticket, zd_client, workspace) -> list[AttachmentEvidence]` |
| `prompt_drop_and_wait` | `(workspace) -> list[LocalFileEvidence]` |
| `summarize_workspace` | `(workspace, ingested, skipped) -> str` (stderr only) |

The Zendesk client is passed in (not constructed) so tests can inject a fake.

### Single LLM entry point

Both `cli.triage` and the new interactive `cli.investigate` end at the same `pipeline.triage_one` call. The interactive flow's only job is to enrich the `TriageBundle` before passing it. Watcher/inbox bundle has empty evidence fields; the LLM call is byte-identical to today.

## Data flow

```
[1]  Parse ticket id (extract.parse_ticket_id)
[2]  Fetch ticket via ZendeskClient
[3]  Stderr ticket header:
       ZD-44496 · requester · N attachments · M comments · created TS
[4]  If ticket has ≥1 attachment:
       List attachments (name, size); prompt "Download all? [Y/n]"
       Y → download_attachments() to triage-notes/<id>/attachments/
       N → AttachmentEvidence list stays metadata-only
[5]  Drop-and-ready loop:
       Print suggested types (Apex zips, SIP extracts, Datadog CSV)
       Read stdin; empty enter or "ready" → finish; "skip" → finish empty
       Scan local/, classify each file, print summary:
         Ingesting:  apex-station.log (8KB, log)
                     twilio-sip.txt (2KB, text)
         Skipping:   dump.pcap (50MB, binary)
       No second confirmation; proceed.
[6]  Resolve site (existing pipeline.resolve_site; interactive prompt on miss)
[7]  Anchor extraction (existing extract_anchor)
[8]  Datadog query (existing; --no-logs respected)
[9]  Build TriageBundle including downloaded_attachments, local_files, pasted_logs
       Pass to pipeline.triage_one
[10] Render markdown to stdout
[11] Save report.md and report.json in triage-notes/<id>/
```

### Stdout discipline

stdout is reserved for the rendered markdown report so output stays pipeable. All prompts, summaries, spinners, save-path notices go to stderr via `typer.echo(..., err=True)`. This matches the existing rule in CLAUDE.md.

### TTY guard

`investigate` requires a TTY. Without one, it aborts at the top with:

```
investigate requires an interactive terminal. Use 'triage' for headless runs.
```

This mirrors the existing inbox TTY guard.

## Data model changes

### `AttachmentEvidence` gains `content_url`

```python
class AttachmentEvidence(BaseModel):
    filename: str
    content_type: str | None = None
    size_bytes: int | None = None
    source: Literal["zendesk_attachment"] = "zendesk_attachment"
    local_path: Path | None = None         # set after download
    extracted_text: str | None = None      # set after text extraction
    content_url: str | None = None         # NEW: pre-signed Zendesk URL
```

`triage_cli/zendesk.py:260` currently drops the URL with the comment *"Map Zendesk attachment metadata without preserving downloadable URLs."* That comment is removed.

**Privacy:** `content_url` is excluded from the JSON-saved report. Render layer uses `model_dump(exclude={"content_url"})` when serializing to `triage-notes/<id>/report.json`. Pre-signed URLs are time-limited but writing them to disk is still an unnecessary leak vector. The field exists in memory only.

### `TriageBundle` gains three optional evidence fields

```python
class TriageBundle(BaseModel):
    ticket: Ticket
    site_entry: SiteEntry
    log_lines: list[LogLine] = Field(default_factory=list)
    log_truncated: bool = False
    anchor: datetime
    anchor_source: AnchorSource
    window_start: datetime
    window_end: datetime
    # NEW (default empty; headless triage path leaves them empty):
    downloaded_attachments: list[AttachmentEvidence] = Field(default_factory=list)
    local_files: list[LocalFileEvidence] = Field(default_factory=list)
    pasted_logs: list[PastedEvidence] = Field(default_factory=list)
```

### `as_user_message()` extension

A new `# Supplemental Evidence` section appended after `# Logs`. Per-file token policy:

| Type | Policy |
|---|---|
| Text / log / json under cap | Inline full content |
| Text / log / json over cap | Inline first `EVIDENCE_HEAD_BYTES` + last `EVIDENCE_TAIL_BYTES`, separated by `[truncated N bytes]` |
| Binary / unknown | Metadata only (filename, size, type). No bytes. |

Defaults: `EVIDENCE_HEAD_BYTES = 32_000`, `EVIDENCE_TAIL_BYTES = 8_000` (~10K tokens per file). Both env-overridable. Per-file caps; no aggregate cap.

Head + tail truncation beats first-N for log files because incidents tend to be visible at both ends — boot sequence at the top, the actual error near the bottom.

### `investigation.py` disposition

Most of it stays. `add_local_file`, `add_pasted_evidence`, `build_timeline`, `assess_session`, `session_to_report` are reused by the `--no-llm` deterministic fallback and are reachable from `interactive.py`. `_attachments_from_comment` no longer needs to discover URLs (they come pre-populated). `session_to_report`'s `unknowns` list drops the line about attachment download being future work.

## Workspace lifecycle

### Layout

```
triage-notes/                                  # already gitignored
  44496/                                       # created on first investigate run
    44496-2026-05-09T20-15-32.md               # the rendered note
    44496-2026-05-09T20-15-32.json             # structured TriageReport (content_url stripped)
    attachments/                               # downloaded from Zendesk
      log.txt
      Carbyne Event - 11061506.pdf
      .download-manifest.json                  # {filename: {size, sha256, downloaded_at}}
    local/                                     # analyst-dropped; tool never writes here
      apex-station.zip
      twilio-sip.txt
```

### Operations

| Event | Behavior |
|---|---|
| Workspace dir doesn't exist | `mkdir -p triage-notes/<id>/{attachments,local}` before the attachment prompt (the dir must exist to be a download target) |
| Re-run on same ticket | Reuse existing dir. New note + json files (timestamp suffix); old ones preserved. |
| Attachment exists in `.download-manifest.json` with matching size | Skip download, reuse local file |
| Attachment exists on disk but not in manifest | Skip download, log warning, treat existing file as authoritative |
| Attachment exists with mismatched size in manifest | Re-download with `.N` suffix (`log.txt.2`), update manifest |
| User dropped files in `local/` between runs | Re-ingested every run; no dedup. The user owns this dir. |
| User cleared `local/` between runs | Empty ingest; tool prints "no local evidence" |

### Lifecycle policies

- **No auto-cleanup.** Disk is cheap; ticket workspaces are useful as a forensic record. Manual cleanup via `rm -rf triage-notes/<id>/`.
- **No `triage-cli clean` command.** YAGNI; shell suffices.
- **No concurrency lock.** Single-user tool; running `investigate <id>` twice in parallel is undefined behavior. Not worth a lockfile for v1.
- **Default umask.** Workspace dirs created with mode 0o755. The whole `triage-notes/` tree is gitignored.

### `.download-manifest.json`

Carries `{filename: {size, sha256, downloaded_at}}` and nothing else. Sha256 is computed during the download stream (single pass). Lets re-runs distinguish "same file, skip" from "different file, suffix" without a second Zendesk roundtrip.

## Zendesk attachment download

### Method on `ZendeskClient`

```python
def download_attachment(
    self,
    url: str,
    dest_path: Path,
    *,
    max_bytes: int = 150 * 1024 * 1024,  # 150 MB
) -> tuple[int, str]:
    """Stream-download a Zendesk attachment to disk. Returns (bytes_written, sha256_hex).

    Aborts mid-stream and unlinks the partial file if max_bytes is exceeded.
    Reuses the existing authenticated client.
    """
```

Streaming with httpx (`client.stream("GET", url)`) is required. Reading the whole body into memory before checking size would be a footgun on the analyst's laptop given typical Carbyne event PDFs and log archives.

### Size enforcement

- `TRIAGE_MAX_ATTACHMENT_BYTES` env var sets the cap (default 150 MB).
- Two enforcement points:
  1. **Pre-flight:** if Zendesk's `size_bytes` metadata is over the cap, skip without GET.
  2. **Mid-stream:** if `Content-Length` was unset or the stream exceeds expected size, abort the write, `path.unlink()` the partial, log the skip.
- Per-attachment cap; no aggregate.

### Failure / retry policy

| Failure | Behavior |
|---|---|
| 401 / 403 | Auth boundary. Abort entire investigate run. |
| 404 on URL | Probably an expired pre-signed URL. Log warning, skip this attachment, continue with others. |
| 429 | Sleep `Retry-After` (or 30s default), retry once. Second 429 = skip. |
| Network timeout / read error | Retry once with fresh stream. Second failure = skip. |
| Partial write (max_bytes hit) | `unlink()` the partial; record as "skipped: too large". |

Auth errors abort because they affect every attachment. Per-attachment errors skip and continue — partial evidence is still useful.

### Concurrency: serial

Downloads run one at a time. Common case is 1–3 attachments per ticket; parallelism saves <1s. Serial preserves clean stderr ("Downloading log.txt... done.") and avoids burst rate limits.

### Pre-signed URL handling

Zendesk's `content_url` may 302 to S3. We pass `follow_redirects=True`. Pre-signed S3 URLs reject the basic-auth header; httpx strips auth on cross-origin redirects by default since 0.27 — which is the behavior we want. Worth a unit test pinning that.

## Edge cases & error handling

### Ticket-shape variations

| Condition | Behavior |
|---|---|
| Zero attachments | Skip attachment prompt; stderr note; go to drop loop. |
| Attachment metadata missing `size_bytes` | Print `(unknown size)`; download anyway, enforce mid-stream cap. |
| Ticket fetch returns 404 | Existing behavior. Abort. |
| Zero comments | Drop loop still runs; LLM call proceeds with sparse context. |

### Interactive flow

| Condition | Behavior |
|---|---|
| stdin not a TTY | Abort at the top with the helpful message pointing at `triage`. |
| Ctrl-C during prompt | SIGINT. Workspace preserved; partial download (if any) unlinked via signal handler. Exit 130. |
| Ctrl-C mid-download | httpx stream interrupted; `attachments/<file>.partial` unlinked in `finally:`. Exit 130. Workspace and prior downloads preserved. |
| User types `skip` / `quit` / `q` / `abort` at drop prompt | Skip drop loop; continue with whatever was already downloaded + flag-supplied evidence. |
| `n` to attachments **and** `skip` to drop | Run with ticket-only context. Equivalent to `triage <id>` plus the workspace dir. |
| `--file` / `--paste` flags supplied | Added before drop prompt runs. Drop prompt asks for *additional* evidence. Flags are additive. |

### Site / anchor / Datadog

| Condition | Behavior |
|---|---|
| Site can't be resolved | Existing interactive prompt. Blank entry → abort. |
| Datadog query fails | Existing prompt: "Continue without Datadog logs? [y/N]". Evidence is preserved in the bundle even with no logs. |
| LLM call fails after retry | Raise. Workspace preserved; analyst can re-run or fall back to `--no-llm`. |
| LLM returns invalid JSON twice | Existing behavior: raise. Evidence preserved on disk. |

### Filesystem failures

| Condition | Behavior |
|---|---|
| Workspace dir creation fails | Abort with the OSError message. No partial workspace. |
| Disk full mid-download | OSError; `.partial` unlinked; offer skip-and-continue (other attachments may still fit). |
| `local/` deleted between prompt and "ready" | Treated as empty; print "local/ is empty or missing; proceeding with no local evidence." |
| Manifest file corrupt | Print warning, treat as missing, rebuild on next download. Don't abort. |

### Watcher and inbox interactions

Watcher and inbox are unchanged. Important consequence: tickets the watcher auto-triages do **not** get attachment downloads. If the analyst later runs `investigate <id>` on the same ticket, the workspace is created fresh and they can download/drop normally. The two paths don't conflict because they write different files:

```
triage-notes/44496-2026-05-09T18-00.md     ← watcher-generated (existing convention)
triage-notes/44496/44496-2026-05-09T20-15.md  ← investigate-generated (new)
triage-notes/44496/attachments/...
```

No migration needed.

## Testing strategy

CLAUDE.md is firm: no network-touching tests. Everything is stubbed/monkeypatched. This drives the design — `interactive.py` functions take the Zendesk client as a parameter so tests inject a fake.

### New test files

| File | What it covers |
|---|---|
| `tests/test_interactive.py` | Drop-and-ready loop, attachment download orchestration, workspace summary |
| `tests/test_attachment_download.py` | `ZendeskClient.download_attachment` happy path, size cap, partial-unlink, retry |
| `tests/test_workspace.py` | Manifest read/write, idempotent re-run, collision suffix rule |

### Existing test files extended

| File | What changes |
|---|---|
| `tests/test_zendesk.py` | `_attachments_from_raw` preserves `content_url` |
| `tests/test_models.py` | `TriageBundle.as_user_message` evidence rendering; head+tail truncation; JSON serialize excludes `content_url` |
| `tests/test_cli.py` | New `investigate` integration tests with TTY mock, stdin mock, mocked Zendesk + Datadog + LLM |
| `tests/test_pipeline.py` | Regression: `triage_one` works with empty evidence fields (watcher path) |

### Required scenarios for merge

1. **Watcher regression** — `pipeline.triage_one(...)` called with empty evidence fields produces byte-identical output to current behavior. **Write this first**, TDD-style. Every other change is layered on it.
2. **Attachment happy path** — fake Zendesk returns 2 attachments; `investigate` downloads both; workspace contains both files + manifest.
3. **Attachment cap** — fake stream returns 200 MB; aborts mid-stream; partial unlinked; summary records skip.
4. **Re-run skip** — second call with same manifest → no GET issued, file reused.
5. **Re-run collision** — second call where Zendesk reports a different size for the same filename → `.2` suffix applied.
6. **Drop loop, empty** — user types ready with nothing in `local/` → empty `local_files`; LLM call proceeds.
7. **Drop loop, mixed** — `local/` has 2 text files + 1 binary → text ingested with truncation; binary listed metadata-only.
8. **TTY-required guard** — non-TTY stdin → exits with the helpful error pointing at `triage`.
9. **content_url leak guard** — saved `report.json` parsed back; `content_url` field is absent.
10. **`--no-llm` fallback** — interactive flow runs to completion but the deterministic path produces the report.

### Certification script

`scripts/certify_readonly_my_queue.py` is updated:
- Adding attachment download is still read-only (GET only, no PUT/POST/PATCH/DELETE).
- The script gains a check: assert no `httpx.Client` method other than `get()` is called during a full investigate run. If we ever introduce a write, this check trips.

### Explicitly NOT tested

- Real Zendesk attachment download. Network tests are forbidden.
- LLM token budget under real load. We test the truncation algorithm; we don't test that the model accepts the resulting prompt size.
- TTY behavior across terminals. We mock `sys.stdin.isatty()`.
- Concurrent investigate runs on the same ticket. Out of scope.

## Out of scope / parking lot

### Deferred from this brainstorm by decision

| Item | Why parked | Trigger to revisit |
|---|---|---|
| Zendesk MCP server bundling | Wrong tool today (LLM call is one-shot; MCP needs a multi-turn loop). | If we ever switch to a multi-turn agent flow. |
| Upfront "internal notes exist" banner | triage-cli is not a Zendesk replacement; timeline already shows internal comments. | If the signal proves load-bearing in real use. |

### YAGNI / scope discipline

| Item | Why parked |
|---|---|
| `triage-cli clean` workspace cleanup command | `rm -rf triage-notes/<id>/` is one shell command. |
| TTL-based auto-cleanup | No clear lifetime policy from real use yet. |
| Concurrent-run lockfile | Single-user tool; collisions are theoretical. |
| Aggregate token budget across all evidence files | Per-file cap is simpler. |
| Per-attachment y/n download | Decided all-or-nothing; per-file picking is keystrokes for marginal gain. |
| Streaming LLM response to stdout incrementally | Not a UX bottleneck. |

### Plausible follow-ups

| Item | Reason it's plausibly worth doing later |
|---|---|
| Watcher gains attachment download | Watcher fires on every new ticket; eager download could be hundreds of files/day. Need a download policy first. |
| Inbox TUI gains "investigate" action | TUI already displays tickets; a key binding to launch the interactive flow on the selected ticket would be natural. |
| Site / anchor extraction reads downloaded logs | Could disambiguate edge cases. Cost: more tokens per call, and prompt engineering. |
| Two-stage flow (`investigate --collect`, then `investigate --analyze`) | Useful for context-switching mid-investigation. Adds state surface. |
| Pre-supplied evidence via YAML config | Useful for replay/debugging. `--file`/`--paste` cover the common case. |
| **Confluence MCP server (single shared key)** | Per-user Confluence API keys are not realistic. A packaged MCP server with one shared credential — used by analysts in their own Claude Code sessions, **not by triage-cli's one-shot LLM call** — would let them cross-reference runbooks during manual investigation. Different shape from the deferred Zendesk MCP: triage-cli already has its own Zendesk client and a one-shot LLM call that doesn't benefit from MCP. The Confluence MCP lives outside triage-cli entirely; triage-cli stays Confluence-blind in process. |

### Explicitly NOT a follow-up

| Item | Why we should never do this |
|---|---|
| Python `confluence.py` module embedded in triage-cli | CLAUDE.md: *"There is no `confluence.py` by design."* The boundary is about what runs **in-process** — not whether Confluence is reachable from the analyst's tools at all. An out-of-process MCP server with shared credentials is a separate, legitimate path. |
| Triage-cli writing back to Zendesk | CLAUDE.md: *"v1 is terminal-only; if anything ever posts back to Zendesk, this assumption must be revisited."* Read-only is load-bearing. |

## Migration notes

- No state-file format change. Watcher state shape is unchanged.
- No CLI flag removals. `--file` / `--paste` continue to work and are additive to the new prompts.
- No data migration. `triage-notes/<id>/` is created lazily; tickets that have only a flat `triage-notes/<id>-<ts>.md` file from the watcher coexist with the new layout without clashing.
- `--no-llm` is a new flag. Default behavior of `investigate` changes from deterministic to LLM-backed.
