use std::path::Path;

use chrono::{DateTime, Utc};
use ratatui::{
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Gauge, Paragraph, Row, Table, Wrap},
};

use super::app::{DetailMode, Focus, InboxApp, RowEntry};
use super::state::InboxStateSummary;
use super::TABS;

const SELECTED_ICON: &str = "◉";
const PHASES_TOTAL: u8 = 4;

pub(crate) fn draw(app: &mut InboxApp, frame: &mut ratatui::Frame) {
    let area = frame.area();
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(8),
            Constraint::Length(1),
        ])
        .split(area);
    draw_header(app, frame, outer[0]);
    draw_body(app, frame, outer[1]);
    draw_footer(frame, outer[2]);
    if app.notification.is_some() {
        draw_notification(app, frame, area);
    }
    if app.modal.is_some() {
        draw_modal(app, frame, area);
    }
    app.effects.render(frame);
}

fn draw_header(app: &InboxApp, frame: &mut ratatui::Frame, area: Rect) {
    let view_label = match app.opts.view_id {
        Some(id) => id.to_string(),
        None => "my tickets".into(),
    };
    let last = app
        .last_poll
        .map(|d| d.with_timezone(&chrono::Local).format("%H:%M").to_string())
        .unwrap_or_else(|| "-".into());
    let count = app.rows.len();
    let plural = if count == 1 { "ticket" } else { "tickets" };
    let polling_marker = if app.polling { " · polling…" } else { "" };
    let title = Line::from(vec![
        Span::styled(
            "triage-cli inbox",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("  ·  "),
        Span::raw(format!(
            "{view_label} · {count} {plural} · last poll: {last}{polling_marker}"
        )),
    ]);
    let para = Paragraph::new(title).block(Block::default().borders(Borders::BOTTOM));
    frame.render_widget(para, area);
}

fn draw_body(app: &mut InboxApp, frame: &mut ratatui::Frame, area: Rect) {
    let split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(area);
    draw_list(app, frame, split[0]);
    draw_detail(app, frame, split[1]);
}

fn draw_list(app: &InboxApp, frame: &mut ratatui::Frame, area: Rect) {
    let rows_data = app.sorted_rows();
    let now = Utc::now();
    let header = Row::new(vec![
        Cell::from(" "),
        Cell::from("Ticket"),
        Cell::from("Fork"),
        Cell::from("When"),
        Cell::from("Conf"),
        Cell::from("Owner / Status"),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = rows_data
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let is_selected = i == app.cursor;
            let icon_str = if is_selected {
                format!("{SELECTED_ICON} {}", row.status.icon())
            } else {
                format!("  {}", row.status.icon())
            };
            let fork = row
                .state
                .as_ref()
                .and_then(|s| s.fork.clone())
                .unwrap_or_else(|| "—".into());
            let when = match row.state.as_ref().and_then(|s| s.updated_at) {
                Some(t) => relative_time(t, now),
                None => "—".into(),
            };
            let conf = match row.state.as_ref().and_then(|s| s.confidence.clone()) {
                Some(c) => confidence_cell(&c),
                None => Cell::from("—"),
            };
            let summary = match row.state.as_ref() {
                Some(s) => {
                    let status = s.status.clone().unwrap_or_else(|| "open".into());
                    let owner = s.owner.clone().unwrap_or_else(|| "(unowned)".into());
                    truncate(&format!("{owner} · {status}"), 60)
                }
                None => row
                    .failure_reason
                    .clone()
                    .unwrap_or_else(|| row.status.label().to_string()),
            };
            let mut style = Style::default();
            if let Some(bg) = row.status.row_bg() {
                style = style.bg(bg);
            }
            if is_selected {
                style = style.add_modifier(Modifier::BOLD);
            }
            Row::new(vec![
                Cell::from(icon_str),
                Cell::from(format!("#{}", row.ticket_id)),
                Cell::from(fork),
                Cell::from(when),
                conf,
                Cell::from(summary),
            ])
            .style(style)
        })
        .collect();

    let widths = [
        Constraint::Length(4),
        Constraint::Length(8),
        Constraint::Length(5),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Min(20),
    ];

    let title = if app.focus == Focus::List {
        "Tickets ◀"
    } else {
        "Tickets"
    };
    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(title));
    frame.render_widget(table, area);
}

