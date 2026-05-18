# Distribution Design

**Date:** 2026-05-17
**Status:** Approved

## Goal

Replace the current "clone the repo and `cargo build --release`" install story with a one-liner install per platform that pulls a pre-built binary plus data bundle, sets up a per-user data directory, and works across Windows (primary — NOC operates on Windows machines), macOS, and Linux. Polished enough to demo upward; minimal enough to ship in ~2-3 days.

## Decisions

| Question | Decision | Reason |
|---|---|---|
| Target audience | Internal NOC team (~5-20 analysts) + presentable upward | Polished demo for potential org-wide test run, but no Apple notarization / SmartScreen reputation yet |
| Primary platform | Windows (`x86_64-pc-windows-msvc`) | NOC operates on Windows; macOS/Linux are dev-side |
| Transport | GitHub Releases + install script per OS | One package manager registration per release is too much maintenance for 5-20 users |
| Windows install path | PowerShell one-liner (`irm \| iex`) only | No Scoop/WinGet/MSI — one polished path, not several mediocre ones |
| Mac/Linux install path | `curl \| sh` one-liner, mirror of the PS script | Symmetric UX across platforms |
| Working-directory contract | Decouple via `$TRIAGE_HOME`, default per-platform | Today's "binary expects cwd=repo" is the single biggest demo-quality blocker |
| Backwards compat for current users | "If cwd has `.env`, use cwd" branch | Zero churn for analysts mid-migration |
| Code signing | Not in scope yet | Months-long reputation tail; revisit if/when management commits to broader rollout |
| Data bundle (CNC inventory) | Ship in release archive alongside binary | One atomic shipping unit per release; preserve analyst-edited inventories |
| Version awareness | Opportunistic check in `triage-cli doctor`, all failures silent | Doctor's primary job is env-var validation; upgrade nudge is icing |
| Update mechanism | Re-run install script | Idempotent; no separate "upgrade" subcommand |

## Architecture

Five components ship together as the v1 distribution story:

1. **Release pipeline** — GitHub Actions workflow that builds platform binaries on `v*` tag push and attaches archives to a GitHub Release.
2. **Windows compat fixes** — small refactor to remove unix-only assumptions from the existing codebase.
3. **`$TRIAGE_HOME` resolution** — new path-resolution layer; backwards-compatible cwd fallback.
4. **Install scripts** — `install.ps1` and `install.sh`, checked into repo root.
5. **Update & version awareness** — `doctor` opportunistic version check; in-place binary swap on install re-run.

The pieces are independent but ordered for safe rollout: compat fixes land first, then the cwd refactor, then the install pipeline, then version checks.

## Component 1: Release pipeline

New workflow `.github/workflows/release.yml` triggered on `v*` tag push. The existing `ci.yml` is untouched.

### Job matrix

| Runner | Rust target | Output artifact |
|---|---|---|
| `windows-latest` | `x86_64-pc-windows-msvc` | `triage-cli-x86_64-windows.zip` |
| `macos-14` | `aarch64-apple-darwin` | `triage-cli-aarch64-macos.tar.gz` |
| `macos-13` | `x86_64-apple-darwin` | `triage-cli-x86_64-macos.tar.gz` |
| `ubuntu-latest` | `x86_64-unknown-linux-gnu` | `triage-cli-x86_64-linux.tar.gz` |

Every archive contains both the binary (`triage-cli` or `triage-cli.exe`) and `apex-cnc-inventory.md`. The install scripts know to extract both.

Per-job steps: checkout → install toolchain → `cargo build --release --target <triple>` → bundle binary + inventory into a platform-appropriate archive → compute SHA256 → upload as a workflow artifact.

A final `release` job (depends on all four build jobs) downloads all artifacts, generates a single `SHA256SUMS` file with one line per archive, and creates the GitHub Release with all five files attached (4 archives + 1 sums file). Release body is auto-generated from `git log <prev-tag>..HEAD`.

### Why a single aggregating release job

