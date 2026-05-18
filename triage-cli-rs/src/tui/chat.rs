//! Inbox chat pane (spec § 6). Transcript view above; input modal +
//! command bar below. V1 ships static throbber + plain path prompt;
//! file picker, $EDITOR integration, and animated gradient spinner are
//! deferred to v2 (see `docs/ROADMAP.md` item #1 V2 list).

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};
use tui_textarea::TextArea;

use crate::models::{Turn, TurnKind};

const ANALYST_HEADER: Color = Color::Rgb(0x7e, 0xc8, 0xff);
const CODEX_HEADER: Color = Color::Rgb(0x6f, 0xdc, 0x8c);
const SYSTEM_HEADER: Color = Color::Rgb(0xff, 0xb8, 0x6c);
const AUTOMATED_HEADER: Color = Color::Rgb(0xbd, 0x93, 0xf9);
const CMD_KEY: Color = Color::Rgb(0x3f, 0xbf, 0x3f);
const CMD_DESC: Color = Color::Rgb(0x88, 0x88, 0x88);

const THROBBER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub struct ChatPane<'a> {
    pub turns: &'a [Turn],
    pub input: &'a TextArea<'a>,
    pub ticket_id: &'a str,
    pub in_flight: Option<InFlightState>,
}

#[derive(Debug, Clone)]
pub struct InFlightState {
    pub elapsed_s: f64,
    pub frame_idx: usize,
}

impl<'a> Widget for &ChatPane<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(5),    // transcript
                Constraint::Length(7), // input modal (5) + status (1) + cmd bar (1)
            ])
            .split(area);

        render_transcript(self.turns, chunks[0], buf);

        let lower = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(5),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(chunks[1]);
        render_input(self.input, lower[0], buf);
        render_status_line(self.in_flight.as_ref(), lower[1], buf);
        render_command_bar(lower[2], buf);
    }
}

fn render_transcript(turns: &[Turn], area: Rect, buf: &mut Buffer) {
    let mut lines: Vec<Line> = Vec::new();
    for t in turns {
        let kind = match t.turn_kind {
            TurnKind::Analyst => "analyst",
            TurnKind::Codex => "codex",
            TurnKind::System => "system",
            TurnKind::Automated => "automated",
        };
        let color = header_color(t.turn_kind);
        let mut header = format!(
            "{kind} {ts} (turn-{turn:03})",
            kind = kind,
            ts = t.ts.format("%Y-%m-%dT%H:%M:%SZ"),
            turn = t.turn,
        );
        if !t.evidence.is_empty() {
            header.push_str(&format!(" attached:{}", t.evidence.len()));
        }
        if let Some(true) = t.resumed {
            header.push_str(" resumed");
        }
        lines.push(Line::from(Span::styled(
            header,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )));
        for body_line in t.body.lines().take(20) {
            lines.push(Line::from(format!("  {body_line}")));
        }
        lines.push(Line::from(""));
    }
    let block = Block::default().borders(Borders::ALL).title(" Transcript ");
    Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .render(area, buf);
}

fn render_input(input: &TextArea, area: Rect, buf: &mut Buffer) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(input_box_title());
    let inner = block.inner(area);
    block.render(area, buf);
    input.render(inner, buf);
}

/// F11-label: title of the chat input box. "Esc" exits the chat
/// session, but only *after* an in-flight provider call returns —
/// so labelling it "cancel" misleads the user into thinking the
/// keystroke aborts the network call.
fn input_box_title() -> &'static str {
    " ASK (Ctrl-S send, Esc exit chat) "
}

/// F11-label: the throbber text shown while a provider call is in
/// flight. Previously suffixed with "(Esc to cancel)" — Esc is
/// buffered, not actioned, during the blocking await. Drop the
/// false affordance and just report elapsed time.
fn throbber_status_line(in_flight: Option<&InFlightState>) -> String {
    match in_flight {
        Some(s) => {
            let frame = THROBBER_FRAMES[s.frame_idx % THROBBER_FRAMES.len()];
            format!(" {frame} codex is thinking… {:.1}s elapsed", s.elapsed_s)
        }
        None => String::new(),
    }
}