fn draw_detail(app: &InboxApp, frame: &mut ratatui::Frame, area: Rect) {
    let title = match app.detail_mode {
        DetailMode::Summary => "Summary".to_string(),
        DetailMode::File(i) => format!("{} ({}/{})", TABS[i], i + 1, TABS.len()),
    };
    let titled = if app.focus == Focus::Detail {
        format!("{title} ◀")
    } else {
        title
    };
    let outer = Block::default().borders(Borders::ALL).title(titled);
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let Some(row) = app.selected_row() else {
        let para = Paragraph::new("Select a ticket to view its report.".dim().to_string())
            .wrap(Wrap { trim: false });
        frame.render_widget(para, inner);
        return;
    };

    match row.status {
        super::app::Status::Queued => {
            let para = Paragraph::new("○ In queue — press Enter to triage now.".dim().to_string())
                .wrap(Wrap { trim: false });
            frame.render_widget(para, inner);
        }
        super::app::Status::Triaging => {
            let split = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(2),
                    Constraint::Length(2),
                    Constraint::Min(0),
                ])
                .split(inner);
            let label = row
                .phase_label
                .clone()
                .unwrap_or_else(|| "Triaging…".into());
            frame.render_widget(Paragraph::new(label), split[0]);
            let ratio = (row.phase_step as f64 / PHASES_TOTAL as f64).min(1.0);
            let gauge = Gauge::default()
                .ratio(ratio)
                .gauge_style(Style::default().fg(Color::Yellow))
                .label(format!("{}/{}", row.phase_step, PHASES_TOTAL));
            frame.render_widget(gauge, split[1]);
        }
        super::app::Status::Failed => {
            let msg = format!(
                "✗ Triage failed:\n\n{}",
                row.failure_reason.unwrap_or_else(|| "Unknown error".into())
            );
            let para = Paragraph::new(msg.red().to_string()).wrap(Wrap { trim: false });
            frame.render_widget(para, inner);
        }
        super::app::Status::Triaged => draw_triaged_detail(app, frame, inner, &row),
    }
}

fn draw_triaged_detail(app: &InboxApp, frame: &mut ratatui::Frame, inner: Rect, row: &RowEntry) {
    let mismatch = row
        .state
        .as_ref()
        .and_then(|s| s.rubric_version.as_deref())
        .and_then(|v| {
            let shipped = app.rubric.version();
            if v != shipped {
                Some((v.to_string(), shipped.to_string()))
            } else {
                None
            }
        });

    let (banner_area, content_area) = if mismatch.is_some() {
        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(inner);
        (Some(split[0]), split[1])
    } else {
        (None, inner)
    };

    if let (Some(area), Some((state_v, shipped_v))) = (banner_area, mismatch) {
        let line = rubric_mismatch_banner(&state_v, &shipped_v);
        let banner = Paragraph::new(line).style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );
        frame.render_widget(banner, area);
    }

    let body = match app.detail_mode {
        DetailMode::Summary => row
            .state
            .as_ref()
            .map(|s| render_synth_summary(row.ticket_id, s, app.rubric.version()).join("\n"))
            .unwrap_or_else(|| "(no STATE.md)".into()),
        DetailMode::File(i) => {
            let file = TABS[i];
            match row.folder.as_ref() {
                Some(folder) => read_file_for_display(&folder.join(file)),
                None => format!("(no ticket folder on disk for #{})", row.ticket_id),
            }
        }
    };
    let para = Paragraph::new(body)
        .wrap(Wrap { trim: false })
        .scroll((app.report_scroll, 0));
    frame.render_widget(para, content_area);
}

fn draw_footer(frame: &mut ratatui::Frame, area: Rect) {
    let hints =
        "↑/k ↓/j move · enter triage/focus · tab cycle files · esc summary · r refresh · y copy · o open · a chat · q quit";
    let para = Paragraph::new(hints.dim().to_string());
    frame.render_widget(para, area);
}

fn draw_notification(app: &InboxApp, frame: &mut ratatui::Frame, area: Rect) {
    let Some(n) = &app.notification else {
        return;
    };
    let style = match n.kind {
        super::app::NotifyKind::Info => Style::default().fg(Color::Cyan),
        super::app::NotifyKind::Success => Style::default().fg(Color::Green),
        super::app::NotifyKind::Warning => Style::default().fg(Color::Yellow),
        super::app::NotifyKind::Error => Style::default().fg(Color::Red),
    };
    let width = (n.text.chars().count() as u16 + 4).min(area.width.saturating_sub(2));
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(width) / 2),
        y: area.y + area.height.saturating_sub(4),
        width,
        height: 3,
    };
    frame.render_widget(Clear, rect);
    let para = Paragraph::new(n.text.clone())
        .style(style)
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(para, rect);
}