`SHA256SUMS` computation lives in one place against the final byte-identical artifacts the user will download. If each build job uploaded and computed its own hash row, the sums file would need to be assembled across jobs anyway.

### Runner notes

- `macos-14` is Apple Silicon; `macos-13` is Intel. Two runners for two arches beats cross-compilation.
- `windows-latest` uses MSVC toolchain by default (produces native `.exe` linked against the standard Windows C runtime — no MinGW DLL dependency).
- `rustls-tls` (already a dep) means no OpenSSL on any platform.

## Component 2: Windows compat fixes

Six concrete changes, ordered by isolation. Land first, before any release pipeline ships.

### 2.1 `USER` → `USERNAME` env-var fallback

**Files:** `triage-cli-rs/src/pipeline.rs:514`, `triage-cli-rs/src/tui/inbox.rs:1972`

Add a helper in `pipeline.rs`:

```rust
fn current_user() -> Option<String> {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .ok()
        .filter(|v| !v.trim().is_empty())
}
```

Replace both call sites. The `TRIAGE_OWNER` override stays primary.

### 2.2 Clipboard cascade → `arboard` crate

**File:** `triage-cli-rs/src/tui/inbox.rs:1396-1421`

Add `arboard = "3"` to `Cargo.toml`. Replace `copy_to_clipboard` with a single `arboard::Clipboard::new()?.set_text(text)?` call. Drop the `pbcopy`/`wl-copy`/`xclip` cascade and the warning string about installing them.

Side benefit: fixes a latent Wayland race condition (`wl-copy` currently forks and detaches without waiting; user can press `y` and quit the TUI before clipboard population completes).

### 2.3 `diff -u` shell-out → in-process via `similar`

**File:** `triage-cli-rs/src/cli.rs:984-988`

Add `similar = "2"` to `Cargo.toml`. Replace the `Command::new("diff")` block with:

```rust
let diff = similar::TextDiff::from_lines(&existing, &new)
    .unified_diff()
    .header("STATE.md (existing)", "STATE.md (new)")
    .to_string();
eprintln!("{}", diff);
```

Removes `/usr/bin/diff` as a runtime dependency on all platforms.

### 2.4 `sh -c $DIFF_VIEWER` → portable arg-splitting via `shlex`

**File:** `triage-cli-rs/src/cli.rs:958-980`

Add `shlex = "1"`. Replace the `Command::new("sh").arg("-c").arg(format!(...))` block with:

```rust
let parts = shlex::split(trimmed)
    .ok_or_else(|| io::Error::other("DIFF_VIEWER has unbalanced quoting"))?;
let (cmd, args) = parts.split_first()
    .ok_or_else(|| io::Error::other("DIFF_VIEWER is empty after parsing"))?;
let status = Command::new(cmd)
    .args(args)
    .arg(&existing_path)
    .arg(&new_path)
    .status()?;
```

Also fixes a latent Mac/Linux bug: shell expansion of paths-with-spaces could have surprised users with names containing spaces.

### 2.5 Cfg-gate the codex shell-script test mod

**File:** `triage-cli-rs/src/providers/codex.rs:240`

Add `#[cfg(unix)]` to the `mod followup_tests` line. The production code in that file is already portable; only the test helpers that write executable shell scripts to disk are unix-specific.

### 2.6 Add `windows-latest` to existing CI matrix

**File:** `.github/workflows/ci.yml`

Convert the single `rust-checks` job to a matrix over `[ubuntu-latest, windows-latest, macos-latest]` running fmt-check + clippy + test on each. This is the most important item — without it, Windows support rots silently.

Cost: ~2-3 minutes added per PR check (windows-latest is the slowest runner). Acceptable tax.

## Component 3: `$TRIAGE_HOME` resolution

The single most user-perceptible change. After this, the binary can be invoked from anywhere and resolves its files from a per-user data dir.

### New module: `triage-cli-rs/src/paths.rs`

