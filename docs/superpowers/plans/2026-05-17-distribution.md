# Distribution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `triage-cli` to Windows-first NOC analysts via a one-line PowerShell install (mirrored by `curl|sh` on Mac/Linux), backed by a GitHub Actions release pipeline, with the binary decoupled from cwd via a `$TRIAGE_HOME` data directory.

**Architecture:** Five independent components in a fixed land order — (1) Windows compat fixes, (2) `$TRIAGE_HOME` path resolution, (3) GH Actions release workflow, (4) Install scripts, (5) Version-aware doctor + atomic update behavior. Each component is self-contained, individually mergeable, and reversible. Backwards compatibility for current analysts is preserved by a "cwd-looks-like-a-repo" fallback in the path resolver — existing workflows feel zero churn.

**Tech Stack:** Rust 1.95 (existing crate). New deps: `arboard = "3"`, `similar = "2"`, `shlex = "1"`. Existing deps used: `dirs`, `reqwest`, `serde_json`. PowerShell 5.1+ (Windows). POSIX `sh` (Mac/Linux). GitHub Actions.

**Reference spec:** `docs/superpowers/specs/2026-05-17-distribution-design.md`

---

## Component 2 — Windows compat fixes

This component lands FIRST. The Windows CI gate added at the end is what keeps everything else from rotting. All other work depends on the build being green on Windows.

### Task 1: Add `arboard`, `similar`, `shlex` to Cargo.toml

**Files:**
- Modify: `triage-cli-rs/Cargo.toml`

- [ ] **Step 1: Add the three new dependencies**

Open `triage-cli-rs/Cargo.toml`. Locate the `[dependencies]` block. Inside the "Filesystem + utilities" group (between `fs2 = "0.4"` and `zip = { ... }`), insert:

```toml
# Cross-platform helpers (Windows compat: clipboard, diff, arg-splitting)
arboard = "3"
similar = "2"
shlex = "1"
```

- [ ] **Step 2: Verify the manifest still parses**

Run: `cd triage-cli-rs && cargo check --no-default-features`
Expected: compile succeeds (may take a few minutes to fetch the new crates on first run). No errors. Warnings about unused deps are fine — they get used in subsequent tasks.

- [ ] **Step 3: Commit**

```bash
git add triage-cli-rs/Cargo.toml triage-cli-rs/Cargo.lock
git commit -m "deps: add arboard, similar, shlex for Windows compat"
```

---

### Task 2: `USER` → `USERNAME` env-var fallback

**Files:**
- Modify: `triage-cli-rs/src/pipeline.rs:505-520` (`current_owner`)
- Modify: `triage-cli-rs/src/tui/inbox.rs:1970-1974` (second `USER` read)
- Test: `triage-cli-rs/src/pipeline.rs` (inline `#[cfg(test)]`)

- [ ] **Step 1: Write the failing test**

