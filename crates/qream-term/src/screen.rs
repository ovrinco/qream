use crate::input::Modes;
use serde::{Deserialize, Serialize};

/// Defaults and indexed colours are kept symbolic for the window's palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Color {
    Default,
    /// Inverse video: resolve against the palette's default foreground.
    DefaultForeground,
    /// Inverse video: resolve against the palette's default background.
    DefaultBackground,
    Indexed(u8),
    Rgb(u8, u8, u8),
}
impl From<vt100::Color> for Color {
    fn from(value: vt100::Color) -> Self {
        match value {
            vt100::Color::Default => Self::Default,
            vt100::Color::Idx(i) => Self::Indexed(i),
            vt100::Color::Rgb(r, g, b) => Self::Rgb(r, g, b),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub text: String,
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}
impl Run {
    fn same_style(&self, other: &Self) -> bool {
        self.fg == other.fg
            && self.bg == other.bg
            && self.bold == other.bold
            && self.italic == other.italic
            && self.underline == other.underline
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub y: u16,
    pub runs: Vec<Run>,
}
impl Row {
    pub fn text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub x: u16,
    pub y: u16,
    pub visible: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Screen {
    pub cols: u16,
    pub rows: Vec<Row>,
    pub cursor: Cursor,
    pub scrollback_len: usize,
    pub modes: Modes,
}
/// A coalesced update; an empty rows vector is a legitimate cursor/mode update.
/// Consumers replace the frame on reset, otherwise replace only listed y rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Update {
    pub rows: Vec<Row>,
    pub cursor: Cursor,
    pub scrollback_len: usize,
    pub modes: Modes,
    pub reset: bool,
    pub cols: u16,
    pub height: u16,
}
pub(crate) fn render(screen: &vt100::Screen, cols: u16, rows: u16, history: usize) -> Screen {
    let mut result = Vec::with_capacity(rows as usize);
    for y in 0..rows {
        let mut runs: Vec<Run> = Vec::new();
        for x in 0..cols {
            let Some(cell) = screen.cell(y, x) else {
                append(
                    &mut runs,
                    Run {
                        text: " ".into(),
                        fg: Color::Default,
                        bg: Color::Default,
                        bold: false,
                        italic: false,
                        underline: false,
                    },
                );
                continue;
            };
            if cell.is_wide_continuation() {
                continue;
            }
            let (mut fg, mut bg) = (cell.fgcolor().into(), cell.bgcolor().into());
            if cell.inverse() {
                if fg == Color::Default {
                    fg = Color::DefaultForeground;
                }
                if bg == Color::Default {
                    bg = Color::DefaultBackground;
                }
                std::mem::swap(&mut fg, &mut bg);
            }
            append(
                &mut runs,
                Run {
                    text: if cell.is_wide() && x + 1 == cols {
                        // Crop a wide glyph to an empty cell at the view edge.
                        " ".into()
                    } else if cell.has_contents() {
                        cell.contents().into()
                    } else {
                        " ".into()
                    },
                    fg,
                    bg,
                    bold: cell.bold(),
                    italic: cell.italic(),
                    underline: cell.underline(),
                },
            );
        }
        result.push(Row { y, runs });
    }
    let (y, x) = screen.cursor_position();
    Screen {
        cols,
        rows: result,
        cursor: Cursor {
            x,
            y,
            visible: !screen.hide_cursor() && screen.scrollback() == 0 && y < rows && x < cols,
        },
        scrollback_len: history,
        modes: Modes::from(screen),
    }
}
fn append(runs: &mut Vec<Run>, run: Run) {
    if let Some(last) = runs.last_mut()
        && last.same_style(&run)
    {
        last.text.push_str(&run.text);
    } else {
        runs.push(run);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inverse_defaults_and_wide_crop_are_renderable() {
        let mut parser = vt100::Parser::new(2, 8, 0);
        parser.process("\x1b[7m界\x1b[0m".as_bytes());
        let full = render(parser.screen(), 8, 2, 0);
        assert_eq!(full.rows[0].runs[0].fg, Color::DefaultBackground);
        assert_eq!(full.rows[0].runs[0].bg, Color::DefaultForeground);
        assert_eq!(render(parser.screen(), 1, 2, 0).rows[0].text(), " ");
    }
}
