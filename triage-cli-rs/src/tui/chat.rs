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

pub enum ChatInputSurface<'a> {
    Ask(&'a TextArea<'a>),
    FilePath { value: &'a str },
    PasteLine { value: &'a str },
    DirPath { value: &'a str },
}

pub struct ChatPane<'a> {
    pub turns: &'a [Turn],
    pub input: ChatInputSurface<'a>,
    pub ticket_id: &'a str,
    pub progress: Option<&'a crate::chat::ChatProgress>,
    pub status_hint: Option<&'a str>,
    pub transcript_scroll: u16,
    pub transcript_follow_bottom: bool,
}

impl<'a> Widget for &ChatPane<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let banner_h = self
            .progress
            .map(|_| banner_rows(area.height))
            .filter(|rows| *rows > 1)
            .unwrap_or(0);
        let reclaimed_banner_h = if self.progress.is_none() {
            let rows = banner_rows(area.height);
            if rows > 1 {
                rows
            } else {
                0
            }
        } else {
            0
        };
        let input_h = (if area.height >= 14 { 5 } else { 3 }) + reclaimed_banner_h;
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),
                Constraint::Length(banner_h),
                Constraint::Length(input_h),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);

        render_transcript(
            self.turns,
            self.ticket_id,
            chunks[0],
            buf,
            self.transcript_scroll,
            self.transcript_follow_bottom,
        );
        if let Some(progress) = self.progress {
            render_phase_banner(progress, chunks[1], buf);
        }
        render_input(&self.input, chunks[2], buf);
        let status_progress = if self.progress.is_some() && banner_h == 0 {
            self.progress
        } else {
            None
        };
        render_status_line(status_progress, self.status_hint, chunks[3], buf);
        render_command_bar(chunks[4], buf);
    }
}

fn banner_rows(area_height: u16) -> u16 {
    match area_height {
        h if h >= 20 => 4,
        h if h >= 14 => 3,
        h if h >= 10 => 2,
        _ => 1,
    }
}