In `triage-cli-rs/src/pipeline.rs`, find the existing test module (search for `#[cfg(test)]\nmod tests` near the bottom of the file). Add the following test inside it (or in a new sibling test module if there isn't one):

```rust
#[test]
fn current_owner_falls_back_to_username_when_user_unset() {
    // Save and clear both vars so we test in a known state.
    let prev_user = std::env::var("USER").ok();
    let prev_username = std::env::var("USERNAME").ok();
    let prev_triage_owner = std::env::var("TRIAGE_OWNER").ok();

    std::env::remove_var("USER");
    std::env::remove_var("TRIAGE_OWNER");
    std::env::set_var("USERNAME", "alice");

    assert_eq!(current_owner(), "alice");

    // Restore.
    match prev_user {
        Some(v) => std::env::set_var("USER", v),
        None => std::env::remove_var("USER"),
    }
    match prev_username {
        Some(v) => std::env::set_var("USERNAME", v),
        None => std::env::remove_var("USERNAME"),
    }
    match prev_triage_owner {
        Some(v) => std::env::set_var("TRIAGE_OWNER", v),
        None => std::env::remove_var("TRIAGE_OWNER"),
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd triage-cli-rs && cargo test --lib current_owner_falls_back_to_username_when_user_unset -- --test-threads=1`
Expected: FAIL — `assertion failed: left: "unknown", right: "alice"`. The fallback to `USERNAME` doesn't exist yet, so on Windows-style env state the function returns `"unknown"`.

- [ ] **Step 3: Update `current_owner` to fall back to `USERNAME`**

In `triage-cli-rs/src/pipeline.rs`, replace the existing `current_owner` function (around line 508-520) with:

```rust
/// The current analyst's identifier for `STATE.md`. Falls back through
/// `TRIAGE_OWNER` → `USER` (unix) → `USERNAME` (Windows) → "unknown" so the
/// soft-lock has a useful value even in headless / CI environments and on
/// Windows where `$USER` does not exist.
fn current_owner() -> String {
    if let Ok(v) = std::env::var("TRIAGE_OWNER") {
        if !v.trim().is_empty() {
            return v;
        }
    }
    if let Ok(v) = std::env::var("USER") {
        if !v.trim().is_empty() {
            return v;
        }
    }
    if let Ok(v) = std::env::var("USERNAME") {
        if !v.trim().is_empty() {
            return v;
        }
    }
    "unknown".into()
}
```

- [ ] **Step 4: Update the second call site in `tui/inbox.rs`**

In `triage-cli-rs/src/tui/inbox.rs`, find the existing `USER` env-var read around line 1972 (search for `std::env::var("USER")`). Update the call to also try `USERNAME`. The existing code is a chained `.or_else()` after a `TRIAGE_OWNER` read — extend it:

```rust
.or_else(|_| std::env::var("USER"))
.or_else(|_| std::env::var("USERNAME"))
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cd triage-cli-rs && cargo test --lib current_owner_falls_back_to_username_when_user_unset -- --test-threads=1`
Expected: PASS.

- [ ] **Step 6: Run the full pipeline test module to confirm no regressions**

Run: `cd triage-cli-rs && cargo test --lib pipeline::`
Expected: all existing tests still pass.

- [ ] **Step 7: Commit**

```bash
git add triage-cli-rs/src/pipeline.rs triage-cli-rs/src/tui/inbox.rs
git commit -m "feat(compat): fall back to USERNAME after USER for Windows"
```

---

### Task 3: Replace clipboard cascade with `arboard`

**Files:**
- Modify: `triage-cli-rs/src/tui/inbox.rs:686-697` (warning string), `1396-1421` (`copy_to_clipboard` body)

- [ ] **Step 1: Read the current `copy_to_clipboard` block**

Run: `sed -n '1390,1425p' triage-cli-rs/src/tui/inbox.rs`

Familiarize yourself with the existing cascade — it tries `pbcopy` → `wl-copy` → `xclip` via subprocess. The signature is `fn copy_to_clipboard(text: &str) -> bool`.

- [ ] **Step 2: Replace the function body**

In `triage-cli-rs/src/tui/inbox.rs`, replace the entire `copy_to_clipboard` function (the `fn copy_to_clipboard(text: &str) -> bool { ... }` block, approximately lines 1396-1425) with:

```rust
/// Copy `text` to the OS clipboard via the `arboard` crate. Returns `true`
/// on success. `arboard` handles platform differences internally (uses the
/// native clipboard API on Windows/macOS/Linux X11+Wayland) and is
/// synchronous — the call returns only after the OS has accepted the text,
/// removing the wl-copy fork-and-detach race we used to have on Wayland.
fn copy_to_clipboard(text: &str) -> bool {
    use arboard::Clipboard;
    match Clipboard::new().and_then(|mut c| c.set_text(text.to_owned())) {
        Ok(()) => true,
        Err(_) => false,
    }
}
```

- [ ] **Step 3: Update the user-visible failure message**

In `triage-cli-rs/src/tui/inbox.rs`, find the warning text around line 690-694 that says `"No clipboard tool found (install pbcopy/wl-copy/xclip)"`. Replace with:

```rust
"Clipboard not available on this system"
```

(That's the message the inbox surfaces to the user when `copy_to_clipboard` returns `false`. Keep the surrounding code structure exactly the same — just swap the message string.)

- [ ] **Step 4: Build to confirm it compiles**

Run: `cd triage-cli-rs && cargo build --release`
Expected: succeeds. The build may pull `arboard` for the first time.

- [ ] **Step 5: Run inbox tests**

Run: `cd triage-cli-rs && cargo test --lib tui::inbox::`
Expected: all existing tests pass. `arboard` itself has no test that needs a real clipboard in CI — the function is exercised by code paths that go through user interaction.

- [ ] **Step 6: Commit**

```bash
git add triage-cli-rs/src/tui/inbox.rs
git commit -m "refactor(tui): use arboard for cross-platform clipboard"
```

---

### Task 4: Replace `diff -u` shell-out with `similar`

**Files:**
- Modify: `triage-cli-rs/src/cli.rs:983-1000` (the `Command::new("diff")` fallback block)

- [ ] **Step 1: Read the current diff fallback block**

Run: `sed -n '980,1010p' triage-cli-rs/src/cli.rs`

Understand the surrounding context: this is the "no `$DIFF_VIEWER` set, fall back to `diff -u`" branch of a soft-lock conflict display. It captures `diff` output and prints to stderr; exit codes ≥ 2 mean a real diff failure.

- [ ] **Step 2: Replace the `Command::new("diff")` block**

In `triage-cli-rs/src/cli.rs`, find the fallback that begins with `// Fallback: `/usr/bin/diff -u`, captured and printed to stderr.` (around line 983). Replace the entire fallback block (from that comment through the closing brace of the `if let Some(code)` handling) with:

```rust
    // Fallback: in-process unified diff via the `similar` crate. This used
    // to shell out to `/usr/bin/diff -u`, which doesn't exist on Windows.
    let existing_bytes = std::fs::read_to_string(existing_path)?;
    let new_bytes = std::fs::read_to_string(new_path)?;
    let diff = similar::TextDiff::from_lines(&existing_bytes, &new_bytes)
        .unified_diff()
        .header("STATE.md (existing)", "STATE.md (new)")
        .to_string();
    eprintln!("{}", diff);
    Ok(())
```

(Note the exact whitespace: this block sits inside the same enclosing function as the `if let Ok(viewer)` block above it. Match the indentation of the original `Command::new` block.)

- [ ] **Step 3: Build to confirm it compiles**

Run: `cd triage-cli-rs && cargo build --release`
Expected: succeeds. Any error about unused imports → remove the now-unused `Command` import at the top of the file (only if no other `Command::new` call remains in this function; do not remove if other callers still need it).

- [ ] **Step 4: Run cli tests**

Run: `cd triage-cli-rs && cargo test --lib cli::`
Expected: all existing tests pass. If any test was specifically asserting `diff -u` output text format, it may need updating to match `similar`'s output — that crate produces standard unified-diff hunks but headers differ slightly.

- [ ] **Step 5: Commit**

```bash
git add triage-cli-rs/src/cli.rs
git commit -m "refactor(diff): use similar crate instead of /usr/bin/diff"
```

---

### Task 5: Replace `sh -c $DIFF_VIEWER` with `shlex` arg-splitting

**Files:**
- Modify: `triage-cli-rs/src/cli.rs:958-981` (the `$DIFF_VIEWER` invocation block)

- [ ] **Step 1: Read the current `$DIFF_VIEWER` block**

Run: `sed -n '955,985p' triage-cli-rs/src/cli.rs`

Note the existing shape: it reads `$DIFF_VIEWER`, falls into a `Command::new("sh").arg("-c").arg(format!(...))` invocation that interpolates `shell_escape`-ed file paths.

- [ ] **Step 2: Replace the block with portable arg-splitting**

In `triage-cli-rs/src/cli.rs`, replace the `Command::new("sh")` block (from `let status = Command::new("sh")` through the trailing `return Ok(());`) with:

```rust
            // Split the user's $DIFF_VIEWER on shell-style word boundaries.
            // `shlex::split` handles single/double-quoted args and escapes
            // exactly like POSIX `sh` would, but cross-platform and without
            // ever spawning a real shell. The file-path args are passed via
            // `Command::args`, so they never go through a parser at all.
            let parts = shlex::split(trimmed).ok_or_else(|| {
                std::io::Error::other("DIFF_VIEWER has unbalanced quoting")
            })?;
            let (cmd, args) = parts.split_first().ok_or_else(|| {
                std::io::Error::other("DIFF_VIEWER is empty after parsing")
            })?;
            let status = Command::new(cmd)
                .args(args)
                .arg(existing_path)
                .arg(new_path)
                .status()?;
            if !status.success() {
                eprintln!(
                    "{}: $DIFF_VIEWER exited with status {}",
                    "warning".yellow().bold(),
                    status
                );
            }
            return Ok(());
```

(Note: this drops the `shell_escape` helper call if it was only used here. If `shell_escape` is unused after this change, also remove the now-dead helper function.)

- [ ] **Step 3: Search for and remove any now-dead `shell_escape`**

Run: `grep -n 'shell_escape' triage-cli-rs/src/cli.rs`
Expected: only the function definition remains (no other call sites). If true, delete the `fn shell_escape` definition.

If the grep shows other call sites, leave the function in place.

- [ ] **Step 4: Build**

Run: `cd triage-cli-rs && cargo build --release`
Expected: succeeds with no warnings about unused functions (if you removed `shell_escape`).

- [ ] **Step 5: Run cli tests**

Run: `cd triage-cli-rs && cargo test --lib cli::`
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add triage-cli-rs/src/cli.rs
git commit -m "refactor(diff): use shlex + Command::args instead of sh -c"
```

---

### Task 6: cfg(unix)-gate the codex shell-script test mod

**Files:**
- Modify: `triage-cli-rs/src/providers/codex.rs:240` (just adds one `#[cfg(unix)]` attr)

- [ ] **Step 1: Add the cfg gate**

In `triage-cli-rs/src/providers/codex.rs`, find line 240 (`#[cfg(test)]` immediately followed by `mod followup_tests {`). Add a `cfg(unix)` attribute on the same line as `cfg(test)`:

```rust
#[cfg(all(test, unix))]
mod followup_tests {
```

- [ ] **Step 2: Confirm test builds on the current platform (macOS/Linux)**

Run: `cd triage-cli-rs && cargo test --lib providers::codex::followup_tests --no-run`
Expected: succeeds. (`--no-run` builds without executing; we just want to confirm the cfg attribute parses correctly and the tests still compile on unix.)

- [ ] **Step 3: Confirm the production code still compiles in isolation**

Run: `cd triage-cli-rs && cargo build --release --lib`
Expected: succeeds.

- [ ] **Step 4: Commit**

```bash
git add triage-cli-rs/src/providers/codex.rs
git commit -m "test(codex): cfg(unix)-gate followup_tests for Windows builds"
```

---

### Task 7: Add `windows-latest` and `macos-latest` to existing CI matrix

This is the MOST IMPORTANT change in Component 2. It's what prevents Windows support from rotting.

**Files:**
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: Read the current ci.yml**

Run: `cat .github/workflows/ci.yml`

Note the current shape: single job `rust-checks` on `ubuntu-latest`, runs fmt-check / clippy / test in sequence.

- [ ] **Step 2: Convert to a matrix job**

Replace the entire `jobs:` block in `.github/workflows/ci.yml` with:

```yaml
jobs:
  rust-checks:
    name: rust-checks (${{ matrix.os }})
    runs-on: ${{ matrix.os }}
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-latest, windows-latest, macos-latest]
    steps:
      - name: Checkout
        uses: actions/checkout@v4

      - name: Install Rust toolchain (1.95.0)
        uses: dtolnay/rust-toolchain@stable
        with:
          toolchain: 1.95.0
          components: rustfmt, clippy

      - name: Cache cargo registry and target
        uses: Swatinem/rust-cache@v2
        with:
          workspaces: triage-cli-rs -> triage-cli-rs/target

      - name: cargo fmt --check
        working-directory: triage-cli-rs
        run: cargo fmt --all -- --check

      - name: cargo clippy
        working-directory: triage-cli-rs
        run: cargo clippy --all-targets -- -D warnings

      - name: cargo test
        working-directory: triage-cli-rs
        run: cargo test --all-targets --no-fail-fast
```

Notes on the change:
- `fail-fast: false` — when Windows fails, we still want to see Mac and Linux results to triage faster.
- The `name:` interpolates `${{ matrix.os }}` so PR status checks show per-OS results.
- Everything else (toolchain, cache, steps) is identical to the current single-OS job.

- [ ] **Step 3: Validate the YAML syntax**

Run: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml'))"`
Expected: no output (success). If `yaml` module unavailable, use `gh workflow view ci.yml` once it's pushed, or skip and let GitHub validate.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: run checks on windows-latest and macos-latest"
```

- [ ] **Step 5: Push and confirm CI passes on all three OSes**

```bash
git push
```

Then on GitHub Actions, watch the run. Three matrix legs must all pass green:
- `rust-checks (ubuntu-latest)` ✅
- `rust-checks (macos-latest)` ✅
- `rust-checks (windows-latest)` ✅

If Windows fails: this is the moment Tasks 1-6 get validated end-to-end. Common failure modes:
- Test code still references `pbcopy`/`xclip` → re-check Task 3.
- `diff -u` test still expects shell-out → re-check Task 4.
- `sh -c` test still expects shell behavior → re-check Task 5.

Fix any failures with follow-up commits before proceeding to Component 3.

---

## Component 3 — `$TRIAGE_HOME` path resolution

Self-contained refactor. Strictly additive: the "if cwd has `.env`, use cwd" branch keeps existing analysts unaffected.

### Task 8: Create `paths.rs` module with `triage_home()` resolver

**Files:**
- Create: `triage-cli-rs/src/paths.rs`
- Modify: `triage-cli-rs/src/lib.rs` (add `pub mod paths;`)

- [ ] **Step 1: Write the failing test**

Create `triage-cli-rs/src/paths.rs` with this content (test-only for now — the production code follows in Step 3):

```rust
//! Path resolution for the per-user data directory. Replaces the old
//! cwd-coupled file layout. Three-tier priority:
//!
//!   1. `$TRIAGE_HOME` if set and non-empty.
//!   2. Else, the current working directory if it "looks like a repo"
//!      (contains `.env` OR `apex-cnc-inventory.md`). Backwards-compat for
//!      analysts who still `cd` into a git checkout before running.
//!   3. Else, the platform-default per-user data dir via `dirs::data_local_dir()`:
//!      - Windows: `%LOCALAPPDATA%\triage-cli\`
//!      - macOS:   `~/Library/Application Support/triage-cli/`
//!      - Linux:   `${XDG_DATA_HOME:-~/.local/share}/triage-cli/`

use std::path::PathBuf;

pub const TRIAGE_HOME_ENV: &str = "TRIAGE_HOME";

pub fn triage_home() -> PathBuf {
    if let Ok(h) = std::env::var(TRIAGE_HOME_ENV) {
        if !h.trim().is_empty() {
            return PathBuf::from(h);
        }
    }
    if cwd_looks_like_repo() {
        if let Ok(cwd) = std::env::current_dir() {
            return cwd;
        }
    }
    platform_default_dir()
}

fn cwd_looks_like_repo() -> bool {
    let Ok(cwd) = std::env::current_dir() else {
        return false;
    };
    cwd.join(".env").exists() || cwd.join("apex-cnc-inventory.md").exists()
}

fn platform_default_dir() -> PathBuf {
    dirs::data_local_dir()
        .expect("OS provides a local data dir")
        .join("triage-cli")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Serialize tests that touch global env vars / cwd.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn triage_home_env_var_takes_priority() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev = std::env::var(TRIAGE_HOME_ENV).ok();
        std::env::set_var(TRIAGE_HOME_ENV, "/tmp/explicit-home");
        assert_eq!(triage_home(), PathBuf::from("/tmp/explicit-home"));
        match prev {
            Some(v) => std::env::set_var(TRIAGE_HOME_ENV, v),
            None => std::env::remove_var(TRIAGE_HOME_ENV),
        }
    }

    #[test]
    fn triage_home_empty_env_var_falls_through() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev = std::env::var(TRIAGE_HOME_ENV).ok();
        std::env::set_var(TRIAGE_HOME_ENV, "   ");
        // Should not return "   " — should fall through to either cwd or
        // the platform default. We just assert it's not the empty/whitespace
        // string.
        assert_ne!(triage_home(), PathBuf::from("   "));
        match prev {
            Some(v) => std::env::set_var(TRIAGE_HOME_ENV, v),
            None => std::env::remove_var(TRIAGE_HOME_ENV),
        }
    }

    #[test]
    fn platform_default_dir_ends_in_triage_cli() {
        let p = platform_default_dir();
        assert_eq!(p.file_name().and_then(|s| s.to_str()), Some("triage-cli"));
    }
}
```

- [ ] **Step 2: Wire the module into the lib**

In `triage-cli-rs/src/lib.rs`, add a `pub mod paths;` declaration. Find the existing `pub mod` block (look for `pub mod ticket_folder;` or similar) and add `pub mod paths;` in alphabetical order.

- [ ] **Step 3: Run the new tests to verify they pass**

Run: `cd triage-cli-rs && cargo test --lib paths::`
Expected: 3 tests pass.

- [ ] **Step 4: Commit**

```bash
git add triage-cli-rs/src/paths.rs triage-cli-rs/src/lib.rs
git commit -m "feat(paths): add triage_home() resolver with cwd-fallback"
```

---

### Task 9: Anchor `.env`, `MEMORY.md`, `apex-cnc-inventory.md`, `data/` under `triage_home()`

This is the bulk of the cwd-decoupling work. Every hardcoded path that was implicitly cwd-relative needs to be re-anchored to `paths::triage_home()`.

**Files:**
- Modify: `triage-cli-rs/src/memory.rs:36-37, 57-67` (MEMORY.md, memory.db)
- Modify: `triage-cli-rs/src/ticket_folder.rs:25-26, 74-78` (DEFAULT_TICKETS_ROOT)
- Modify: `triage-cli-rs/src/build_map.rs:28-30` (INVENTORY, MAP_OUT, GAPS_OUT)
- Modify: `triage-cli-rs/src/setup.rs:16` (ENV_PATH)
- Modify: `triage-cli-rs/src/cli.rs:299, 1015, 1041` (views.json, watcher-state)
- Modify: `triage-cli-rs/src/pipeline.rs:300` (cnc-map.json read)
- Modify: `triage-cli-rs/src/watcher.rs:346` (cnc-map.json read)
- Modify: `triage-cli-rs/src/tui/inbox.rs:1242` (cnc-map.json read)

- [ ] **Step 1: Update `memory.rs` path resolvers**

In `triage-cli-rs/src/memory.rs`, replace the existing `memory_md_path` and `memory_db_path` functions (lines 57-67) with:

```rust
fn memory_md_path() -> PathBuf {
    if let Ok(v) = std::env::var(MEMORY_MD_ENV) {
        return PathBuf::from(v);
    }
    crate::paths::triage_home().join(MEMORY_MD)
}

