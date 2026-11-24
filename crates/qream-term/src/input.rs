//! Input encoders are pure. Only an explicit caller action writes to a pane.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseMode {
    #[default]
    None,
    Press,
    PressRelease,
    ButtonMotion,
    AnyMotion,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseEncoding {
    #[default]
    Default,
    Utf8,
    Sgr,
}
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Modes {
    pub bracketed_paste: bool,
    pub application_cursor: bool,
    pub alternate_screen: bool,
    pub mouse: MouseMode,
    pub mouse_encoding: MouseEncoding,
}
impl From<&vt100::Screen> for Modes {
    fn from(s: &vt100::Screen) -> Self {
        Self {
            bracketed_paste: s.bracketed_paste(),
            application_cursor: s.application_cursor(),
            alternate_screen: s.alternate_screen(),
            mouse: match s.mouse_protocol_mode() {
                vt100::MouseProtocolMode::None => MouseMode::None,
                vt100::MouseProtocolMode::Press => MouseMode::Press,
                vt100::MouseProtocolMode::PressRelease => MouseMode::PressRelease,
                vt100::MouseProtocolMode::ButtonMotion => MouseMode::ButtonMotion,
                vt100::MouseProtocolMode::AnyMotion => MouseMode::AnyMotion,
            },
            mouse_encoding: match s.mouse_protocol_encoding() {
                vt100::MouseProtocolEncoding::Default => MouseEncoding::Default,
                vt100::MouseProtocolEncoding::Utf8 => MouseEncoding::Utf8,
                vt100::MouseProtocolEncoding::Sgr => MouseEncoding::Sgr,
            },
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}
impl Modifiers {
    fn number(self) -> u8 {
        1 + u8::from(self.shift) + 2 * u8::from(self.alt) + 4 * u8::from(self.ctrl)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Key {
    Text(String),
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Right,
    Left,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    Function(u8),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyEvent {
    pub key: Key,
    pub modifiers: Modifiers,
}
impl KeyEvent {
    pub fn encode(&self, modes: &Modes) -> Vec<u8> {
        let m = self.modifiers;
        let n = m.number();
        let mut prefix_alt = false;
        let text = match &self.key {
            Key::Text(text) => {
                prefix_alt = true;
                if m.ctrl && text.chars().count() == 1 {
                    let ch = text.chars().next().unwrap_or_default();
                    let code = match ch {
                        'a'..='z' => Some(ch as u8 - b'a' + 1),
                        '@'..='_' => Some(ch as u8 & 31),
                        ' ' | '2' => Some(0),
                        '3' => Some(27),
                        '4' => Some(28),
                        '5' => Some(29),
                        '6' => Some(30),
                        '7' => Some(31),
                        '?' | '8' => Some(127),
                        _ => None,
                    };
                    if let Some(code) = code {
                        String::from(char::from(code))
                    } else {
                        text.clone()
                    }
                } else {
                    text.clone()
                }
            }
            Key::Enter => {
                prefix_alt = true;
                "\r".into()
            }
            Key::Tab => {
                prefix_alt = true;
                if m.shift {
                    "\x1b[Z".into()
                } else {
                    "\t".into()
                }
            }
            Key::Backspace => {
                prefix_alt = true;
                if m.ctrl { "\x08".into() } else { "\x7f".into() }
            }
            Key::Escape => {
                prefix_alt = true;
                "\x1b".into()
            }
            Key::Up | Key::Down | Key::Right | Key::Left | Key::Home | Key::End => {
                let end = match self.key {
                    Key::Up => 'A',
                    Key::Down => 'B',
                    Key::Right => 'C',
                    Key::Left => 'D',
                    Key::Home => 'H',
                    _ => 'F',
                };
                if n > 1 {
                    format!("\x1b[1;{n}{end}")
                } else if modes.application_cursor {
                    format!("\x1bO{end}")
                } else {
                    format!("\x1b[{end}")
                }
            }
            Key::PageUp | Key::PageDown | Key::Insert | Key::Delete => {
                let code = match self.key {
                    Key::PageUp => 5,
                    Key::PageDown => 6,
                    Key::Insert => 2,
                    _ => 3,
                };
                tilde(code, n)
            }
            Key::Function(f) => match f {
                1..=4 => {
                    let end = char::from(b'P' + f - 1);
                    if n > 1 {
                        format!("\x1b[1;{n}{end}")
                    } else {
                        format!("\x1bO{end}")
                    }
                }
                5..=12 => tilde([15, 17, 18, 19, 20, 21, 23, 24][usize::from(f - 5)], n),
                _ => String::new(),
            },
        };
        let mut bytes = text.into_bytes();
        if prefix_alt && m.alt {
            bytes.insert(0, 27);
        }
        bytes
    }
}
fn tilde(code: u8, n: u8) -> String {
    if n > 1 {
        format!("\x1b[{code};{n}~")
    } else {
        format!("\x1b[{code}~")
    }
}
pub fn paste(text: &[u8], modes: &Modes) -> Vec<u8> {
    if !modes.bracketed_paste {
        return text.to_vec();
    }
    let mut result = b"\x1b[200~".to_vec();
    result.extend_from_slice(text);
    result.extend_from_slice(b"\x1b[201~");
    result
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    Motion,
}
/// Coordinates are zero-based. Buttons: 0 left, 1 middle, 2 right, 3 none,
/// 64 wheel up, 65 wheel down. A disabled/unsupported event emits no bytes.
#[derive(Debug, Clone, Copy)]
pub struct MouseEvent {
    pub x: u16,
    pub y: u16,
    pub button: u8,
    pub action: MouseAction,
    pub modifiers: Modifiers,
}
impl MouseEvent {
    pub fn encode(self, m: &Modes) -> Vec<u8> {
        match (m.mouse, self.action) {
            (MouseMode::None, _)
            | (MouseMode::Press, MouseAction::Release | MouseAction::Motion)
            | (MouseMode::PressRelease, MouseAction::Motion) => return Vec::new(),
            (MouseMode::ButtonMotion, MouseAction::Motion) if self.button == 3 => {
                return Vec::new();
            }
            _ => {}
        }
        let mut button = u16::from(self.button);
        if self.action == MouseAction::Motion {
            button += 32;
        }
        button += 4 * u16::from(self.modifiers.shift)
            + 8 * u16::from(self.modifiers.alt)
            + 16 * u16::from(self.modifiers.ctrl);
        let (x, y) = (u32::from(self.x) + 1, u32::from(self.y) + 1);
        if m.mouse_encoding == MouseEncoding::Sgr {
            let end = if self.action == MouseAction::Release {
                'm'
            } else {
                'M'
            };
            return format!("\x1b[<{button};{x};{y}{end}").into_bytes();
        }
        if self.action == MouseAction::Release {
            button = 3 + (button & 28);
        }
        let mut result = b"\x1b[M".to_vec();
        for v in [u32::from(button) + 32, x + 32, y + 32] {
            if m.mouse_encoding == MouseEncoding::Utf8 {
                let Some(ch) = char::from_u32(v) else {
                    return Vec::new();
                };
                let mut buffer = [0; 4];
                result.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
            } else {
                let Ok(byte) = u8::try_from(v) else {
                    return Vec::new();
                };
                result.push(byte);
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keys_and_modifier_chords() {
        let modes = Modes::default();
        let plain = Modifiers::default();
        for (key, expected) in [
            (Key::Enter, b"\r".as_slice()),
            (Key::Tab, b"\t"),
            (Key::Backspace, b"\x7f"),
            (Key::Up, b"\x1b[A"),
            (Key::Down, b"\x1b[B"),
            (Key::Left, b"\x1b[D"),
            (Key::Right, b"\x1b[C"),
            (Key::Home, b"\x1b[H"),
            (Key::End, b"\x1b[F"),
            (Key::PageUp, b"\x1b[5~"),
            (Key::PageDown, b"\x1b[6~"),
            (Key::Insert, b"\x1b[2~"),
            (Key::Delete, b"\x1b[3~"),
            (Key::Function(1), b"\x1bOP"),
            (Key::Function(12), b"\x1b[24~"),
        ] {
            assert_eq!(
                KeyEvent {
                    key,
                    modifiers: plain
                }
                .encode(&modes),
                expected
            );
        }
        let chord = Modifiers {
            ctrl: true,
            alt: true,
            shift: false,
        };
        assert_eq!(
            KeyEvent {
                key: Key::Text("c".into()),
                modifiers: chord
            }
            .encode(&modes),
            b"\x1b\x03"
        );
        assert_eq!(
            KeyEvent {
                key: Key::Up,
                modifiers: chord
            }
            .encode(&modes),
            b"\x1b[1;7A"
        );
        assert_eq!(
            KeyEvent {
                key: Key::Function(5),
                modifiers: chord
            }
            .encode(&modes),
            b"\x1b[15;7~"
        );
        assert_eq!(
            KeyEvent {
                key: Key::Text("é界".into()),
                modifiers: plain
            }
            .encode(&modes),
            "é界".as_bytes()
        );
    }
    #[test]
    fn mouse_protocols_filter_motion_and_encode_release() {
        let mut modes = Modes::default();
        let press = MouseEvent {
            x: 2,
            y: 1,
            button: 0,
            action: MouseAction::Press,
            modifiers: Modifiers::default(),
        };
        assert!(press.encode(&modes).is_empty());
        modes.mouse = MouseMode::PressRelease;
        assert_eq!(press.encode(&modes), b"\x1b[M #\x22");
        modes.mouse_encoding = MouseEncoding::Sgr;
        assert_eq!(
            MouseEvent {
                action: MouseAction::Release,
                ..press
            }
            .encode(&modes),
            b"\x1b[<0;3;2m"
        );
        assert!(
            MouseEvent {
                action: MouseAction::Motion,
                ..press
            }
            .encode(&modes)
            .is_empty()
        );
        modes.mouse = MouseMode::ButtonMotion;
        assert_eq!(
            MouseEvent {
                action: MouseAction::Motion,
                ..press
            }
            .encode(&modes),
            b"\x1b[<32;3;2M"
        );
        assert!(
            MouseEvent {
                action: MouseAction::Motion,
                button: 3,
                ..press
            }
            .encode(&modes)
            .is_empty()
        );
        modes.mouse = MouseMode::AnyMotion;
        assert_eq!(
            MouseEvent {
                action: MouseAction::Motion,
                button: 3,
                ..press
            }
            .encode(&modes),
            b"\x1b[<35;3;2M"
        );
        modes.mouse = MouseMode::Press;
        assert!(
            MouseEvent {
                action: MouseAction::Release,
                ..press
            }
            .encode(&modes)
            .is_empty()
        );
    }
}