```rust
pub fn triage_home() -> PathBuf {
    if let Ok(h) = std::env::var("TRIAGE_HOME") {
        if !h.trim().is_empty() { return PathBuf::from(h); }
    }
    if cwd_looks_like_repo() {
        return std::env::current_dir().unwrap();
    }
    platform_default_dir()
}

fn cwd_looks_like_repo() -> bool {
    let cwd = std::env::current_dir().ok();
    cwd.as_ref().map_or(false, |c|
        c.join(".env").exists() || c.join("apex-cnc-inventory.md").exists())
}

fn platform_default_dir() -> PathBuf {
    dirs::data_local_dir()
        .expect("OS provides a local data dir")
        .join("triage-cli")
}
```

### Resolution priority

1. `$TRIAGE_HOME` if set and non-empty → use it.
2. Else if cwd contains `.env` or `apex-cnc-inventory.md` → use cwd. **(backwards compat for current analysts)**
3. Else → platform default via `dirs::data_local_dir()`.

### Platform defaults

| Platform | Default `$TRIAGE_HOME` |
|---|---|
| Windows | `%LOCALAPPDATA%\triage-cli\` |
| macOS | `~/Library/Application Support/triage-cli/` |
| Linux | `${XDG_DATA_HOME:-~/.local/share}/triage-cli/` |

`dirs::data_local_dir()` returns the right thing on each. We use `data_local_dir` not `data_dir` so the state doesn't roam across machines (matters for Windows AD environments).

### Files anchored to `triage_home()` instead of cwd

| File | Today | After |
|---|---|---|
| `.env` | `./.env` | `$TRIAGE_HOME/.env` |
| `MEMORY.md` | `./MEMORY.md` | `$TRIAGE_HOME/MEMORY.md` |
| `apex-cnc-inventory.md` | `./apex-cnc-inventory.md` | `$TRIAGE_HOME/apex-cnc-inventory.md` |
| `data/cnc-map.json` | `./data/cnc-map.json` | `$TRIAGE_HOME/data/cnc-map.json` |
| `data/cnc-map-gaps.md` | `./data/cnc-map-gaps.md` | `$TRIAGE_HOME/data/cnc-map-gaps.md` |
| `data/memory.db` | `./data/memory.db` | `$TRIAGE_HOME/data/memory.db` |
| `data/watcher-state-*.json` | `./data/watcher-state-*.json` | `$TRIAGE_HOME/data/watcher-state-*.json` |

`TRIAGE_TICKETS_ROOT` default becomes `$TRIAGE_HOME/Tickets/`. The interactive scratch workspace (`./triage-notes/<id>/`) becomes `$TRIAGE_HOME/scratch/<id>/`.

### Migration subcommand

New `triage-cli migrate-home`:

- Reads cwd as source (where the analyst has been running it from).
- Resolves destination as: `$TRIAGE_HOME` if set, else the platform default. (Same resolution as the binary, minus the cwd-fallback branch — the whole point of `migrate-home` is to *leave* cwd.)
- Copies `.env`, `MEMORY.md`, `apex-cnc-inventory.md`, and `data/` into the resolved destination.
- Refuses to run if cwd and destination are the same directory.
- Prints "Done. You can now run triage-cli from any directory" + the destination path.
- Does **not** delete the cwd source files. Analyst confirms and cleans up manually.

### `doctor` updates

- Prints which paths it's using (resolves `triage_home()` and logs it).
- When run from cwd-with-`.env`, transparently uses cwd. Existing analysts see today's behavior.
- When run from a fresh per-user install, validates files exist in `$TRIAGE_HOME` and points at `migrate-home` if a sibling repo dir is detected.

## Component 4: Install scripts

Two scripts checked into the repo root: `install.ps1` (Windows) and `install.sh` (Mac/Linux). Both referenced from the README.

### Windows: `install.ps1`

Invoked as:
```powershell
irm https://raw.githubusercontent.com/midwestman35/triage-cli/main/install.ps1 | iex
```

Steps:

1. **Pre-flight:** confirm PowerShell ≥ 5.1; set `[Net.ServicePointManager]::SecurityProtocol = 'Tls12'` for older PS that defaults to TLS 1.0.
2. **Detect arch:** `$env:PROCESSOR_ARCHITECTURE`. Currently always `AMD64` for NOC; abort with a clear message on `ARM64`/`x86`.
3. **Resolve target version:** by default, GitHub Releases API → latest non-draft, non-prerelease tag. `-Version v0.2.0` flag overrides. `-Channel prerelease` flag opts in.
4. **Download:** `.zip` asset for `x86_64-windows` from the resolved release, plus `SHA256SUMS`.
5. **Verify:** `Get-FileHash` on the downloaded `.zip`, compare to `SHA256SUMS`. **Abort on mismatch.**
6. **Install:**
   - Unpack the zip into `$env:LOCALAPPDATA\Programs\triage-cli\bin\`. (Binary location is fixed; PATH manipulation depends on it.)
   - Resolve **data destination**: `$env:TRIAGE_HOME` if set, else `$env:LOCALAPPDATA\triage-cli\`. (Match the binary's resolution so script and binary always agree.)
   - Drop `apex-cnc-inventory.md` into the data destination (skip if exists and unchanged from previously-shipped version; see Component 5).
   - Drop `.env.example` into the data destination only if not already there.
7. **PATH management:** if `%LOCALAPPDATA%\Programs\triage-cli\bin` isn't in user PATH, append via `[Environment]::SetEnvironmentVariable("PATH", $new, "User")`. Print: *"Open a new terminal window for PATH changes to take effect."*
8. **Final output:**
   ```
   triage-cli installed (v0.2.0).
   Run: triage-cli setup    # to enter your Zendesk and provider credentials
   Run: triage-cli doctor   # to verify everything works
   ```

### Mac/Linux: `install.sh`

Invoked as:
```bash
curl -fsSL https://raw.githubusercontent.com/midwestman35/triage-cli/main/install.sh | sh
```

Same flow with platform substitutions:

- **Arch detection:** `uname -sm` → `aarch64-macos`, `x86_64-macos`, or `x86_64-linux`.
- **SHA256 verification:** prefer `shasum -a 256` (Mac), fall back to `sha256sum` (Linux).
- **Install location:** binary → `~/.local/bin/triage-cli`. Data destination resolves the same way as the binary: `$TRIAGE_HOME` if set, else `~/Library/Application Support/triage-cli/` on Mac, `${XDG_DATA_HOME:-~/.local/share}/triage-cli/` on Linux.
- **PATH hint:** if `~/.local/bin` isn't on `$PATH`, print the right rc-file line for the user's `$SHELL`. **Do not auto-edit rc files.**

### Shared script conventions

- `set -euo pipefail` (sh) / `$ErrorActionPreference = 'Stop'` (PS) — fail loud.
- `--dry-run` / `-DryRun` flag — print every action without executing.
- Idempotent re-run — upgrades binary in place, leaves analyst state untouched.
- No admin/sudo required. PowerShell script does **not** invoke `Start-Process -Verb RunAs`.

## Component 5: Update path & data-bundle handling

### Update mechanism

Re-running the install one-liner upgrades the tool. No separate "upgrade" subcommand. The script detects newer release tag, downloads, verifies, replaces the binary.

### Atomic binary replacement on Windows

`triage-cli.exe` can't be overwritten while running. The script:

1. Downloads to `$env:LOCALAPPDATA\Programs\triage-cli\bin\triage-cli.exe.new`.
2. Verifies SHA256.
3. Renames running `.exe` → `.exe.old` (Windows allows rename of a running exe).
4. Renames `.exe.new` → `.exe`.
5. Best-effort deletes `.exe.old` (cleaned up next run if currently busy).

Mac/Linux: same rename trick. Inode-based file references survive unlinks of the directory entry.

### Version check in `doctor`

At the end of the existing health checks:

1. Read embedded version (`env!("CARGO_PKG_VERSION")` at build time).
2. `GET` to `https://api.github.com/repos/midwestman35/triage-cli/releases/latest` with 2-second timeout.
3. Parse `tag_name`, strip leading `v`, compare semver.
4. If newer is available, print one yellow line:
   ```
   update available: 0.3.0 (you have 0.2.0). re-run install.ps1 to upgrade.
   ```