fn memory_db_path() -> PathBuf {
    if let Ok(v) = std::env::var(MEMORY_DB_ENV) {
        return PathBuf::from(v);
    }
    crate::paths::triage_home().join(MEMORY_DB)
}
```

`MEMORY_MD = "MEMORY.md"` and `MEMORY_DB = "data/memory.db"` constants at lines 36-37 stay unchanged — they're now interpreted as relative to `triage_home()`.

- [ ] **Step 2: Update `ticket_folder.rs::tickets_root`**

In `triage-cli-rs/src/ticket_folder.rs`, replace the existing `tickets_root` function (lines 74-78) with:

```rust
pub fn tickets_root() -> PathBuf {
    if let Ok(v) = std::env::var(TICKETS_ROOT_ENV) {
        return PathBuf::from(v);
    }
    crate::paths::triage_home().join("Tickets")
}
```

Remove the unused `DEFAULT_TICKETS_ROOT` constant at line 26 (or leave it as `pub const` for backwards-compat if it's referenced elsewhere — run `grep -rn DEFAULT_TICKETS_ROOT triage-cli-rs/src/` first to check).

- [ ] **Step 3: Update `build_map.rs` to resolve paths via `triage_home`**

In `triage-cli-rs/src/build_map.rs`, the existing code uses the bare constants `INVENTORY`, `MAP_OUT`, `GAPS_OUT` as relative paths. Find each `std::fs::read_to_string(INVENTORY)` / `std::fs::write(MAP_OUT, ...)` / `std::fs::write(GAPS_OUT, ...)` call and prefix each with `crate::paths::triage_home().join(...)`. Example transformation:

Before:
```rust
let inventory = std::fs::read_to_string(INVENTORY)?;
```
After:
```rust
let inventory_path = crate::paths::triage_home().join(INVENTORY);
let inventory = std::fs::read_to_string(&inventory_path)?;
```

Apply the same pattern at every read/write site for the three constants.

- [ ] **Step 4: Update `setup.rs` ENV_PATH**

In `triage-cli-rs/src/setup.rs`, the `ENV_PATH` constant at line 16 (`const ENV_PATH: &str = ".env";`) is used at lines 117, 120, 167, 168, and 171. Two of those (117, 168, 171) are diagnostic strings — leave them alone. Two are real file-path uses (120, 167) — replace each:

Line 120, replace:
```rust
let existing = read_env_file(Path::new(ENV_PATH));
```
with:
```rust
let env_path_buf = crate::paths::triage_home().join(ENV_PATH);
let existing = read_env_file(env_path_buf.as_path());
```

Line 167, replace:
```rust
if let Err(e) = write_env_file(Path::new(ENV_PATH), &next) {
```
with:
```rust
let env_path_buf = crate::paths::triage_home().join(ENV_PATH);
if let Err(e) = write_env_file(env_path_buf.as_path(), &next) {
```

(If the `env_path_buf` binding from line 120 is still in scope at line 167, reuse it instead of re-binding. Otherwise the two bindings can coexist with different scopes.)

- [ ] **Step 5: Update `cli.rs` watcher-state and views.json paths**

In `triage-cli-rs/src/cli.rs`:

Line 299, replace:
```rust
let views_path = PathBuf::from("data/views.json");
```
with:
```rust
let views_path = crate::paths::triage_home().join("data/views.json");
```

Lines 1015 and 1041, replace each occurrence of:
```rust
PathBuf::from(format!("data/watcher-state-{...}.json", ...))
```
with:
```rust
crate::paths::triage_home().join(format!("data/watcher-state-{...}.json", ...))
```

(Note: `PathBuf::from(format!(...))` is replaced by `triage_home().join(format!(...))` — the formatted string becomes the relative segment.)

- [ ] **Step 6: Update `pipeline.rs`, `watcher.rs`, `tui/inbox.rs` cnc-map.json reads**

Three call sites all hardcode `"data/cnc-map.json"`:

`triage-cli-rs/src/pipeline.rs:300` — replace `Path::new("data/cnc-map.json")` with the binding:
```rust
let sites_path_buf = crate::paths::triage_home().join("data/cnc-map.json");
let sites_path = sites_path_buf.as_path();
```

`triage-cli-rs/src/watcher.rs:346` — same pattern.

`triage-cli-rs/src/tui/inbox.rs:1242` — same pattern.

- [ ] **Step 7: Update `interactive.rs` workspace path**

In `triage-cli-rs/src/cli.rs:569`, the call site is:
```rust
let workspace = match interactive::ensure_workspace(Path::new("./triage-notes"), ticket.id) {
```

Replace with:
```rust
let scratch_root_buf = crate::paths::triage_home().join("scratch");
let workspace = match interactive::ensure_workspace(scratch_root_buf.as_path(), ticket.id) {
```

(`./triage-notes` → `$TRIAGE_HOME/scratch` per spec § Component 3.)

- [ ] **Step 8: Build and run all tests**

Run: `cd triage-cli-rs && cargo build --release && cargo test --lib`
Expected: build succeeds; tests pass. Likely-flaky tests that need attention:
- Any test that creates fixture files in cwd and expects them to be picked up — those tests need to either (a) set `$TRIAGE_HOME` to a tempdir, or (b) the existing test fixtures already use `TRIAGE_TICKETS_ROOT`/`TRIAGE_MEMORY_MD`/`TRIAGE_MEMORY_DB` overrides which keep working as before.

Fix any test failures by setting `TRIAGE_HOME` to a tempdir in the failing test (using the same `ENV_LOCK` pattern from Task 8 to serialize).

- [ ] **Step 9: Commit**

```bash
git add triage-cli-rs/src/memory.rs triage-cli-rs/src/ticket_folder.rs \
        triage-cli-rs/src/build_map.rs triage-cli-rs/src/setup.rs \
        triage-cli-rs/src/cli.rs triage-cli-rs/src/pipeline.rs \
        triage-cli-rs/src/watcher.rs triage-cli-rs/src/tui/inbox.rs
git commit -m "feat(paths): anchor data files under triage_home()"
```

---

### Task 10: Add `migrate-home` subcommand

**Files:**
- Modify: `triage-cli-rs/src/cli.rs` (clap subcommand definition + dispatch)
- Modify: `triage-cli-rs/src/paths.rs` (add `migrate_home_dest()` helper and a `migrate_home` action function)

- [ ] **Step 1: Add the helper that resolves the migration destination**

In `triage-cli-rs/src/paths.rs`, add (below the existing `triage_home` function):

```rust
/// Destination for `migrate-home`: respects `$TRIAGE_HOME` but never falls
/// back to cwd (the whole point of migrate-home is to LEAVE cwd).
pub fn migrate_home_dest() -> PathBuf {
    if let Ok(h) = std::env::var(TRIAGE_HOME_ENV) {
        if !h.trim().is_empty() {
            return PathBuf::from(h);
        }
    }
    platform_default_dir()
}
```

- [ ] **Step 2: Add the migrate-home action**

In `triage-cli-rs/src/paths.rs`, append:

```rust
/// Copy `.env`, `MEMORY.md`, `apex-cnc-inventory.md`, and `data/` from `src`
/// into `dest`. Refuses if `src == dest`. Does not delete from `src`.
/// Returns the destination path on success.
pub fn migrate_home(src: &std::path::Path, dest: &std::path::Path) -> std::io::Result<PathBuf> {
    if src == dest {
        return Err(std::io::Error::other(
            "migrate-home refuses: source and destination are the same",
        ));
    }
    std::fs::create_dir_all(dest)?;

    for name in [".env", "MEMORY.md", "apex-cnc-inventory.md"] {
        let from = src.join(name);
        if from.exists() {
            let to = dest.join(name);
            std::fs::copy(&from, &to)?;
        }
    }

    let data_src = src.join("data");
    if data_src.is_dir() {
        let data_dst = dest.join("data");
        std::fs::create_dir_all(&data_dst)?;
        for entry in std::fs::read_dir(&data_src)? {
            let entry = entry?;
            let from = entry.path();
            let to = data_dst.join(entry.file_name());
            if from.is_file() {
                std::fs::copy(&from, &to)?;
            }
        }
    }

    Ok(dest.to_path_buf())
}
```

- [ ] **Step 3: Write a test for `migrate_home`**

Append to the `#[cfg(test)] mod tests` block in `triage-cli-rs/src/paths.rs`:

```rust
#[test]
fn migrate_home_copies_files_and_data_dir() {
    let src = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    std::fs::write(src.path().join(".env"), "FOO=bar").unwrap();
    std::fs::write(src.path().join("MEMORY.md"), "memory").unwrap();
    std::fs::create_dir(src.path().join("data")).unwrap();
    std::fs::write(src.path().join("data").join("memory.db"), "db").unwrap();

    let returned = migrate_home(src.path(), dest.path()).unwrap();
    assert_eq!(returned, dest.path());

    assert_eq!(
        std::fs::read_to_string(dest.path().join(".env")).unwrap(),
        "FOO=bar"
    );
    assert_eq!(
        std::fs::read_to_string(dest.path().join("MEMORY.md")).unwrap(),
        "memory"
    );
    assert_eq!(
        std::fs::read_to_string(dest.path().join("data").join("memory.db")).unwrap(),
        "db"
    );
    // Source files preserved (not deleted).
    assert!(src.path().join(".env").exists());
}

#[test]
fn migrate_home_refuses_same_dir() {
    let dir = tempfile::tempdir().unwrap();
    let result = migrate_home(dir.path(), dir.path());
    assert!(result.is_err());
}
```

- [ ] **Step 4: Add the clap subcommand to `cli.rs`**

In `triage-cli-rs/src/cli.rs`, find the existing `enum Command` (or whatever the subcommand enum is named — search for `#[derive(Subcommand)]`). Add a new variant:

```rust
    /// Copy data files from the current directory into `$TRIAGE_HOME`
    /// (or the platform default). Use this once after upgrading from
    /// cwd-coupled installs.
    MigrateHome,
```

Then in the subcommand dispatch (the `match` block that handles each `Command::*` variant), add:

```rust
        Command::MigrateHome => {
            let src = std::env::current_dir()?;
            let dest = crate::paths::migrate_home_dest();
            match crate::paths::migrate_home(&src, &dest) {
                Ok(path) => {
                    eprintln!(
                        "Done. You can now run triage-cli from any directory."
                    );
                    eprintln!("Files migrated to: {}", path.display());
                    Ok(())
                }
                Err(e) => Err(anyhow::anyhow!("migrate-home failed: {e}")),
            }
        }
```

- [ ] **Step 5: Run all tests**

Run: `cd triage-cli-rs && cargo test --lib paths::`
Expected: 5 tests pass (the 3 from Task 8 plus the 2 new ones).

Then: `cd triage-cli-rs && cargo build --release`
Expected: succeeds, no warnings.

- [ ] **Step 6: Manual smoke test**

```bash
cd /tmp && mkdir migrate-test && cd migrate-test
echo "FOO=bar" > .env
echo "memory" > MEMORY.md
mkdir data && echo "db" > data/memory.db

TRIAGE_HOME=/tmp/migrate-dest /path/to/triage-cli migrate-home

ls -la /tmp/migrate-dest
```

Expected: `/tmp/migrate-dest/` contains `.env`, `MEMORY.md`, `data/memory.db`.

- [ ] **Step 7: Commit**

```bash
git add triage-cli-rs/src/paths.rs triage-cli-rs/src/cli.rs
git commit -m "feat(cli): add migrate-home subcommand"
```

---

### Task 11: Update `doctor` to print resolved paths and detect stale cnc-map

**Files:**
- Modify: `triage-cli-rs/src/setup.rs` (where `doctor` lives — check via `grep -n "fn run_doctor\|cmd_doctor"`)

- [ ] **Step 1: Locate the doctor function**

Run: `grep -rn 'cmd_doctor\|run_doctor\|fn doctor' triage-cli-rs/src/`
Expected output points at `setup.rs` (per CLAUDE.md, "Doctor + setup commands" lives in `setup.rs`).

- [ ] **Step 2: Add a "resolved paths" section to doctor output**

In `triage-cli-rs/src/setup.rs`, find the doctor function and add — near the start of its output, after any banner — the following block:

```rust
    let home = crate::paths::triage_home();
    eprintln!("triage_home: {}", home.display());
    eprintln!("  .env:                  {}", home.join(".env").display());
    eprintln!("  MEMORY.md:             {}", home.join("MEMORY.md").display());
    eprintln!("  apex-cnc-inventory.md: {}", home.join("apex-cnc-inventory.md").display());
    eprintln!("  data/cnc-map.json:     {}", home.join("data/cnc-map.json").display());
    eprintln!("  data/memory.db:        {}", home.join("data/memory.db").display());
    eprintln!("  Tickets/:              {}", crate::ticket_folder::tickets_root().display());
    eprintln!();
```

- [ ] **Step 3: Add stale-cnc-map detection**

In the same doctor function, near the end (after the existing critical checks complete), append:

```rust
    let inv = home.join("apex-cnc-inventory.md");
    let map = home.join("data/cnc-map.json");
    if let (Ok(inv_md), Ok(map_md)) = (
        std::fs::metadata(&inv),
        std::fs::metadata(&map),
    ) {
        if let (Ok(inv_mt), Ok(map_mt)) = (inv_md.modified(), map_md.modified()) {
            if inv_mt > map_mt {
                eprintln!(
                    "{}: cnc-map is stale; run triage-cli build-map to refresh.",
                    "warning".yellow().bold()
                );
            }
        }
    }
```

(Adjust the `owo_colors::OwoColorize` import if needed — search for how the existing doctor function colors output and match the style.)

- [ ] **Step 4: Build and smoke-test**

Run: `cd triage-cli-rs && cargo build --release`
Then: `./target/release/triage-cli doctor`
Expected: doctor's output starts with a `triage_home: ...` block listing resolved paths.

- [ ] **Step 5: Commit**

```bash
git add triage-cli-rs/src/setup.rs
git commit -m "feat(doctor): show resolved paths and warn on stale cnc-map"
```

---

## Component 1 — GitHub Actions release pipeline

### Task 12: Create release.yml with cross-platform build matrix

**Files:**
- Create: `.github/workflows/release.yml`

- [ ] **Step 1: Create the release workflow**

Create `.github/workflows/release.yml` with this exact content:

```yaml
name: release

on:
  push:
    tags:
      - 'v*'
  workflow_dispatch:
    inputs:
      tag:
        description: 'Tag to build (e.g., v0.2.0)'
        required: true

permissions:
  contents: write  # required to create a GitHub Release

env:
  RUSTFLAGS: "-D warnings"
  CARGO_TERM_COLOR: always

jobs:
  build:
    name: build (${{ matrix.target }})
    runs-on: ${{ matrix.runner }}
    strategy:
      fail-fast: false
      matrix:
        include:
          - runner: windows-latest
            target: x86_64-pc-windows-msvc
            archive: triage-cli-x86_64-windows.zip
            binary: triage-cli.exe
          - runner: macos-14
            target: aarch64-apple-darwin
            archive: triage-cli-aarch64-macos.tar.gz
            binary: triage-cli
          - runner: macos-13
            target: x86_64-apple-darwin
            archive: triage-cli-x86_64-macos.tar.gz
            binary: triage-cli
          - runner: ubuntu-latest
            target: x86_64-unknown-linux-gnu
            archive: triage-cli-x86_64-linux.tar.gz
            binary: triage-cli
    steps:
      - name: Checkout
        uses: actions/checkout@v4

      - name: Install Rust toolchain (1.95.0)
        uses: dtolnay/rust-toolchain@stable
        with:
          toolchain: 1.95.0
          targets: ${{ matrix.target }}

      - name: Cache cargo registry and target
        uses: Swatinem/rust-cache@v2
        with:
          workspaces: triage-cli-rs -> triage-cli-rs/target
          key: ${{ matrix.target }}

      - name: cargo build --release
        working-directory: triage-cli-rs
        run: cargo build --release --target ${{ matrix.target }}

      - name: Assemble archive (Windows)
        if: matrix.runner == 'windows-latest'
        shell: pwsh
        run: |
          $stage = "stage"
          New-Item -ItemType Directory -Path $stage | Out-Null
          Copy-Item "triage-cli-rs/target/${{ matrix.target }}/release/${{ matrix.binary }}" $stage
          Copy-Item "apex-cnc-inventory.md" $stage
          Compress-Archive -Path "$stage/*" -DestinationPath "${{ matrix.archive }}"

      - name: Assemble archive (Unix)
        if: matrix.runner != 'windows-latest'
        run: |
          mkdir -p stage
          cp "triage-cli-rs/target/${{ matrix.target }}/release/${{ matrix.binary }}" stage/
          cp apex-cnc-inventory.md stage/
          tar -C stage -czf "${{ matrix.archive }}" .

      - name: Upload archive as workflow artifact
        uses: actions/upload-artifact@v4
        with:
          name: ${{ matrix.archive }}
          path: ${{ matrix.archive }}
          if-no-files-found: error
          retention-days: 1

  release:
    name: release
    needs: build
    runs-on: ubuntu-latest
    steps:
      - name: Checkout (for changelog generation)
        uses: actions/checkout@v4
        with:
          fetch-depth: 0  # full history so the changelog walk works

      - name: Download all archives
        uses: actions/download-artifact@v4
        with:
          path: artifacts

      - name: Flatten and compute SHA256SUMS
        run: |
          mkdir -p release
          find artifacts -type f \( -name '*.zip' -o -name '*.tar.gz' \) -exec mv {} release/ \;
          cd release
          sha256sum *.zip *.tar.gz > SHA256SUMS
          cat SHA256SUMS

      - name: Generate changelog from git log
        id: changelog
        run: |
          PREV=$(git describe --tags --abbrev=0 HEAD^ 2>/dev/null || echo "")
          {
            echo 'body<<EOF'
            if [ -n "$PREV" ]; then
              echo "Changes since $PREV:"
              echo ""
              git log "$PREV..HEAD" --pretty=format:'- %s'
            else
              echo "Initial release."
            fi
            echo ""
            echo "EOF"
          } >> "$GITHUB_OUTPUT"

      - name: Create GitHub Release
        uses: softprops/action-gh-release@v2
        with:
          files: release/*
          body: ${{ steps.changelog.outputs.body }}
          draft: false
          prerelease: false
```

- [ ] **Step 2: Validate the YAML**

Run: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release.yml'))"`
Expected: no output (success).

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci: add release workflow building windows/mac/linux binaries"
```

- [ ] **Step 4: Test by pushing a pre-release tag**

```bash
git tag v0.2.0-rc1
git push origin v0.2.0-rc1
```

Watch the `release` workflow run. Expected: 4 build jobs (one per matrix entry) followed by 1 release job. Total runtime ~10-15 minutes the first time (Windows is slowest).

If the run succeeds, navigate to the GitHub Releases page and confirm:
- 4 archives present (windows.zip, 2× macos.tar.gz, linux.tar.gz)
- `SHA256SUMS` present
- Release notes auto-generated from git log

If anything fails: read the failing step's log, fix in a follow-up commit, delete the failed tag (`git push --delete origin v0.2.0-rc1 && git tag -d v0.2.0-rc1`), and try a new tag.

---

## Component 4 — Install scripts

### Task 13: Create `install.ps1` (Windows)

**Files:**
- Create: `install.ps1` (repo root)

- [ ] **Step 1: Create the PowerShell installer**

Create `install.ps1` at the repo root with this content:

```powershell
# triage-cli installer for Windows.
# Usage:  irm https://raw.githubusercontent.com/midwestman35/triage-cli/main/install.ps1 | iex
# Flags:  -Version v0.2.0      Pin to a specific release tag.
#         -Channel prerelease  Allow prereleases when picking "latest".
#         -DryRun              Print actions without executing them.

[CmdletBinding()]
param(
    [string]$Version,
    [ValidateSet('stable', 'prerelease')]
    [string]$Channel = 'stable',
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$Repo       = 'midwestman35/triage-cli'
$BinDir     = Join-Path $env:LOCALAPPDATA 'Programs\triage-cli\bin'
$DataDirEnv = $env:TRIAGE_HOME
$DataDir    = if ($DataDirEnv) { $DataDirEnv } else { Join-Path $env:LOCALAPPDATA 'triage-cli' }

function Step($msg) {
    if ($DryRun) { Write-Host "[dry-run] $msg" -ForegroundColor Yellow }
    else        { Write-Host $msg -ForegroundColor Cyan }
}

# 1. Pre-flight: arch check.
if ($env:PROCESSOR_ARCHITECTURE -ne 'AMD64') {
    throw "Unsupported architecture: $env:PROCESSOR_ARCHITECTURE. Only AMD64 (x64) is supported."
}

# 2. Resolve target release.
$apiUrl = if ($Version) {
    "https://api.github.com/repos/$Repo/releases/tags/$Version"
} elseif ($Channel -eq 'prerelease') {
    "https://api.github.com/repos/$Repo/releases"  # returns array; we'll pick [0]
} else {
    "https://api.github.com/repos/$Repo/releases/latest"
}

Step "Querying $apiUrl"
$release = Invoke-RestMethod -Uri $apiUrl -UseBasicParsing
if ($release -is [System.Array]) { $release = $release[0] }
$tag = $release.tag_name
Write-Host "Installing $tag"

# 3. Find the windows zip and sums asset.
$zipName  = 'triage-cli-x86_64-windows.zip'
$sumsName = 'SHA256SUMS'
$zipAsset  = $release.assets | Where-Object { $_.name -eq $zipName }  | Select-Object -First 1
$sumsAsset = $release.assets | Where-Object { $_.name -eq $sumsName } | Select-Object -First 1
if (-not $zipAsset)  { throw "Release $tag is missing asset: $zipName" }
if (-not $sumsAsset) { throw "Release $tag is missing asset: $sumsName" }

# 4. Download to a tempdir.
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) "triage-cli-install-$([System.Guid]::NewGuid())"
New-Item -ItemType Directory -Path $tmp | Out-Null
$zipPath  = Join-Path $tmp $zipName
$sumsPath = Join-Path $tmp $sumsName

Step "Downloading $zipName"
if (-not $DryRun) { Invoke-WebRequest -Uri $zipAsset.browser_download_url -OutFile $zipPath -UseBasicParsing }
Step "Downloading $sumsName"
if (-not $DryRun) { Invoke-WebRequest -Uri $sumsAsset.browser_download_url -OutFile $sumsPath -UseBasicParsing }

# 5. Verify SHA256.
if (-not $DryRun) {
    $actual   = (Get-FileHash $zipPath -Algorithm SHA256).Hash.ToLower()
    $expected = (Select-String -Path $sumsPath -Pattern "  $zipName$" | Select-Object -First 1).Line.Split(' ')[0].ToLower()
    if (-not $expected) { throw "SHA256SUMS did not contain a line for $zipName" }
    if ($actual -ne $expected) {
        throw "SHA256 mismatch for ${zipName}: expected $expected, got $actual"
    }
    Step "SHA256 verified: $expected"
}

# 6. Install: unpack into BinDir.
Step "Installing binary to $BinDir"
if (-not $DryRun) {
    New-Item -ItemType Directory -Path $BinDir -Force | Out-Null
    $unpack = Join-Path $tmp 'unpack'
    New-Item -ItemType Directory -Path $unpack | Out-Null
    Expand-Archive -Path $zipPath -DestinationPath $unpack -Force

    # 6a. Atomic binary swap (handles "exe is running" by renaming first).
    $exeDest = Join-Path $BinDir 'triage-cli.exe'
    $exeNew  = Join-Path $BinDir 'triage-cli.exe.new'
    $exeOld  = Join-Path $BinDir 'triage-cli.exe.old'
    Copy-Item (Join-Path $unpack 'triage-cli.exe') $exeNew -Force
    if (Test-Path $exeDest) {
        if (Test-Path $exeOld) { Remove-Item $exeOld -Force -ErrorAction SilentlyContinue }
        Rename-Item $exeDest $exeOld -ErrorAction SilentlyContinue
    }
    Rename-Item $exeNew $exeDest
    Remove-Item $exeOld -Force -ErrorAction SilentlyContinue  # best-effort
}

# 7. Seed data dir.
Step "Seeding data dir at $DataDir"
if (-not $DryRun) {
    New-Item -ItemType Directory -Path $DataDir -Force | Out-Null
    $invSrc = Join-Path $unpack 'apex-cnc-inventory.md'
    $invDst = Join-Path $DataDir 'apex-cnc-inventory.md'
    $invVer = Join-Path $DataDir '.inventory-version'
    if (-not (Test-Path $invDst)) {
        Copy-Item $invSrc $invDst
        (Get-FileHash $invDst -Algorithm SHA256).Hash.ToLower() | Set-Content $invVer
    } else {
        $shippedHash = (Get-FileHash $invSrc -Algorithm SHA256).Hash.ToLower()
        $previousHash = if (Test-Path $invVer) { (Get-Content $invVer).Trim().ToLower() } else { '' }
        $localHash    = (Get-FileHash $invDst -Algorithm SHA256).Hash.ToLower()
        if ($localHash -eq $previousHash) {
            # Analyst hasn't edited locally — safe to update.
            Copy-Item $invSrc $invDst -Force
            $shippedHash | Set-Content $invVer
        } else {
            # Analyst has hand-edited. Drop the new copy beside it.
            Copy-Item $invSrc "$invDst.new" -Force
            Write-Host "warning: existing apex-cnc-inventory.md has local edits; new copy saved as apex-cnc-inventory.md.new" -ForegroundColor Yellow
        }
    }
}

# 8. PATH management.
$userPath = [Environment]::GetEnvironmentVariable('PATH', 'User')
if ($userPath -notlike "*$BinDir*") {
    Step "Adding $BinDir to user PATH"
    if (-not $DryRun) {
        $newPath = if ($userPath) { "$userPath;$BinDir" } else { $BinDir }
        [Environment]::SetEnvironmentVariable('PATH', $newPath, 'User')
    }
    Write-Host "note: Open a new terminal window for PATH changes to take effect." -ForegroundColor Yellow
}

# 9. Cleanup.
if (-not $DryRun) { Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue }

# 10. Final output.
Write-Host ""
Write-Host "triage-cli installed ($tag)." -ForegroundColor Green
Write-Host "Run: triage-cli setup    # to enter your Zendesk and provider credentials"
Write-Host "Run: triage-cli doctor   # to verify everything works"
```

- [ ] **Step 2: Lint with PSScriptAnalyzer if available (optional)**

Run (on Windows or in a PS Core env): `Invoke-ScriptAnalyzer install.ps1`
Expected: no errors. (Skip this step if PSScriptAnalyzer isn't installed; we don't gate the commit on it.)

- [ ] **Step 3: Commit**

```bash
git add install.ps1
git commit -m "feat(install): add Windows PowerShell installer"
```

- [ ] **Step 4: Smoke-test against the v0.2.0-rc1 release**

On a Windows machine (or a CI scratch run), execute:

```powershell
$env:DRY_RUN_OK = $true
irm https://raw.githubusercontent.com/midwestman35/triage-cli/main/install.ps1 | iex -DryRun
```

Expected: prints each step prefixed `[dry-run]`, no files modified.

Then re-run without `-DryRun` and confirm `triage-cli --version` works in a new PowerShell window.

---

### Task 14: Create `install.sh` (Mac/Linux)

**Files:**
- Create: `install.sh` (repo root)

- [ ] **Step 1: Create the POSIX shell installer**

Create `install.sh` at the repo root with this content:

```bash
#!/usr/bin/env sh
# triage-cli installer for macOS and Linux.
# Usage:  curl -fsSL https://raw.githubusercontent.com/midwestman35/triage-cli/main/install.sh | sh
# Flags:  --version v0.2.0       Pin to a specific release tag.
#         --channel prerelease   Allow prereleases when picking "latest".
#         --dry-run              Print actions without executing them.

set -eu

REPO="midwestman35/triage-cli"
VERSION=""
CHANNEL="stable"
DRY_RUN=""

while [ "$#" -gt 0 ]; do
    case "$1" in
        --version) VERSION="$2"; shift 2 ;;
        --channel) CHANNEL="$2"; shift 2 ;;
        --dry-run) DRY_RUN="1"; shift ;;
        *) echo "Unknown flag: $1" >&2; exit 1 ;;
    esac
