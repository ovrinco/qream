use crate::{
    Error, Result, Viewer,
    input::{self, KeyEvent, Modes, MouseEvent},
};
use parking_lot::Mutex;
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize};
use std::{collections::BTreeMap, io::Read, path::PathBuf, sync::Arc};

pub const SCROLLBACK_LINES: usize = 10_000;
#[derive(Debug, Clone)]
pub struct PtySpec {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub cols: u16,
    pub rows: u16,
}
/// Only the pane owner holds PTY input/resize handles. Viewers hold screen state.
pub struct Pane {
    pub(crate) state: Arc<Mutex<State>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: crate::writer::Writer,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    pid: Option<u32>,
    cwd: PathBuf,
}
pub(crate) struct State {
    pub parser: vt100::Parser,
    pub history_len: usize,
    pub scroll_total: u64,
    pub epoch: u64,
    pub exit: Option<i32>,
    pub fault: Option<Error>,
    /// Bumped on every fed chunk so the session layer can journal without a viewer.
    pub generation: u64,
}
impl State {
    fn feed(&mut self, bytes: &[u8]) {
        self.generation = self.generation.wrapping_add(1);
        // vt100 advances a nonzero history offset on every full-screen scroll,
        // even after its ring is full. Reset that sentinel after each byte so
        // ring eviction and multiple CSI scrolls still have an exact count.
        for byte in bytes {
            let before = self.parser.screen().scrollback();
            let was_alt = self.parser.screen().alternate_screen();
            self.parser.process(&[*byte]);
            let alt = self.parser.screen().alternate_screen();
            let after = self.parser.screen().scrollback();
            self.parser.screen_mut().set_scrollback(1);
            let sentinel = self.parser.screen().scrollback();
            if !was_alt && !alt {
                if before > 0 && after == 0 {
                    self.epoch += 1;
                }
                self.scroll_total += if before > 0 {
                    after.saturating_sub(before) as u64
                } else {
                    sentinel as u64
                };
            }
        }
        // Probe the ring length under the same lock, then restore the sentinel.
        // Keep primary history length while the alternate screen is active.
        if !self.parser.screen().alternate_screen() {
            self.parser.screen_mut().set_scrollback(usize::MAX);
            self.history_len = self.parser.screen().scrollback();
            self.parser.screen_mut().set_scrollback(1);
        }
    }
}
impl Pane {
    pub fn spawn(spec: PtySpec) -> Result<Self> {
        Self::spawn_with_history(spec, &[])
    }
    /// Spawn with restored history fed to the screen before the child's first byte,
    /// so a pane rebuilt from its journal keeps its scrollback ahead of new output.
    pub fn spawn_with_history(spec: PtySpec, history: &[String]) -> Result<Self> {
        validate_size(spec.cols, spec.rows)?;
        let Some(program) = spec.argv.first() else {
            return Err(Error::Invalid("argv must contain a program".into()));
        };
        if program.is_empty() {
            return Err(Error::Invalid("program must not be empty".into()));
        }
        crate::privacy::check_spawn(&spec)?;
        let pty_error = |operation, source| Error::Pty {
            operation,
            cwd: spec.cwd.clone(),
            source,
        };
        let pair = portable_pty::native_pty_system()
            .openpty(size(spec.cols, spec.rows))
            .map_err(|e| pty_error("open", e.into()))?;
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| pty_error("reader", e.into()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| pty_error("writer", e.into()))?;
        let mut cmd = CommandBuilder::new(program);
        cmd.args(&spec.argv[1..]);
        cmd.cwd(&spec.cwd);
        // A pane never inherits the daemon's own environment (CONTRACTS §2, 22:4x): only the
        // allow-list passes, then the keys the engine set for this pane.
        cmd.env_clear();
        for (key, value) in std::env::vars().filter(|(k, _)| pane_env_allowed(k)) {
            cmd.env(key, value);
        }
        for (key, value) in &spec.env {
            cmd.env(key, value);
        }
        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| pty_error("spawn", e.into()))?;
        drop(pair.slave);
        let pid = child.process_id();
        let mut killer = child.clone_killer();
        let state = Arc::new(Mutex::new(State {
            parser: vt100::Parser::new(spec.rows, spec.cols, SCROLLBACK_LINES),
            history_len: 0,
            scroll_total: 0,
            epoch: 0,
            exit: None,
            fault: None,
            generation: 0,
        }));
        {
            let mut seeded = state.lock();
            for line in history {
                seeded.feed(line.as_bytes());
                seeded.feed(b"\r\n");
            }
        }
        let wait_state = Arc::clone(&state);
        let wait_path = spec.cwd.clone();
        if let Err(e) = std::thread::Builder::new()
            .name("qream-pane-child".into())
            .spawn(move || match child.wait() {
                Ok(status) => wait_state.lock().exit = Some(status.exit_code() as i32),
                Err(e) => wait_state.lock().fault = Some(Error::io("wait for child", wait_path, e)),
            })
        {
            let _ = killer.kill();
            return Err(Error::io("start child waiter", &spec.cwd, e));
        }
        let read_state = Arc::clone(&state);
        let read_path = spec.cwd.clone();
        if let Err(e) = std::thread::Builder::new()
            .name("qream-pane-reader".into())
            .spawn(move || {
                let mut buffer = [0; 8192];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(n) => read_state.lock().feed(&buffer[..n]),
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        // Linux PTYs return EIO when the last slave closes.
                        Err(e) if e.raw_os_error() == Some(5) => break,
                        Err(e) => {
                            read_state.lock().fault = Some(Error::io("read PTY", &read_path, e));
                            break;
                        }
                    }
                }
            })
        {
            let _ = killer.kill();
            return Err(Error::io("start PTY reader", &spec.cwd, e));
        }
        let writer = match crate::writer::Writer::start(writer, spec.cwd.clone()) {
            Ok(writer) => writer,
            Err(e) => {
                let _ = killer.kill();
                return Err(e);
            }
        };
        Ok(Self {
            state,
            master: Mutex::new(pair.master),
            writer,
            killer: Mutex::new(killer),
            pid,
            cwd: spec.cwd,
        })
    }
    pub fn write(&self, bytes: &[u8]) -> Result<()> {
        self.writer.write(bytes)
    }
    /// One queue item keeps text, pacing and submit indivisible from user input.
    pub fn enqueue_prompt(&self, text: &str) -> Result<()> {
        self.writer.prompt(text)
    }
    pub fn modes(&self) -> Modes {
        Modes::from(self.state.lock().parser.screen())
    }
    pub fn key(&self, event: &KeyEvent) -> Result<()> {
        self.write(&event.encode(&self.modes()))
    }
    pub fn paste(&self, text: &[u8]) -> Result<()> {
        self.write(&input::paste(text, &self.modes()))
    }
    pub fn mouse(&self, event: MouseEvent) -> Result<()> {
        self.write(&event.encode(&self.modes()))
    }
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        validate_size(cols, rows)?;
        // Serialize the kernel resize and parser size with reader updates.
        let mut state = self.state.lock();
        self.master
            .lock()
            .resize(size(cols, rows))
            .map_err(|e| Error::Pty {
                operation: "resize",
                cwd: self.cwd.clone(),
                source: e.into(),
            })?;
        state.parser.screen_mut().set_size(rows, cols);
        Ok(())
    }
    pub fn subscribe(&self, cols: u16, rows: u16) -> Viewer {
        Viewer::new(Arc::clone(&self.state), cols, rows)
    }
    /// End the child now; the waiter reaps it. Used by the owning session only.
    pub fn kill(&self) {
        if self.exited().is_none() {
            let _ = self.killer.lock().kill();
        }
    }
    pub fn exited(&self) -> Option<i32> {
        self.state.lock().exit
    }
    pub fn take_error(&self) -> Option<Error> {
        self.writer
            .take_error()
            .or_else(|| self.state.lock().fault.take())
    }
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }
    /// Root and descendants, including children of every thread, read from procfs.
    /// A child that disappears between reads is legitimately absent.
    pub fn child_pids(&self) -> Result<Vec<u32>> {
        let Some(root) = self.pid else {
            return Ok(Vec::new());
        };
        if self.exited().is_some() {
            return Ok(Vec::new());
        }
        let mut pending = vec![root];
        let mut result = Vec::new();
        while let Some(pid) = pending.pop() {
            if result.contains(&pid) {
                continue;
            }
            let path = PathBuf::from(format!("/proc/{pid}/task"));
            let entries = match std::fs::read_dir(&path) {
                Ok(entries) => entries,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(Error::io("list process threads", path, e)),
            };
            result.push(pid);
            for entry in entries {
                let entry = entry.map_err(|e| Error::io("read process thread", &path, e))?;
                let children_path = entry.path().join("children");
                match std::fs::read_to_string(&children_path) {
                    Ok(text) => {
                        for pid in text
                            .split_whitespace()
                            .filter_map(|s| s.parse::<u32>().ok())
                        {
                            pending.push(pid);
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(Error::io("read process children", children_path, e)),
                }
            }
        }
        result.sort_unstable();
        Ok(result)
    }
    /// Output counter; it changes whenever the PTY has produced bytes.
    pub fn generation(&self) -> u64 {
        self.state.lock().generation
    }
    /// Primary history (oldest first) and the live screen rows, both trimmed.
    pub fn lines(&self, max_history: usize) -> (Vec<String>, Vec<String>) {
        let state = self.state.lock();
        let mut screen = state.parser.screen().clone();
        let (rows, cols) = screen.size();
        screen.set_scrollback(0);
        let mut live: Vec<String> = screen
            .rows(0, cols)
            .map(|s| s.trim_end().to_owned())
            .collect();
        while live.last().is_some_and(String::is_empty) {
            live.pop();
        }
        let wanted = if screen.alternate_screen() {
            0
        } else {
            state.history_len.min(max_history)
        };
        let mut history = Vec::with_capacity(wanted);
        let mut remaining = wanted;
        while remaining > 0 {
            screen.set_scrollback(remaining);
            let count = remaining.min(usize::from(rows));
            history.extend(
                screen
                    .rows(0, cols)
                    .take(count)
                    .map(|s| s.trim_end().to_owned()),
            );
            remaining -= count;
        }
        (history, live)
    }
    /// The full styled screen for restoration and pane events.
    pub fn snapshot(&self) -> crate::Screen {
        let state = self.state.lock();
        let (rows, cols) = state.parser.screen().size();
        let mut screen = state.parser.screen().clone();
        screen.set_scrollback(0);
        crate::screen::render(&screen, cols, rows, state.history_len)
    }
    pub fn text(&self, last_n_lines: usize) -> String {
        if last_n_lines == 0 {
            return String::new();
        }
        let state = self.state.lock();
        let mut screen = state.parser.screen().clone();
        let (rows, cols) = screen.size();
        screen.set_scrollback(0);
        let mut live: Vec<String> = screen
            .rows(0, cols)
            .map(|s| s.trim_end().to_owned())
            .collect();
        while live.last().is_some_and(String::is_empty) {
            live.pop();
        }
        let needed = last_n_lines
            .saturating_sub(live.len())
            .min(if screen.alternate_screen() {
                0
            } else {
                state.history_len
            });
        let mut history = Vec::new();
        // Bounded windows avoid extracting the entire ring for a short read.
        let mut remaining = needed;
        while remaining > 0 {
            screen.set_scrollback(remaining);
            let count = remaining.min(usize::from(rows));
            history.extend(
                screen
                    .rows(0, cols)
                    .take(count)
                    .map(|s| s.trim_end().to_owned()),
            );
            remaining -= count;
        }
        history.extend(live);
        history[history.len().saturating_sub(last_n_lines)..].join("\n")
    }
}
impl Drop for Pane {
    fn drop(&mut self) {
        // A viewer's drop never reaches here. The owning session dropping a pane
        // ends its own child; the waiter remains responsible for reaping it.
        if self.exited().is_none() {
            let _ = self.killer.get_mut().kill();
        }
    }
}
fn size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        cols,
        rows,
        pixel_width: 0,
        pixel_height: 0,
    }
}
fn validate_size(cols: u16, rows: u16) -> Result<()> {
    if cols == 0 || rows == 0 || cols > 4096 || rows > 4096 {
        return Err(Error::Invalid("dimensions must be in 1..=4096".into()));
    }
    Ok(())
}