5. **Any failure is silent** — no network, GH down, rate-limited, JSON parse error → all skipped.

### CNC inventory bundling

The inventory ships in each release archive alongside the binary. Install script behavior:

- **First install:** drop into `$TRIAGE_HOME/apex-cnc-inventory.md`. `triage-cli setup` runs `build-map` automatically as its last step, generating `data/cnc-map.json`. Analyst can also re-run `triage-cli build-map` standalone any time.
- **Subsequent installs (upgrades):** the inventory is updated in place only if the existing file's hash matches the `.inventory-version` marker the script writes alongside it. If the analyst hand-edited the inventory, the new copy is dropped beside it as `apex-cnc-inventory.md.new` and the script prints a warning.
- **After any inventory update:** `doctor` notices `data/cnc-map.json` is older than `apex-cnc-inventory.md` and prints a yellow line: `cnc-map is stale; run triage-cli build-map to refresh.`

### Release cadence

- **Semver:** `0.x.y` pre-1.0. Breaking changes to ticket-folder shape or env-var contract bump `0.x.`; everything else bumps `0.x.y`.
- **Tag = release.** No release branches.
- **Changelog:** auto-generated from `git log <prev-tag>..HEAD`. Hand-curated `CHANGELOG.md` deferred until public.

### Failure & rollback behavior