done

step() {
    if [ -n "$DRY_RUN" ]; then printf '\033[33m[dry-run]\033[0m %s\n' "$*"
    else                       printf '\033[36m%s\033[0m\n' "$*"
    fi
}

# 1. Detect OS + arch.
uname_s="$(uname -s)"
uname_m="$(uname -m)"
case "$uname_s/$uname_m" in
    Darwin/arm64)         TARGET="aarch64-macos";   ARCHIVE="triage-cli-aarch64-macos.tar.gz" ;;
    Darwin/x86_64)        TARGET="x86_64-macos";    ARCHIVE="triage-cli-x86_64-macos.tar.gz" ;;
    Linux/x86_64)         TARGET="x86_64-linux";    ARCHIVE="triage-cli-x86_64-linux.tar.gz" ;;
    *) echo "Unsupported platform: $uname_s/$uname_m" >&2; exit 1 ;;
esac

# 2. Resolve install dirs.
BIN_DIR="$HOME/.local/bin"
if [ -n "${TRIAGE_HOME:-}" ]; then
    DATA_DIR="$TRIAGE_HOME"
elif [ "$uname_s" = "Darwin" ]; then
    DATA_DIR="$HOME/Library/Application Support/triage-cli"
else
    DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/triage-cli"
fi

# 3. Resolve release.
if [ -n "$VERSION" ]; then
    API="https://api.github.com/repos/$REPO/releases/tags/$VERSION"