/// Whether a variable of the daemon's process may reach a pane it spawns.
pub fn pane_env_allowed(key: &str) -> bool {
    const DROP_PREFIX: &[&str] = &["CLAUDE_CODE_", "CODEX_"];
    const DROP_SUFFIX: &[&str] = &["_TOKEN", "_KEY", "_SECRET"];
    const KEEP: &[&str] = &[
        "PATH",
        "HOME",
        "USER",
        "SHELL",
        "LANG",
        "TERM",
        "COLORTERM",
        "WAYLAND_DISPLAY",
        "DISPLAY",
        "DBUS_SESSION_BUS_ADDRESS",
        "CLAUDE_CONFIG_DIR",
        "OPENCODE_CONFIG",
        "OPENCODE_CONFIG_DIR",
    ];
    if DROP_PREFIX.iter().any(|p| key.starts_with(p))
        || DROP_SUFFIX.iter().any(|s| key.ends_with(s))
    {
        return false;
    }
    KEEP.contains(&key) || key.starts_with("LC_") || key.starts_with("XDG_")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn test_state(rows: u16, cols: u16) -> Arc<Mutex<State>> {
        Arc::new(Mutex::new(State {
            parser: vt100::Parser::new(rows, cols, SCROLLBACK_LINES),
            history_len: 0,
            scroll_total: 0,
            epoch: 0,
            exit: None,
            fault: None,
            generation: 0,
        }))
    }
    #[test]
    fn ring_cap_styled_history_and_eviction_anchor() {
        let state = test_state(3, 20);
        state.lock().feed(b"\x1b[31mred\x1b[0m\r\n");
        for i in 0..10_020 {
            state.lock().feed(format!("line-{i:05}\r\n").as_bytes());
        }
        assert_eq!(state.lock().history_len, 10_000);
        let mut viewer = Viewer::new(Arc::clone(&state), 20, 3);
        viewer.scroll(100);
        let held = viewer.full().rows;
        for _ in 0..30 {
            state.lock().feed(b"new\r\n");
        }
        assert_eq!(viewer.full().rows, held);
        assert_eq!(viewer.offset(), 130);
        viewer.scroll(10_000);
        assert_eq!(viewer.offset(), 10_000);
        state.lock().feed(b"evict\r\n");
        assert_eq!(
            viewer.offset(),
            10_000,
            "evicted anchor clamps to oldest retained row"
        );
        // Styled rows themselves survive in the ring, not a plain text log.
        let styled = test_state(3, 20);
        styled
            .lock()
            .feed(b"\x1b[1;31mred\x1b[0m\r\n2\r\n3\r\n4\r\n");
        let mut v = Viewer::new(styled, 20, 3);
        v.scroll(2);
        assert_eq!(v.full().rows[0].runs[0].fg, crate::Color::Indexed(1));
        assert!(v.full().rows[0].runs[0].bold);
    }
    #[test]
    fn alternate_screen_preserves_private_primary_history() {
        let state = test_state(3, 20);
        state.lock().feed(b"a\r\nb\r\nc\r\nd\r\ne\r\n");
        let mut view = Viewer::new(Arc::clone(&state), 20, 3);
        view.scroll(2);
        let held = view.full();
        state
            .lock()
            .feed(b"\x1b[?1049hALT\r\n1\r\n2\r\n3\x1b[?1049l");
        assert_eq!(view.full().rows, held.rows);
        assert_eq!(view.offset(), 2);
        state.lock().feed(b"f\r\n");
        assert_eq!(view.full().rows, held.rows);
        assert_eq!(view.offset(), 3);
    }
    #[test]
    fn unicode_runs_inverse_colours_and_history_reset() {
        let state = test_state(3, 20);
        state
            .lock()
            .feed("\x1b[38;2;1;2;3;48;5;22;7m界e\u{301}\x1b[0m\r\n2\r\n3\r\n4\r\n".as_bytes());
        let mut view = Viewer::new(Arc::clone(&state), 20, 3);
        view.scroll(2);
        let full = view.full();
        let run = &full.rows[0].runs[0];
        assert_eq!(run.text, "界e\u{301}");
        assert_eq!(run.fg, crate::Color::Indexed(22));
        assert_eq!(run.bg, crate::Color::Rgb(1, 2, 3));
        state.lock().feed(b"\x1bc");
        assert_eq!(view.offset(), 0);
        assert_eq!(view.full().scrollback_len, 0);
    }
}