fn draw_modal(app: &InboxApp, frame: &mut ratatui::Frame, area: Rect) {
    let Some(modal) = &app.modal else {
        return;
    };
    let width = (area.width.saturating_sub(10)).min(70);
    let height = 10u16;
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(width) / 2),
        y: area.y + (area.height.saturating_sub(height) / 2),
        width,
        height,
    };
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Site lookup failed");
    frame.render_widget(block, rect);
    let inner = rect.inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    let mut lines: Vec<Line> = Vec::new();
    let subject_clipped: String = modal.subject.chars().take(70).collect();
    lines.push(Line::from(vec![
        Span::raw(format!("#{} ", modal.ticket_id)).bold(),
        Span::raw(subject_clipped),
    ]));
    if let Some(org) = &modal.org {
        lines.push(Line::from(format!("Org: {org}").dim().to_string()));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(
        "Could not auto-resolve site. Enter site_name (e.g. us-ga-roswell):"
            .yellow()
            .to_string(),
    ));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::raw("site_name> ").bold(),
        Span::raw(&modal.input),
        Span::raw("_"),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(
        "Enter to submit · Esc to cancel".dim().to_string(),
    ));
    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(para, inner);
}

fn confidence_cell(c: &str) -> Cell<'static> {
    let normalized = c.to_ascii_lowercase();
    let (text, style) = match normalized.as_str() {
        "high" => (
            "high",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        "medium" => ("med", Style::default().fg(Color::Yellow)),
        "low" => ("low", Style::default().fg(Color::Red)),
        _ => (normalized.as_str(), Style::default()),
    };
    Cell::from(text.to_string()).style(style)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

fn relative_time(dt: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let mins = (now - dt).num_minutes();
    if mins < 2 {
        "just now".into()
    } else if mins < 60 {
        format!("{mins}m ago")
    } else if mins < 60 * 24 {
        format!("{}h ago", mins / 60)
    } else {
        format!("{}d ago", (now - dt).num_days())
    }
}

/// Render the synth single-pane summary as a list of lines. The shipped rubric
/// version is included so a mismatch is obvious side-by-side.
pub fn render_synth_summary(
    ticket_id: u64,
    state: &InboxStateSummary,
    shipped_rubric_version: &str,
) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(format!("Ticket: ZD-{ticket_id}"));
    lines.push(format!(
        "Fork:   {} · Confidence: {} · Status: {}",
        state.fork.clone().unwrap_or_else(|| "—".into()),
        state.confidence.clone().unwrap_or_else(|| "—".into()),
        state.status.clone().unwrap_or_else(|| "open".into()),
    ));
    lines.push(format!(
        "Owner:  {}",
        state.owner.clone().unwrap_or_else(|| "(unowned)".into())
    ));
    lines.push(String::new());

    lines.push("Quoted rubric row:".to_string());
    let row = state
        .quoted_rubric_row
        .clone()
        .unwrap_or_else(|| "(none)".into());
    lines.push(format!("  \"{}\"", row));
    lines.push(format!(
        "  rubric_version on STATE.md: {}",
        state
            .rubric_version
            .clone()
            .unwrap_or_else(|| "(unset)".into())
    ));
    lines.push(format!(
        "  shipped rubric_version:    {shipped_rubric_version}"
    ));
    lines.push(String::new());

    lines.push("Related:".to_string());
    let zd = if state.related_zendesk.is_empty() {
        "(none)".to_string()
    } else {
        state
            .related_zendesk
            .iter()
            .map(|i| format!("#{i}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    lines.push(format!("  Zendesk: {zd}"));
    let jira = if state.related_jira.is_empty() {
        "(none)".to_string()
    } else {
        state.related_jira.join(", ")
    };
    lines.push(format!("  Jira:    {jira}"));
    lines.push(format!(
        "  Master:  {}",
        state
            .master
            .map(|i| format!("#{i}"))
            .unwrap_or_else(|| "(none)".into()),
    ));
    if let Some(c) = &state.cluster {
        lines.push(format!("  Cluster: {c}"));
    }

    if !state.validator_warnings.is_empty() {
        lines.push(String::new());
        lines.push("Validator soft-warnings (accepted):".to_string());
        for w in &state.validator_warnings {
            lines.push(format!("  · {w}"));
        }
    }
    lines
}

/// Render the rubric-version mismatch banner line.
pub fn rubric_mismatch_banner(state_version: &str, shipped_version: &str) -> String {
    format!("⚠ Rubric version mismatch: state={state_version}, shipped={shipped_version}")
}

/// Returns true when the on-disk `STATE.md` `rubric_version` does not match the
/// shipped rubric's version. Missing `rubric_version` is treated as no mismatch.
pub fn rubric_mismatch(state: &InboxStateSummary, shipped_rubric_version: &str) -> bool {
    state
        .rubric_version
        .as_deref()
        .is_some_and(|v| v != shipped_rubric_version)
}

fn read_file_for_display(path: &Path) -> String {
    match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => format!(
            "(could not read {}: {e})",
            path.file_name().and_then(|s| s.to_str()).unwrap_or("file"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::inbox::parse_state_md_str;

    fn sample_state_md() -> &'static str {
        r#"---
ticket_id: 44671
fork: B
confidence: medium
quoted_rubric_row: "customer LAN, switch, or SDWAN. Link to site master ticket"
rubric_version: "2026-04-30"
owner: "alice@example.com"
created_at: 2026-05-13T07:32:11Z
updated_at: 2026-05-13T07:32:11Z
status: open
related:
  zendesk: [43874, 42708]
  jira: ["REP-1234", "REP-5678"]
  master: null
cluster: "jeffcom-network-error"
validator_warnings: ["quoted_rubric_row paraphrased"]
---
"#
    }

    #[test]
    fn synth_summary_contains_fork_confidence_rubric_owner_status_and_related() {
        let s = parse_state_md_str(sample_state_md());
        let out = render_synth_summary(44671, &s, "2026-05-13");
        let joined = out.join("\n");
        assert!(joined.contains("ZD-44671"));
        assert!(joined.contains("Fork:   B"));
        assert!(joined.contains("Confidence: medium"));
        assert!(joined.contains("Status: open"));
        assert!(joined.contains("alice@example.com"));
        assert!(joined.contains("customer LAN, switch, or SDWAN"));
        assert!(joined.contains("#43874"));
        assert!(joined.contains("#42708"));
        assert!(joined.contains("REP-1234"));
    }

    #[test]
    fn synth_summary_surfaces_both_rubric_versions() {
        let s = parse_state_md_str(sample_state_md());
        let out = render_synth_summary(44671, &s, "2026-05-13").join("\n");
        assert!(out.contains("STATE.md: 2026-04-30"));
        assert!(out.contains("shipped rubric_version:    2026-05-13"));
    }

    #[test]
    fn synth_summary_handles_no_related() {
        let text = "---\nticket_id: 1\nfork: A\nconfidence: high\nowner: x@y\nstatus: open\n---\n";
        let s = parse_state_md_str(text);
        let out = render_synth_summary(1, &s, "2026-05-13").join("\n");
        assert!(out.contains("Zendesk: (none)"));
        assert!(out.contains("Jira:    (none)"));
        assert!(out.contains("Master:  (none)"));
    }

    #[test]
    fn rubric_mismatch_detects_drift() {
        let s = parse_state_md_str(sample_state_md());
        assert!(rubric_mismatch(&s, "2026-05-13"));
    }

    #[test]
    fn rubric_mismatch_quiet_on_match() {
        let s = parse_state_md_str(sample_state_md());
        assert!(!rubric_mismatch(&s, "2026-04-30"));
    }

    #[test]
    fn rubric_mismatch_quiet_when_version_unset() {
        let s = InboxStateSummary {
            rubric_version: None,
            ..Default::default()
        };
        assert!(!rubric_mismatch(&s, "2026-05-13"));
    }

    #[test]
    fn rubric_mismatch_banner_names_both_versions() {
        let line = rubric_mismatch_banner("2026-04-30", "2026-05-13");
        assert!(line.contains("2026-04-30"));
        assert!(line.contains("2026-05-13"));
        assert!(line.contains("Rubric version mismatch"));
    }
}
