# archive/

Frozen snapshots of code/data that left the live tree but is worth keeping
recoverable. Nothing in here is read by the Rust binary at runtime.

## Contents

### `python-source-YYYY-MM-DD.zip`

The original Python `triage_cli` package, plus tests, scripts, and
`pyproject.toml`, snapshotted on the date the Python → Rust port was
finalized. Contents at archive time:

- `triage_cli/` — full Python package (cli, models, pipeline, providers,
  inbox/tui, watcher, memory, redactor, …)
- `tests/` — Pytest suite + fixtures
- `scripts/build_cnc_map.py` — ported to Rust (`triage-cli-rs/src/build_map.rs`,
  output byte-identical to this script)
- `scripts/setup.py` — replaced by `triage-cli setup` subcommand
- `scripts/certify_readonly_my_queue.py` — **not yet ported to Rust**; the
  closest thing tracked in `REGRESSIONS.md` R11. Restore from this zip if you
  need to re-run the read-only Zendesk-boundary certification.
- `pyproject.toml` — Python package metadata.

### Restoring from the archive

```bash
unzip -d /tmp/triage-cli-python archive/python-source-YYYY-MM-DD.zip
# Then `pip install -e .` from the unpacked dir if you need a working venv.
```
