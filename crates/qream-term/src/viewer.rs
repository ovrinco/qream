use crate::{Screen, Update, pane::State, screen};
use parking_lot::Mutex;
use std::sync::Arc;

/// Polling coalesces damage against the last delivered frame. There is no
/// unbounded output queue; a slow viewer recovers with its current/full frame.
pub struct Viewer {
    state: Arc<Mutex<State>>,
    cols: u16,
    rows: u16,
    anchor: Option<u64>,
    epoch: u64,
    last: Option<Screen>,
}
impl Viewer {
    pub(crate) fn new(state: Arc<Mutex<State>>, cols: u16, rows: u16) -> Self {
        let epoch = state.lock().epoch;
        Self {
            state,
            cols: cols.clamp(1, 4096),
            rows: rows.clamp(1, 4096),
            anchor: None,
            epoch,
            last: None,
        }
    }
    /// Absolute distance into primary scrollback; zero resumes live following.
    /// Existing anchored content remains fixed until it falls out of the ring.
    pub fn scroll(&mut self, offset: usize) {
        let state = self.state.lock();
        let offset = offset.min(state.history_len);
        self.anchor = if offset == 0 {
            None
        } else {
            Some(state.scroll_total.saturating_sub(offset as u64))
        };
        self.epoch = state.epoch;
    }
    pub fn offset(&self) -> usize {
        let state = self.state.lock();
        if self.epoch != state.epoch {
            return 0;
        }
        self.anchor.map_or(0, |a| {
            state
                .scroll_total
                .saturating_sub(a)
                .min(state.history_len as u64) as usize
        })
    }
    /// Viewer geometry affects only cropping/padding, never the shared PTY.
    pub fn set_size(&mut self, cols: u16, rows: u16) {
        self.cols = cols.clamp(1, 4096);
        self.rows = rows.clamp(1, 4096);
        self.last = None;
    }
    pub fn full(&mut self) -> Screen {
        let current = self.snapshot();
        self.last = Some(current.clone());
        current
    }
    pub fn next_update(&mut self) -> Update {
        let current = self.snapshot();
        let reset = self
            .last
            .as_ref()
            .is_none_or(|s| s.cols != current.cols || s.rows.len() != current.rows.len());
        let rows = current
            .rows
            .iter()
            .filter(|row| {
                reset
                    || self
                        .last
                        .as_ref()
                        .and_then(|s| s.rows.get(usize::from(row.y)))
                        != Some(*row)
            })
            .cloned()
            .collect();
        let update = Update {
            rows,
            cursor: current.cursor.clone(),
            scrollback_len: current.scrollback_len,
            modes: current.modes.clone(),
            reset,
            cols: current.cols,
            height: current.rows.len() as u16,
        };
        self.last = Some(current);
        update
    }
    fn snapshot(&mut self) -> Screen {
        let mut state = self.state.lock();
        if self.epoch != state.epoch {
            self.anchor = None;
            self.epoch = state.epoch;
        }
        let offset = self.anchor.map_or(0, |a| {
            state
                .scroll_total
                .saturating_sub(a)
                .min(state.history_len as u64) as usize
        });
        let alternate = state.parser.screen().alternate_screen();
        // The private offset is applied only for this locked render. No viewer
        // observes it, and the parser sentinel is restored before output runs.
        state
            .parser
            .screen_mut()
            .set_scrollback(if alternate { 0 } else { offset });
        let result = screen::render(
            state.parser.screen(),
            self.cols,
            self.rows,
            state.history_len,
        );
        state.parser.screen_mut().set_scrollback(1);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mode_only_damage_and_full_baseline() {
        let state = Arc::new(Mutex::new(State {
            parser: vt100::Parser::new(3, 10, 10_000),
            history_len: 0,
            scroll_total: 0,
            epoch: 0,
            exit: None,
            fault: None,
            generation: 0,
        }));
        let mut viewer = Viewer::new(Arc::clone(&state), 10, 3);
        viewer.full();
        assert!(viewer.next_update().rows.is_empty());
        state.lock().parser.process(b"\x1b[?2004h");
        let update = viewer.next_update();
        assert!(update.rows.is_empty());
        assert!(update.modes.bracketed_paste);
        assert!(!update.reset);
        viewer.set_size(12, 4);
        let reset = viewer.next_update();
        assert!(reset.reset);
        assert_eq!(reset.rows.len(), 4);
    }
}