fn render_status_line(in_flight: Option<&InFlightState>, area: Rect, buf: &mut Buffer) {
    let text = throbber_status_line(in_flight);
    Paragraph::new(text)
        .style(Style::default().fg(CODEX_HEADER))
        .render(area, buf);
}

/// F11-label: keystrokes shown in the bottom command bar. Esc is now
/// labelled "exit chat" to match what it actually does — close the
/// chat session — rather than the previous "cancel" wording that
/// implied it interrupts an in-flight provider call.
fn command_bar_labels() -> &'static [(&'static str, &'static str)] {
    &[
        ("Ctrl-S", "send"),
        ("Ctrl-F", "file"),
        ("Ctrl-V", "paste"),
        ("Ctrl-R", "/revise"),
        ("Ctrl-T", "retry"),
        ("Esc", "exit chat"),
        ("Ctrl-C", "quit"),
    ]
}

fn render_command_bar(area: Rect, buf: &mut Buffer) {
    let cmds = command_bar_labels();
    let mut spans = vec![Span::raw(" ")];
    for (i, (key, desc)) in cmds.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", Style::default().fg(CMD_DESC)));
        }
        spans.push(Span::styled(*key, Style::default().fg(CMD_KEY)));
        spans.push(Span::styled(
            format!(" {desc}"),
            Style::default().fg(CMD_DESC),
        ));
    }
    Paragraph::new(Line::from(spans)).render(area, buf);
}

fn header_color(kind: TurnKind) -> Color {
    match kind {
        TurnKind::Analyst => ANALYST_HEADER,
        TurnKind::Codex => CODEX_HEADER,
        TurnKind::System => SYSTEM_HEADER,
        TurnKind::Automated => AUTOMATED_HEADER,
    }
}

// ──────────────────────────────────────────────────────────────────────
//   ChatCommand enum + parser
// ──────────────────────────────────────────────────────────────────────

/// Slash commands recognized by the chat input modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatCommand {
    /// `/file <path>` — attach a file by path.
    File(std::path::PathBuf),
    /// `/paste <label>=<body>` — attach a labeled paste.
    Paste { label: String, body: String },
    /// `/revise` — re-run the structured pipeline.
    Revise,
    /// `/retry` — re-attempt the last failed provider call.
    Retry,
    /// `/quit` — close the chat pane.
    Quit,
    /// A plain analyst body (no leading slash).
    Body(String),
}