fn render_transcript(
    turns: &[Turn],
    ticket_id: &str,
    area: Rect,
    buf: &mut Buffer,
    transcript_scroll: u16,
    transcript_follow_bottom: bool,
) {
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
    let visible_height = area.height.saturating_sub(2) as usize;
    let scroll = if transcript_follow_bottom {
        lines.len().saturating_sub(visible_height) as u16
    } else {
        transcript_scroll
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Transcript ticket {ticket_id} "));
    Paragraph::new(lines)
        .block(block)
        .scroll((scroll, 0))
        .wrap(Wrap { trim: false })
        .render(area, buf);
}

fn render_input(input: &ChatInputSurface<'_>, area: Rect, buf: &mut Buffer) {
    match input {
        ChatInputSurface::Ask(input) => {
            let block = Block::default()
                .borders(Borders::ALL)
                .title(" ASK (Ctrl-S send, Esc cancel) ");
            let inner = block.inner(area);
            block.render(area, buf);
            input.render(inner, buf);
        }
        ChatInputSurface::FilePath { value } => render_line_modal(
            " FILE PATH (Enter attach, Esc cancel) ",
            "file> ",
            value,
            area,
            buf,
        ),
        ChatInputSurface::PasteLine { value } => render_line_modal(
            " PASTE (label=body, Enter attach, Esc cancel) ",
            "paste> ",
            value,
            area,
            buf,
        ),
        ChatInputSurface::DirPath { value } => render_line_modal(
            " DIR PATH (Enter attach, Esc cancel; -r for recurse; *.log for glob) ",
            "dir> ",
            value,
            area,
            buf,
        ),
    }
}

fn render_line_modal(title: &str, prompt: &str, value: &str, area: Rect, buf: &mut Buffer) {
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    block.render(area, buf);
    Paragraph::new(vec![
        Line::from(""),
        Line::from(vec![
            Span::raw(prompt.to_string()),
            Span::raw(value.to_string()),
            Span::raw("_"),
        ]),
    ])
    .render(inner, buf);
}

fn render_status_line(
    progress: Option<&crate::chat::ChatProgress>,
    status_hint: Option<&str>,
    area: Rect,
    buf: &mut Buffer,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let (text, color) = match progress {
        Some(p) if p.elapsed_s >= 0.0 && banner_rows(area.height) == 1 => {
            let frame = THROBBER_FRAMES[p.frame_idx % THROBBER_FRAMES.len()];
            (
                format!(
                    " {frame} {} {:.1}s elapsed (Esc to cancel)",
                    p.canned_msg, p.elapsed_s
                ),
                stage_color(p.stage),
            )
        }
        _ => (status_hint.unwrap_or("").to_string(), SYSTEM_HEADER),
    };
    Paragraph::new(text)
        .style(Style::default().fg(color))
        .render(area, buf);
}

fn render_phase_banner(progress: &crate::chat::ChatProgress, area: Rect, buf: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let frame = THROBBER_FRAMES[progress.frame_idx % THROBBER_FRAMES.len()];
    let color = stage_color(progress.stage);
    let stage_label = stage_label_str(progress.stage);

    match area.height {
        4 => {
            let block = Block::default()
                .borders(Borders::ALL)
                .title(" codex follow-up ")
                .style(Style::default().fg(color));
            let inner = block.inner(area);
            block.render(area, buf);
            let session = match (progress.resumed, &progress.session_id) {
                (Some(true), Some(sid)) => format!("session: resumed (sid {sid})"),
                (Some(false), Some(sid)) => format!("session: fresh (sid {sid})"),
                (Some(_), None) => "session: in flight".to_string(),
                (None, _) => "session: pending".to_string(),
            };
            Paragraph::new(vec![
                Line::from(format!(
                    "{frame}  {}  stage: {}   elapsed {:.1}s",
                    progress.canned_msg, stage_label, progress.elapsed_s
                )),
                Line::from(session),
                Line::from("Esc cancels · Ctrl-T retries last turn"),
            ])
            .render(inner, buf);
        }
        3 => {
            let block = Block::default()
                .borders(Borders::ALL)
                .title(" codex follow-up ")
                .style(Style::default().fg(color));
            let inner = block.inner(area);
            block.render(area, buf);
            Paragraph::new(format!(
                "{frame}  {}  {:.1}s  (Esc cancel)",
                progress.canned_msg, progress.elapsed_s
            ))
            .render(inner, buf);
        }
        2 => {
            Paragraph::new(vec![
                Line::from("─".repeat(area.width as usize)),
                Line::from(format!(
                    " {frame}  {}  {:.1}s  (Esc cancel)",
                    progress.canned_msg, progress.elapsed_s
                )),
            ])
            .style(Style::default().fg(color))
            .render(area, buf);
        }
        _ => {}
    }
}

fn stage_color(stage: crate::chat::ChatStage) -> Color {
    match stage {
        crate::chat::ChatStage::Ingesting | crate::chat::ChatStage::ContextAssembled => {
            SYSTEM_HEADER
        }
        crate::chat::ChatStage::SessionResumeAttempt | crate::chat::ChatStage::ProviderAwait => {
            CODEX_HEADER
        }
        crate::chat::ChatStage::ResponseParsed | crate::chat::ChatStage::Saved => ANALYST_HEADER,
    }
}

fn stage_label_str(stage: crate::chat::ChatStage) -> &'static str {
    match stage {
        crate::chat::ChatStage::Ingesting => "ingesting",
        crate::chat::ChatStage::ContextAssembled => "context_assembled",
        crate::chat::ChatStage::SessionResumeAttempt => "session_resume",
        crate::chat::ChatStage::ProviderAwait => "provider_await",
        crate::chat::ChatStage::ResponseParsed => "response_parsed",
        crate::chat::ChatStage::Saved => "saved",
    }
}

