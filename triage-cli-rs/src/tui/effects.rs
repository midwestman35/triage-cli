//! Optional visual effects facade for the inbox TUI.
//!
//! The current implementation is deliberately inert. It gives `inbox` a stable
//! integration point for future TachyonFX-backed animations without coupling
//! the human-facing TUI to a specific effects crate or Ratatui version.

use std::time::Duration;

/// Inbox-local effects controller.
#[derive(Debug, Default)]
pub struct InboxEffects {
    wants_animation_frame: bool,
}

impl InboxEffects {
    /// Build the reduced-motion/default facade: no animation, no buffer changes.
    pub fn disabled() -> Self {
        Self::default()
    }

    /// Advance any active effects. The no-op facade intentionally never asks
    /// the event loop for faster redraws.
    pub fn tick(&mut self, _elapsed: Duration) {
        self.wants_animation_frame = false;
    }

    /// Whether the event loop should prefer a short frame timeout.
    pub fn wants_animation_frame(&self) -> bool {
        self.wants_animation_frame
    }

    /// Apply effects after widgets have rendered.
    pub fn render(&mut self, _frame: &mut ratatui::Frame) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};
    use std::time::Duration;

    #[test]
    fn disabled_effects_do_not_request_animation_or_mutate_buffer() {
        let mut effects = InboxEffects::disabled();
        let mut terminal = Terminal::new(TestBackend::new(20, 4)).unwrap();

        effects.tick(Duration::from_millis(120));

        terminal
            .draw(|frame| {
                let before = frame.buffer_mut().clone();
                effects.render(frame);
                assert_eq!(&before, frame.buffer_mut());
            })
            .unwrap();
        assert!(!effects.wants_animation_frame());
    }
}
