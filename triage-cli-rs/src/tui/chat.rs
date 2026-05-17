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
        .title(" ASK (Ctrl-S send, Esc cancel) ");
    let inner = block.inner(area);
    block.render(area, buf);
    input.render(inner, buf);
}

fn render_status_line(in_flight: Option<&InFlightState>, area: Rect, buf: &mut Buffer) {
    let text = match in_flight {
        Some(s) => {
            let frame = THROBBER_FRAMES[s.frame_idx % THROBBER_FRAMES.len()];
            format!(
                " {frame} codex is thinking… {:.1}s elapsed (Esc to cancel)",
                s.elapsed_s
            )
        }
        None => "".to_string(),
    };
    Paragraph::new(text)
        .style(Style::default().fg(CODEX_HEADER))
        .render(area, buf);
}

fn render_command_bar(area: Rect, buf: &mut Buffer) {
    let cmds = [
        ("Ctrl-S", "send"),
        ("Ctrl-F", "file"),
        ("Ctrl-V", "paste"),
        ("Ctrl-R", "/revise"),
        ("Ctrl-T", "retry"),
        ("Esc", "cancel"),
        ("Ctrl-C", "quit"),
    ];
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
}
