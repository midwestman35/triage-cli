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