fn render_command_bar(area: Rect, buf: &mut Buffer) {
    let cmds = [
        ("Ctrl-S", "send"),
        ("Ctrl-F", "file"),
        ("Ctrl-D", "dir"),
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

// ──────────────────────────────────────────────────────────────────────
//   ChatCommand enum + parser
// ──────────────────────────────────────────────────────────────────────

/// Slash commands recognized by the chat input modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatCommand {
    /// `/file <path>` — attach a file by path.
    File(std::path::PathBuf),
    /// `/dir <path> [-r] [glob]` — attach a directory.
    Dir {
        path: std::path::PathBuf,
        recursive: bool,
        glob: Option<String>,
    },
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
    if let Some(rest) = trimmed.strip_prefix("/dir ") {
        let mut tokens = rest.split_whitespace();
        let path = tokens.next().unwrap_or("").to_string();
        let mut recursive = false;
        let mut glob = None;
        for token in tokens {
            if token == "-r" {
                recursive = true;
            } else if glob.is_none() {
                glob = Some(token.to_string());
            }
        }
        if !path.is_empty() {
            return ChatCommand::Dir {
                path: std::path::PathBuf::from(path),
                recursive,
                glob,
            };
        }
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
            input: ChatInputSurface::Ask(&input),
            ticket_id: "44776",
            progress: None,
            status_hint: None,
            transcript_scroll: 0,
            transcript_follow_bottom: true,
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
    fn parse_dir_command_basic() {
        assert_eq!(
            parse_chat_command("/dir ./logs"),
            ChatCommand::Dir {
                path: std::path::PathBuf::from("./logs"),
                recursive: false,
                glob: None,
            }
        );
    }

    #[test]
    fn parse_dir_command_recursive_flag() {
        assert_eq!(
            parse_chat_command("/dir ./logs -r"),
            ChatCommand::Dir {
                path: std::path::PathBuf::from("./logs"),
                recursive: true,
                glob: None,
            }
        );
    }

    #[test]
    fn parse_dir_command_with_glob() {
        assert_eq!(
            parse_chat_command("/dir ./logs *.log"),
            ChatCommand::Dir {
                path: std::path::PathBuf::from("./logs"),
                recursive: false,
                glob: Some("*.log".into()),
            }
        );
    }

    #[test]
    fn parse_dir_command_recursive_and_glob() {
        assert_eq!(
            parse_chat_command("/dir ./logs -r *.log"),
            ChatCommand::Dir {
                path: std::path::PathBuf::from("./logs"),
                recursive: true,
                glob: Some("*.log".into()),
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

    fn chat_progress(stage: crate::chat::ChatStage, elapsed_s: f64) -> crate::chat::ChatProgress {
        crate::chat::ChatProgress {
            stage,
            canned_msg: crate::chat::canned_message(stage, 0),
            elapsed_s,
            frame_idx: 0,
            resumed: None,
            session_id: None,
        }
    }

    #[test]
    fn pane_renders_canned_message_for_each_stage() {
        let stages = [
            crate::chat::ChatStage::Ingesting,
            crate::chat::ChatStage::ContextAssembled,
            crate::chat::ChatStage::SessionResumeAttempt,
            crate::chat::ChatStage::ProviderAwait,
            crate::chat::ChatStage::ResponseParsed,
            crate::chat::ChatStage::Saved,
        ];
        for stage in stages {
            let progress = chat_progress(stage, 1.0);
            let input = TextArea::default();
            let pane = ChatPane {
                turns: &[],
                input: ChatInputSurface::Ask(&input),
                ticket_id: "1",
                progress: Some(&progress),
                status_hint: None,
                transcript_scroll: 0,
                transcript_follow_bottom: true,
            };
            let backend = TestBackend::new(80, 24);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(|f| f.render_widget(&pane, f.area())).unwrap();
            let joined = buffer_to_strings(terminal.backend().buffer()).join("\n");
            let expected = crate::chat::canned_message(stage, 0);
            assert!(
                joined.contains(expected),
                "expected canned message {expected:?} for {stage:?}; dump:\n{joined}"
            );
        }
    }

    #[test]
    fn pane_renders_at_four_heights_without_panic() {
        let progress = chat_progress(crate::chat::ChatStage::ProviderAwait, 2.3);
        for height in [8u16, 12, 18, 28] {
            let input = TextArea::default();
            let pane = ChatPane {
                turns: &[],
                input: ChatInputSurface::Ask(&input),
                ticket_id: "1",
                progress: Some(&progress),
                status_hint: None,
                transcript_scroll: 0,
                transcript_follow_bottom: true,
            };
            let backend = TestBackend::new(80, height);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(|f| f.render_widget(&pane, f.area())).unwrap();
            let dump = buffer_to_strings(terminal.backend().buffer());
            assert!(
                dump.iter().any(|line| line.contains("asking around")),
                "height {height}: canned text missing in dump:\n{}",
                dump.join("\n")
            );
        }
    }

    #[test]
    fn pane_without_progress_omits_banner() {
        let input = TextArea::default();
        let pane = ChatPane {
            turns: &[],
            input: ChatInputSurface::Ask(&input),
            ticket_id: "1",
            progress: None,
            status_hint: None,
            transcript_scroll: 0,
            transcript_follow_bottom: true,
        };
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| f.render_widget(&pane, f.area())).unwrap();
        let joined = buffer_to_strings(terminal.backend().buffer()).join("\n");
        assert!(
            !joined.contains("codex follow-up"),
            "banner leaked into a no-progress draw:\n{joined}"
        );
    }

    #[test]
    fn pane_without_progress_gives_banner_rows_to_input() {
        let input = TextArea::default();
        let pane = ChatPane {
            turns: &[],
            input: ChatInputSurface::Ask(&input),
            ticket_id: "1",
            progress: None,
            status_hint: None,
            transcript_scroll: 0,
            transcript_follow_bottom: true,
        };
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| f.render_widget(&pane, f.area())).unwrap();

        let dump = buffer_to_strings(terminal.backend().buffer());
        let ask_row = dump
            .iter()
            .position(|line| line.contains("ASK (Ctrl-S send, Esc cancel)"))
            .expect("ASK input title missing");

        assert_eq!(
            ask_row,
            13,
            "no-progress input should reclaim the four banner rows at height 24:\n{}",
            dump.join("\n")
        );
    }
}
