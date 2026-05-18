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

/// Copy `.env`, `MEMORY.md`, `apex-cnc-inventory.md`, and `data/` from `src`
/// into `dest`. Refuses if `src == dest`. Does not delete from `src`.
/// Returns the destination path on success.
///
/// When `force` is `false` (the default), any file that already exists at the
/// destination is **skipped** and a `kept existing <name>` notice is emitted to
/// stderr. Pass `force: true` to restore the original overwrite behaviour.
pub fn migrate_home(
    src: &std::path::Path,
    dest: &std::path::Path,
    force: bool,
) -> std::io::Result<PathBuf> {
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
            if !force && to.exists() {
                eprintln!("migrate-home: kept existing {name}");
            } else {
                std::fs::copy(&from, &to)?;
            }
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
                if !force && to.exists() {
                    let name = entry.file_name();
                    eprintln!(
                        "migrate-home: kept existing data/{}",
                        name.to_string_lossy()
                    );
                } else {
                    std::fs::copy(&from, &to)?;
                }
            }
        }
    }

    Ok(dest.to_path_buf())
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

    #[test]
    fn migrate_home_copies_files_and_data_dir() {
        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join(".env"), "FOO=bar").unwrap();
        std::fs::write(src.path().join("MEMORY.md"), "memory").unwrap();
        std::fs::create_dir(src.path().join("data")).unwrap();
        std::fs::write(src.path().join("data").join("memory.db"), "db").unwrap();

        let returned = migrate_home(src.path(), dest.path(), false).unwrap();
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
        let result = migrate_home(dir.path(), dir.path(), false);
        assert!(result.is_err());
    }

    /// force=false: existing destination files are skipped (preserved), and the
    /// source is NOT lost.
    #[test]
    fn migrate_home_noclobber_preserves_existing_dest() {
        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();

        // Populate source.
        std::fs::write(src.path().join("MEMORY.md"), "new-memory").unwrap();
        std::fs::create_dir(src.path().join("data")).unwrap();
        std::fs::write(src.path().join("data").join("memory.db"), "new-db").unwrap();

        // Pre-populate destination with existing content.
        std::fs::write(dest.path().join("MEMORY.md"), "existing-memory").unwrap();
        std::fs::create_dir(dest.path().join("data")).unwrap();
        std::fs::write(dest.path().join("data").join("memory.db"), "existing-db").unwrap();

        migrate_home(src.path(), dest.path(), false).unwrap();

        // Destination files must be unchanged.
        assert_eq!(
            std::fs::read_to_string(dest.path().join("MEMORY.md")).unwrap(),
            "existing-memory"
        );
        assert_eq!(
            std::fs::read_to_string(dest.path().join("data").join("memory.db")).unwrap(),
            "existing-db"
        );
        // Source must still exist (migrate never deletes).
        assert!(src.path().join("MEMORY.md").exists());
        assert!(src.path().join("data").join("memory.db").exists());
    }

    /// force=true: existing destination files ARE overwritten.
    #[test]
    fn migrate_home_force_overwrites_existing_dest() {
        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();

        std::fs::write(src.path().join("MEMORY.md"), "new-memory").unwrap();
        std::fs::create_dir(src.path().join("data")).unwrap();
        std::fs::write(src.path().join("data").join("memory.db"), "new-db").unwrap();

        std::fs::write(dest.path().join("MEMORY.md"), "existing-memory").unwrap();
        std::fs::create_dir(dest.path().join("data")).unwrap();
        std::fs::write(dest.path().join("data").join("memory.db"), "existing-db").unwrap();

        migrate_home(src.path(), dest.path(), true).unwrap();

        // Destination files must now reflect the source.
        assert_eq!(
            std::fs::read_to_string(dest.path().join("MEMORY.md")).unwrap(),
            "new-memory"
        );
        assert_eq!(
            std::fs::read_to_string(dest.path().join("data").join("memory.db")).unwrap(),
            "new-db"
        );
    }
}