elif [ "$CHANNEL" = "prerelease" ]; then
    API="https://api.github.com/repos/$REPO/releases"
else
    API="https://api.github.com/repos/$REPO/releases/latest"
fi
step "Querying $API"
release_json="$(curl -fsSL "$API")"
if [ "$CHANNEL" = "prerelease" ]; then
    # Take the first element of the array.
    TAG="$(printf '%s' "$release_json" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1)"
else
    TAG="$(printf '%s' "$release_json" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1)"
fi
[ -n "$TAG" ] || { echo "Could not resolve release tag" >&2; exit 1; }
echo "Installing $TAG"

# 4. Download archive + SHA256SUMS.
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
ARCHIVE_URL="https://github.com/$REPO/releases/download/$TAG/$ARCHIVE"
SUMS_URL="https://github.com/$REPO/releases/download/$TAG/SHA256SUMS"
step "Downloading $ARCHIVE"
[ -n "$DRY_RUN" ] || curl -fsSL "$ARCHIVE_URL" -o "$TMP/$ARCHIVE"
step "Downloading SHA256SUMS"
[ -n "$DRY_RUN" ] || curl -fsSL "$SUMS_URL" -o "$TMP/SHA256SUMS"

# 5. Verify SHA256.
if [ -z "$DRY_RUN" ]; then
    if command -v shasum >/dev/null 2>&1; then
        ACTUAL="$(shasum -a 256 "$TMP/$ARCHIVE" | awk '{print $1}')"
    elif command -v sha256sum >/dev/null 2>&1; then
        ACTUAL="$(sha256sum "$TMP/$ARCHIVE" | awk '{print $1}')"
    else
        echo "Neither shasum nor sha256sum available; cannot verify download." >&2
        exit 1
    fi
    EXPECTED="$(awk -v n="$ARCHIVE" '$2 == n { print $1 }' "$TMP/SHA256SUMS")"
    [ -n "$EXPECTED" ] || { echo "SHA256SUMS missing line for $ARCHIVE" >&2; exit 1; }
    if [ "$ACTUAL" != "$EXPECTED" ]; then
        echo "SHA256 mismatch for $ARCHIVE: expected $EXPECTED, got $ACTUAL" >&2
        exit 1
    fi
    step "SHA256 verified: $EXPECTED"
