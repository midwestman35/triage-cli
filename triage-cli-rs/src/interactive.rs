//! Interactive evidence collection. Mirrors Python `triage_cli.interactive`.
//!
//! Responsibilities:
//! - Workspace dir + download manifest helpers
//! - All-or-nothing attachment download
//! - Drop-and-ready prompt + workspace summary

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use dialoguer::Confirm;
use serde_json::{json, Map, Value};
use thiserror::Error;

use crate::investigation::{detect_file_type, read_text_if_supported};
use crate::models::{AttachmentEvidence, LocalFileEvidence, Ticket};
use crate::zendesk::{ZendeskClient, ZendeskError};

const MANIFEST_NAME: &str = ".download-manifest.json";
const SKIP_TOKENS: &[&str] = &["skip", "quit", "q", "abort"];

#[derive(Debug, Error)]
pub enum InteractiveError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("zendesk error: {0}")]
    Zendesk(#[from] ZendeskError),
}

#[derive(Debug, Clone)]
pub struct Workspace {
    pub root: PathBuf,
    pub attachments_dir: PathBuf,
    pub local_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct DownloadDecision {
    pub action: DownloadAction,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadAction {
    Skip,
    Download,
}

pub fn ensure_workspace(notes_root: &Path, ticket_id: u64) -> std::io::Result<Workspace> {
    let root = notes_root.join(ticket_id.to_string());
    let attachments = root.join("attachments");
    let local = root.join("local");
    fs::create_dir_all(&attachments)?;
    fs::create_dir_all(&local)?;
    Ok(Workspace {
        root,
        attachments_dir: attachments,
        local_dir: local,
    })
}

pub fn read_manifest(attachments_dir: &Path) -> std::collections::HashMap<String, Value> {
    let path = attachments_dir.join(MANIFEST_NAME);
    if !path.exists() {
        return Default::default();
    }
    let Ok(text) = fs::read_to_string(&path) else {
        eprintln!(
            "warning: manifest at {} is unreadable; treating as empty.",
            path.display()
        );
        return Default::default();
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        eprintln!(
            "warning: manifest at {} is unreadable; treating as empty.",
            path.display()
        );
        return Default::default();
    };
    if let Value::Object(map) = value {
        map.into_iter().collect()
    } else {
        Default::default()
    }
}

pub fn write_manifest_entry(
    attachments_dir: &Path,
    filename: &str,
    size: u64,
    sha256: &str,
) -> std::io::Result<()> {
    let mut manifest = read_manifest(attachments_dir);
    manifest.insert(
        filename.to_string(),
        json!({
            "size": size,
            "sha256": sha256,
            "downloaded_at": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        }),
    );
    let path = attachments_dir.join(MANIFEST_NAME);
    let tmp = path.with_extension("json.tmp");
    let mut out = Map::new();
    for (k, v) in manifest {
        out.insert(k, v);
    }
    let json = serde_json::to_string_pretty(&Value::Object(out)).unwrap();
    fs::write(&tmp, format!("{json}\n"))?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

pub fn resolve_destination(
    attachments_dir: &Path,
    filename: &str,
    remote_size: Option<u64>,
) -> DownloadDecision {
    let manifest = read_manifest(attachments_dir);
    let entry = manifest.get(filename);
    let target = attachments_dir.join(filename);

    if let (Some(entry), Some(remote)) = (entry, remote_size) {
        let stored = entry.get("size").and_then(Value::as_u64);
        if stored == Some(remote) {
            return DownloadDecision {
                action: DownloadAction::Skip,
                path: target,
            };
        }
        if stored.is_some() && stored != Some(remote) {
            let mut n = 2;
            while attachments_dir
                .join(format!("{filename}.{n}"))
                .exists()
            {
                n += 1;
            }
            return DownloadDecision {
                action: DownloadAction::Download,
                path: attachments_dir.join(format!("{filename}.{n}")),
            };
        }
    }

    if target.exists() {
        return DownloadDecision {
            action: DownloadAction::Skip,
            path: target,
        };
    }
    DownloadDecision {
        action: DownloadAction::Download,
        path: target,
    }
}

pub fn confirm_download(ticket: &Ticket) -> bool {
    let attachments = flatten_attachments(ticket);
    if attachments.is_empty() {
        return false;
    }
    eprintln!(
        "Found {} attachment(s) on ticket #{}:",
        attachments.len(),
        ticket.id
    );
    for a in &attachments {
        let size = a
            .size_bytes
            .map(|b| format!("{b} bytes"))
            .unwrap_or_else(|| "unknown size".into());
        let ctype = a.content_type.as_deref().unwrap_or("unknown");
        eprintln!("  - {} ({ctype}, {size})", a.filename);
    }
    Confirm::new()
        .with_prompt("Download all to workspace?")
        .default(true)
        .interact()
        .unwrap_or(false)
}

/// Orchestrate the all-or-nothing attachment download.
pub async fn download_attachments(
    ticket: &Ticket,
    zd: &ZendeskClient,
    workspace: &Workspace,
    max_bytes: u64,
) -> Vec<AttachmentEvidence> {
    let attachments = flatten_attachments(ticket);
    if attachments.is_empty() {
        return Vec::new();
    }
    if !confirm_download(ticket) {
        return attachments;
    }
    let mut out = Vec::new();
    for a in attachments {
        let Some(url) = a.content_url.as_deref() else {
            eprintln!("warning: no download URL for {}; skipping.", a.filename);
            out.push(a);
            continue;
        };
        let decision = resolve_destination(&workspace.attachments_dir, &a.filename, a.size_bytes);
        if decision.action == DownloadAction::Skip {
            eprintln!(
                "  reused {} (manifest match)",
                decision.path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()
            );
            let mut updated = a;
            updated.local_path = Some(decision.path);
            out.push(updated);
            continue;
        }
        eprint!(
            "  downloading {}... ",
            decision.path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()
        );
        match zd.download_attachment(url, &decision.path, max_bytes).await {
            Ok((bytes_written, sha)) => {
                eprintln!("done ({bytes_written} bytes)");
                let _ = write_manifest_entry(
                    &workspace.attachments_dir,
                    &decision.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                    bytes_written,
                    &sha,
                );
                let mut updated = a;
                updated.local_path = Some(decision.path);
                out.push(updated);
            }
            Err(e) => {
                eprintln!("failed: {e}");
                out.push(a);
            }
        }
    }
    out
}

fn flatten_attachments(ticket: &Ticket) -> Vec<AttachmentEvidence> {
    ticket
        .comments
        .iter()
        .flat_map(|c| c.attachments.iter().cloned())
        .collect()
}

/// Block until user types `ready` (or empty enter); then scan `local/`.
pub fn prompt_drop_and_wait(workspace: &Workspace) -> Vec<LocalFileEvidence> {
    eprintln!(
        "\nDrop supplemental logs in:\n  {}\nSuggested types: zipped Apex station logs, Homer/Twilio SIP extracts,\nDatadog CSV. Press <enter> when ready (or 'skip' to continue with\nno local evidence).",
        workspace.local_dir.display()
    );
    loop {
        let mut line = String::new();
        eprint!("ready> ");
        let _ = std::io::stderr().flush();
        if std::io::stdin().read_line(&mut line).is_err() {
            line.clear();
        }
        let response = line.trim().to_ascii_lowercase();
        if SKIP_TOKENS.iter().any(|t| *t == response) {
            return Vec::new();
        }
        if response.is_empty() || response == "ready" {
            return ingest_local(&workspace.local_dir);
        }
        eprintln!("(type 'ready' or press <enter> to ingest; 'skip' to skip)");
    }
}

fn ingest_local(local_dir: &Path) -> Vec<LocalFileEvidence> {
    if !local_dir.exists() {
        return Vec::new();
    }
    let mut entries: Vec<_> = match fs::read_dir(local_dir) {
        Ok(rd) => rd.filter_map(Result::ok).collect(),
        Err(_) => return Vec::new(),
    };
    entries.sort_by_key(|e| e.path());
    let mut out = Vec::new();
    for entry in entries {
        let path = entry.path();
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let detected = detect_file_type(&path);
        let text = read_text_if_supported(&path, detected);
        out.push(LocalFileEvidence {
            path,
            size_bytes: Some(meta.len()),
            detected_type: Some(detected),
            extracted_text: text,
        });
    }
    out
}

pub fn summarize_workspace(
    _workspace: &Workspace,
    local_files: &[LocalFileEvidence],
    downloaded: &[AttachmentEvidence],
) -> String {
    let mut ingested: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for a in downloaded {
        let size = a
            .size_bytes
            .map(|b| format!("{b} bytes"))
            .unwrap_or_else(|| "?".into());
        if a.local_path.is_some() {
            ingested.push(format!("  {} ({size}, downloaded)", a.filename));
        } else {
            skipped.push(format!("  {} (download skipped)", a.filename));
        }
    }
    for lf in local_files {
        let size = lf
            .size_bytes
            .map(|b| format!("{b} bytes"))
            .unwrap_or_else(|| "?".into());
        let name = lf
            .path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| lf.path.display().to_string());
        if lf.extracted_text.is_some() {
            ingested.push(format!(
                "  {name} ({size}, {})",
                lf.detected_type.map(|t| t.as_str()).unwrap_or("unknown")
            ));
        } else {
            skipped.push(format!("  {name} ({size}, binary)"));
        }
    }
    if ingested.is_empty() && skipped.is_empty() {
        return "(no local evidence; proceeding with ticket-only context)".into();
    }
    let mut parts: Vec<String> = Vec::new();
    if !ingested.is_empty() {
        parts.push("Ingesting:".into());
        parts.extend(ingested);
    }
    if !skipped.is_empty() {
        parts.push("Skipping:".into());
        parts.extend(skipped);
    }
    parts.join("\n")
}