/// Parse the analyst's input into a `ChatCommand`. Empty input maps to
/// `Body("")` so the caller can decide whether to discard it.
pub fn parse_chat_command(raw: &str) -> ChatCommand {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("/file ") {
        return ChatCommand::File(std::path::PathBuf::from(rest.trim()));
    }
    if let Some(rest) = trimmed.strip_prefix("/paste ") {
        if let Some((label, body)) = rest.split_once('=') {
            return ChatCommand::Paste {
                label: label.trim().to_string(),
                body: body.to_string(),
            };
        }
    }
    if trimmed == "/revise" {
        return ChatCommand::Revise;
    }
    if trimmed == "/retry" {
        return ChatCommand::Retry;
    }
    if trimmed == "/quit" {
        return ChatCommand::Quit;
    }
    ChatCommand::Body(raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn sample_turn(turn: u32, kind: TurnKind, body: &str) -> Turn {
        Turn {
            schema: "triage-cli/conversation".into(),
            schema_version: 1,
            ticket_id: "44776".into(),
            turn,
            turn_kind: kind,
            ts: chrono::Utc::now(),
            author: None,
            body: body.into(),
            evidence: vec![],
            provider: None,
            model: None,
            tokens_in: None,
            tokens_out: None,
            elapsed_s: None,
            session_id: None,
            resumed: None,
            action: None,
            outcome: None,
            drove_revision_from_turns: None,
            diff: None,
        }
    }

    #[test]
    fn snapshot_chat_pane_renders_three_turns() {
        let turns = vec![
            sample_turn(1, TurnKind::Analyst, "first question"),
            sample_turn(2, TurnKind::Codex, "first answer"),
            sample_turn(3, TurnKind::System, "system note"),
        ];
        let input = TextArea::default();
        let pane = ChatPane {
            turns: &turns,
            input: &input,
            ticket_id: "44776",
            in_flight: None,
        };
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                let widget = &pane;
                f.render_widget(widget, area);
            })
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        let dump = buffer_to_strings(&buf);
        assert!(dump.iter().any(|l| l.contains("analyst")));
        assert!(dump.iter().any(|l| l.contains("codex")));
        assert!(dump.iter().any(|l| l.contains("system")));
        assert!(dump.iter().any(|l| l.contains("Ctrl-S")));
        assert!(dump.iter().any(|l| l.contains("/revise")));
    }

    fn buffer_to_strings(buf: &Buffer) -> Vec<String> {
        let area = buf.area();
        let mut out = Vec::new();
        for y in 0..area.height {
            let mut row = String::new();
            for x in 0..area.width {
                row.push_str(buf[(x, y)].symbol());
            }
            out.push(row);
        }
        out
    }

    #[test]
    fn parse_file_command() {
        assert_eq!(
            parse_chat_command("/file ./station.log"),
            ChatCommand::File(std::path::PathBuf::from("./station.log"))
        );
    }

    #[test]
    fn parse_paste_command() {
        assert_eq!(
            parse_chat_command("/paste customer-note=they rebooted"),
            ChatCommand::Paste {
                label: "customer-note".into(),
                body: "they rebooted".into(),
            }
        );
    }

    #[test]
    fn parse_revise_retry_quit() {
        assert_eq!(parse_chat_command("/revise"), ChatCommand::Revise);
        assert_eq!(parse_chat_command("/retry"), ChatCommand::Retry);
        assert_eq!(parse_chat_command("/quit"), ChatCommand::Quit);
    }

    #[test]
    fn parse_plain_body() {
        let r = parse_chat_command("what happened?");
        match r {
            ChatCommand::Body(s) => assert_eq!(s, "what happened?"),
            _ => panic!("expected Body"),
        }
    }

    /// F11-label: the chat TUI claimed "Esc to cancel" in three places —
    /// the input-box title, the in-flight throbber, and the command bar.
    /// Esc *does* close the chat session, but only after the in-flight
    /// `provider.followup().await` returns: the single-threaded event
    /// loop is blocked, so an "Esc cancel" press is buffered, not acted
    /// on. These tests pin the labels to behavior the user can actually
    /// expect.
    #[test]
    fn command_bar_labels_do_not_advertise_cancel() {
        let labels = command_bar_labels();
        let esc = labels
            .iter()
            .find(|(k, _)| *k == "Esc")
            .expect("Esc must be in the bar");
        assert!(
            !esc.1.eq_ignore_ascii_case("cancel"),
            "Esc must not claim to 'cancel' — provider calls aren't interruptible: {esc:?}"
        );
        assert!(
            !esc.1.contains("cancel"),
            "any 'cancel' wording is misleading: {esc:?}"
        );
    }

    #[test]
    fn throbber_in_flight_does_not_claim_cancel() {
        let s = InFlightState {
            elapsed_s: 2.3,
            frame_idx: 0,
        };
        let line = throbber_status_line(Some(&s));
        assert!(
            !line.contains("cancel"),
            "in-flight throbber must not advertise Esc cancel — provider call is uninterruptible: {line:?}"
        );
        // It must still display elapsed time so the user knows the call is running.
        assert!(line.contains("2.3"), "elapsed time missing: {line:?}");
    }

    #[test]
    fn throbber_with_no_in_flight_is_empty() {
        assert_eq!(throbber_status_line(None), "");
    }

    #[test]
    fn input_box_title_does_not_claim_cancel() {
        let title = input_box_title();
        assert!(
            !title.contains("cancel"),
            "input box title must not advertise cancel: {title:?}"
        );
    }
}