fi

# 6. Unpack + install binary atomically.
step "Installing binary to $BIN_DIR/triage-cli"
if [ -z "$DRY_RUN" ]; then
    mkdir -p "$BIN_DIR"
    mkdir -p "$TMP/unpack"
    tar -C "$TMP/unpack" -xzf "$TMP/$ARCHIVE"
    BIN_DEST="$BIN_DIR/triage-cli"
    BIN_NEW="$BIN_DEST.new"
    cp "$TMP/unpack/triage-cli" "$BIN_NEW"
    chmod +x "$BIN_NEW"
    mv "$BIN_NEW" "$BIN_DEST"  # atomic rename; survives running binary
fi

# 7. Seed data dir.
step "Seeding data dir at $DATA_DIR"
if [ -z "$DRY_RUN" ]; then
    mkdir -p "$DATA_DIR"
    INV_SRC="$TMP/unpack/apex-cnc-inventory.md"
    INV_DST="$DATA_DIR/apex-cnc-inventory.md"
    INV_VER="$DATA_DIR/.inventory-version"
    if [ ! -f "$INV_DST" ]; then
        cp "$INV_SRC" "$INV_DST"
        ( cd "$DATA_DIR" && (shasum -a 256 apex-cnc-inventory.md 2>/dev/null || sha256sum apex-cnc-inventory.md) | awk '{print $1}' ) > "$INV_VER"
    else
        SHIPPED_HASH="$( (shasum -a 256 "$INV_SRC" 2>/dev/null || sha256sum "$INV_SRC") | awk '{print $1}' )"
        PREV_HASH="$( [ -f "$INV_VER" ] && cat "$INV_VER" | tr -d '[:space:]' || echo "" )"
        LOCAL_HASH="$( (shasum -a 256 "$INV_DST" 2>/dev/null || sha256sum "$INV_DST") | awk '{print $1}' )"
        if [ "$LOCAL_HASH" = "$PREV_HASH" ]; then
            cp "$INV_SRC" "$INV_DST"
            echo "$SHIPPED_HASH" > "$INV_VER"
        else
            cp "$INV_SRC" "$INV_DST.new"
            echo "warning: existing apex-cnc-inventory.md has local edits; new copy saved as apex-cnc-inventory.md.new" >&2
        fi
    fi