| Failure | Behavior |
|---|---|
| CI build failure | GH Actions fails release job. Release never created. Existing release unchanged. |
| SHA mismatch during install | Script aborts before unpacking. Existing binary untouched. Loud red error with expected vs. actual hash and release URL. |
| Install interrupted (Ctrl-C, network drop) | `.exe.new`/`.exe.old` may be left around. Next run cleans up. Currently-installed binary unaffected. |
| Catastrophic uninstall | No uninstaller. Document manual cleanup (delete `Programs\triage-cli\` and `triage-cli\` directories) in README. |

## Non-goals (explicit YAGNI)

- **Code signing** (Apple notarization, Windows EV cert) — months-long reputation tail; revisit if/when org-wide rollout commits.
- **Package manager registrations** (Scoop, Homebrew, WinGet) — per-release manifest maintenance burden for 5-20 users isn't justified.
- **MSI installer / `.pkg`** — too heavy; per-user install script covers the same need without admin prompt.
- **Auto-update from inside the binary** (e.g., `triage-cli update`) — re-running install script is good enough; in-process updates open a Pandora's box of "what if the new binary is broken" scenarios.
- **Telemetry / install metrics** — out of scope; would require separate consent UX.
- **Windows ARM64 builds** — current NOC laptops are AMD64. Add when needed.
- **Separate "data release" channel for CNC inventory** — bundle in release archive for now; revisit if site-add cadence outpaces binary releases.

## Open questions / deferred decisions

None at design lock. Items called out as deferrable in *Non-goals* are explicitly deferred.

## Implementation order

The pieces are independent but ordered for safe rollout:

1. **Component 2 (Windows compat fixes)** — lands first. Self-contained refactor, gated by Windows CI matrix.
2. **Component 3 (`$TRIAGE_HOME`)** — lands second. Strictly additive thanks to the cwd-fallback branch.
3. **Component 1 (Release pipeline)** — lands third. Doesn't change runtime behavior; produces artifacts on `v*` tag.
4. **Component 4 (Install scripts)** — lands fourth. Consumes Component 1 artifacts.
5. **Component 5 (Version check in `doctor`, atomic update)** — lands fifth. Polish layer.

Each component is independently mergeable and testable.