fi

# 8. PATH hint (don't auto-edit rc files).
case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *)
        echo ""
        echo "note: $BIN_DIR is not on your \$PATH."
        SHELL_NAME="$(basename "${SHELL:-/bin/sh}")"
        case "$SHELL_NAME" in
            zsh)  RC="$HOME/.zshrc" ;;
            bash) RC="$HOME/.bashrc" ;;
            *)    RC="your shell's rc file" ;;
        esac
        echo "Add this line to $RC and open a new terminal:"
        echo "  export PATH=\"\$HOME/.local/bin:\$PATH\""
        ;;
esac

# 9. Final output.
echo ""
echo "triage-cli installed ($TAG)."
echo "Run: triage-cli setup    # to enter your Zendesk and provider credentials"
echo "Run: triage-cli doctor   # to verify everything works"
```

- [ ] **Step 2: Make it executable in git**

```bash
chmod +x install.sh
```

- [ ] **Step 3: Lint with shellcheck if available**

Run: `shellcheck -s sh install.sh`
Expected: no errors. (Skip if shellcheck unavailable.)

- [ ] **Step 4: Smoke-test in dry-run mode**

```bash
./install.sh --dry-run
```

Expected: prints each step prefixed `[dry-run]`, no files modified.

- [ ] **Step 5: Commit**

```bash
git add install.sh
git commit -m "feat(install): add macOS/Linux POSIX shell installer"
```

---

### Task 15: Update README.md install section

**Files:**
- Modify: `README.md` (the "Install" section, currently ~lines 60-75 per the recon)

- [ ] **Step 1: Replace the existing "Install" section**

In `README.md`, find the `## Install` heading and replace the section's body (down to but not including the next `## ` heading) with:

```markdown
## Install

### Windows (primary platform)

Open PowerShell and run:

```powershell
irm https://raw.githubusercontent.com/midwestman35/triage-cli/main/install.ps1 | iex
```

This downloads the latest release, verifies its SHA256 against the published `SHA256SUMS`, installs `triage-cli.exe` into `%LOCALAPPDATA%\Programs\triage-cli\bin\`, and seeds the data directory at `%LOCALAPPDATA%\triage-cli\`. Open a new PowerShell window after install for `$PATH` to refresh.

The script does not require admin privileges and never modifies machine-wide settings.

### macOS / Linux

```bash
curl -fsSL https://raw.githubusercontent.com/midwestman35/triage-cli/main/install.sh | sh
```

Installs the binary into `~/.local/bin/triage-cli` and seeds the data directory at `~/Library/Application Support/triage-cli/` (macOS) or `${XDG_DATA_HOME:-~/.local/share}/triage-cli/` (Linux).

### Install script flags

Both scripts accept these flags:

| Flag | Purpose |
|---|---|
| `-Version v0.2.0` / `--version v0.2.0` | Pin to a specific release tag. |
| `-Channel prerelease` / `--channel prerelease` | Pick the newest prerelease instead of the latest stable. |
| `-DryRun` / `--dry-run` | Print every action without executing. Useful for review. |

To pass flags through `iex` on Windows, download the script first:

```powershell
irm https://raw.githubusercontent.com/midwestman35/triage-cli/main/install.ps1 -OutFile install.ps1
.\install.ps1 -Version v0.2.0
```

### Upgrading

Re-run the same install one-liner. The script detects the newer release, verifies SHA256, and replaces the binary in place. Your `.env`, `MEMORY.md`, and other local state are not touched.

### Migrating from a repo-clone install

If you previously installed by cloning the repo and running `cargo build --release`, run once from inside that clone:

```bash
triage-cli migrate-home
```

This copies `.env`, `MEMORY.md`, `apex-cnc-inventory.md`, and the `data/` directory into `$TRIAGE_HOME` (or the platform default), after which the binary can be invoked from any directory.

### Uninstall

There is no uninstaller. Delete the binary directory (`%LOCALAPPDATA%\Programs\triage-cli\` on Windows, `~/.local/bin/triage-cli` elsewhere) and the data directory (`%LOCALAPPDATA%\triage-cli\` on Windows, the path printed by `triage-cli doctor` elsewhere).

### Build from source

The "clone and `cargo build --release`" path described in `CLAUDE.md` still works and is the supported developer setup. End users should prefer the install scripts.
```

- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "docs(install): document install.ps1 / install.sh and migrate-home"
```

---

## Component 5 — Version check in `doctor` + atomic update (mostly done in Tasks 13-14)

The atomic-binary-swap behavior is already in the install scripts written in Tasks 13-14. This component covers the in-tool version check.

### Task 16: Add opportunistic version check to `doctor`

**Files:**
- Modify: `triage-cli-rs/src/setup.rs` (the doctor function)

- [ ] **Step 1: Add a helper that performs the version check**

In `triage-cli-rs/src/setup.rs`, append (at the bottom of the file, before any `#[cfg(test)]` block):

```rust
/// Best-effort check against the latest GitHub Release tag. Returns the new
/// version string if a strictly-newer release exists, else `None`. Any
/// failure (network, timeout, JSON parse, semver compare, GH rate limit)
/// resolves to `None` — this is opportunistic icing on doctor, not a
/// critical check.
async fn check_for_update() -> Option<String> {
    let current = env!("CARGO_PKG_VERSION");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .user_agent(format!("triage-cli/{}", current))
        .build()
        .ok()?;
    let resp = client
        .get("https://api.github.com/repos/midwestman35/triage-cli/releases/latest")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let json: serde_json::Value = resp.json().await.ok()?;
    let tag = json.get("tag_name")?.as_str()?.trim_start_matches('v');
    if is_strictly_newer(tag, current) {
        Some(tag.to_string())
    } else {
        None
    }
}

/// Naive semver compare: split on `.`, compare numeric components
/// left-to-right. Returns true if `a` is strictly greater than `b`.
/// Pre-release suffixes (e.g., `-rc1`) are stripped before comparison —
/// we only nudge users between stable releases, not from `0.2.0-rc1` to
/// `0.2.0`.
fn is_strictly_newer(a: &str, b: &str) -> bool {
    fn parts(s: &str) -> Vec<u32> {
        s.split('-')
            .next()
            .unwrap_or("")
            .split('.')
            .filter_map(|p| p.parse().ok())
            .collect()
    }
    let ap = parts(a);
    let bp = parts(b);
    let n = ap.len().max(bp.len());
    for i in 0..n {
        let av = ap.get(i).copied().unwrap_or(0);
        let bv = bp.get(i).copied().unwrap_or(0);
        if av > bv { return true; }
        if av < bv { return false; }
    }
    false
}

#[cfg(test)]
mod version_tests {
    use super::is_strictly_newer;

    #[test] fn newer_patch()   { assert!(is_strictly_newer("0.2.1", "0.2.0")); }
    #[test] fn newer_minor()   { assert!(is_strictly_newer("0.3.0", "0.2.5")); }
    #[test] fn same_version()  { assert!(!is_strictly_newer("0.2.0", "0.2.0")); }
    #[test] fn older()         { assert!(!is_strictly_newer("0.1.9", "0.2.0")); }
    #[test] fn pre_release_a() { assert!(!is_strictly_newer("0.2.0-rc1", "0.2.0")); }
    #[test] fn pre_release_b() { assert!(!is_strictly_newer("0.2.0", "0.2.0-rc1")); }
}
```

- [ ] **Step 2: Call the version check at the end of doctor**

In the same file, find the `cmd_doctor` (or `run_doctor`) function. Just before its return statement, add:

```rust
    if let Some(newer) = check_for_update().await {
        eprintln!(
            "{}: update available: {} (you have {}). re-run install.ps1 (or install.sh) to upgrade.",
            "note".yellow().bold(),
            newer,
            env!("CARGO_PKG_VERSION"),
        );
    }
```

(If `cmd_doctor` is not currently `async`, you'll need to make it `async` and ensure its caller awaits it. Check the existing call site in `cli.rs`; if it's already `.await`-ed for other async checks, no change needed. If not, the simplest path is `tokio::runtime::Runtime::new()?.block_on(check_for_update())` inline at the call site to avoid changing the function signature.)

- [ ] **Step 3: Run the version-compare tests**

Run: `cd triage-cli-rs && cargo test --lib version_tests::`
Expected: 6 tests pass.

- [ ] **Step 4: Manual smoke**

Run: `./target/release/triage-cli doctor`
Expected: doctor runs normally; if a newer release exists on GitHub, a yellow line appears near the end. If you're offline or no newer release exists, no version-check line appears at all.

- [ ] **Step 5: Commit**

```bash
git add triage-cli-rs/src/setup.rs
git commit -m "feat(doctor): opportunistic latest-release version check"
```

---

## Final integration

### Task 17: End-to-end release rehearsal

- [ ] **Step 1: Cut a release tag**

```bash
git tag v0.2.0
git push origin v0.2.0
```

Watch the `release` workflow run to completion. Expected: 4 build jobs + 1 release job, ~10-15 min, GitHub Release created with 5 attached files.

- [ ] **Step 2: Install on a Windows machine via the one-liner**

```powershell
irm https://raw.githubusercontent.com/midwestman35/triage-cli/main/install.ps1 | iex
```

Expected:
- No errors, no admin prompt.
- `triage-cli --version` in a NEW PowerShell window prints `triage-cli 0.2.0`.
- `triage-cli doctor` prints the resolved paths block and exits cleanly.
- `triage-cli setup` walks through `.env` configuration.

- [ ] **Step 3: Install on Mac/Linux via the one-liner**

```bash
curl -fsSL https://raw.githubusercontent.com/midwestman35/triage-cli/main/install.sh | sh
```

Same expected outcome as Windows, in the platform-default data dir.

- [ ] **Step 4: Verify upgrade flow**

Cut a `v0.2.1` tag with any trivial change. Re-run the install one-liner. Expected: existing data dir is untouched; binary is replaced; `triage-cli --version` reports `0.2.1`.

- [ ] **Step 5: Smoke-test `migrate-home`**

In a fresh repo clone, run:
```bash
triage-cli migrate-home
```

Expected: copies `.env`, `MEMORY.md`, `apex-cnc-inventory.md`, and `data/` into the platform-default `$TRIAGE_HOME`. Prints the destination path. Source files remain in cwd.

- [ ] **Step 6: No-op final commit if any doc tweaks**

If smoke testing surfaced any README or doc tweaks, fold them in here. Otherwise this task has no commit; mark done.

---

## Spec-coverage cross-check

Every spec section maps to at least one task:

| Spec section | Task(s) |
|---|---|
| Component 1 (Release pipeline) | Task 12 |
| Component 2.1 (USER→USERNAME) | Task 2 |
| Component 2.2 (arboard) | Task 3 |
| Component 2.3 (similar) | Task 4 |
| Component 2.4 (shlex) | Task 5 |
| Component 2.5 (cfg(unix) test gate) | Task 6 |
| Component 2.6 (Windows CI matrix) | Task 7 |
| Component 3 (triage_home resolver) | Task 8 |
| Component 3 (anchor data files) | Task 9 |
| Component 3 (migrate-home subcommand) | Task 10 |
| Component 3 (doctor path output + stale cnc-map) | Task 11 |
| Component 4 (install.ps1) | Task 13 |
| Component 4 (install.sh) | Task 14 |
| Component 4 (README docs) | Task 15 |
| Component 5 (atomic binary swap) | Tasks 13, 14 (in scripts) |
| Component 5 (CNC inventory shipping + seed-or-preserve logic) | Tasks 12 (release.yml bundles inventory in archives), 13 + 14 (install scripts seed/preserve via `.inventory-version` marker) |
| Component 5 (version check in doctor) | Task 16 |
| End-to-end integration test | Task 17 |

No gaps.
