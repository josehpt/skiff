//! `skiff peek`: the session's screen as plain text, without attaching.

use alacritty_terminal::{
    event::VoidListener,
    grid::Dimensions,
    index::{Column, Line},
    term::{cell::Flags, test::TermSize, Config, Term},
    vte::ansi::Processor,
};
use anyhow::{bail, Result};
use skiff_client::Client;
use skiff_core::protocol::Event;

use crate::{resolve, sessions};

/// History the local screen keeps. skiffd keeps 2000 lines.
const SCROLLBACK: usize = 10_000;

/// Prints the visible screen, or with `lines` the last that many lines of
/// scrollback and screen. Trailing blank lines are dropped.
pub async fn peek(c: &Client, arg: &str, lines: Option<usize>) -> Result<()> {
    let all = sessions::sorted(c).await?;
    let id = resolve::session(&all, arg)?.id.clone();
    for line in screen(c, &id, lines).await? {
        println!("{line}");
    }
    Ok(())
}

/// The screen text of session `id`, as `peek` prints it.
pub async fn screen(c: &Client, id: &str, lines: Option<usize>) -> Result<Vec<String>> {
    let (data, cols, rows) = snapshot(c, id).await?;
    Ok(render(&data, cols, rows, lines))
}

/// The bytes that redraw session `id`, and its size.
pub async fn snapshot(c: &Client, id: &str) -> Result<(Vec<u8>, u16, u16)> {
    let mut out = c.output(id);
    c.subscribe(id).await?;
    let snap = loop {
        match out.recv().await {
            Ok(Event::Snapshot { data, cols, rows, .. }) => break (data, cols, rows),
            Ok(_) => continue,
            Err(_) => bail!("no snapshot from skiffd"),
        }
    };
    let _ = c.unsubscribe(id).await;
    Ok(snap)
}

/// A local terminal fed with `bytes`: the same emulator skiffd uses.
pub fn term(bytes: &[u8], cols: u16, rows: u16) -> Term<VoidListener> {
    let config = Config {
        scrolling_history: SCROLLBACK,
        ..Config::default()
    };
    let size = TermSize::new(cols.max(1) as usize, rows.max(1) as usize);
    let mut term = Term::new(config, &size, VoidListener);
    let mut parser: Processor = Processor::new();
    parser.advance(&mut term, bytes);
    term
}

/// Runs the snapshot through the same emulator skiffd uses and reads the
/// text back out of its grid.
fn render(bytes: &[u8], cols: u16, rows: u16, lines: Option<usize>) -> Vec<String> {
    let term = term(bytes, cols, rows);
    let grid = term.grid();
    let top = if lines.is_some() { grid.topmost_line().0 } else { 0 };
    let bottom = grid.bottommost_line().0;
    let mut text: Vec<String> = (top..=bottom)
        .map(|l| {
            let row = &grid[Line(l)];
            let mut s = String::new();
            for c in 0..grid.columns() {
                let cell = &row[Column(c)];
                if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    continue;
                }
                s.push(if cell.c == '\0' { ' ' } else { cell.c });
                if let Some(zw) = cell.zerowidth() {
                    s.extend(zw);
                }
            }
            s.trim_end().to_string()
        })
        .collect();
    while text.last().is_some_and(String::is_empty) {
        text.pop();
    }
    if let Some(n) = lines {
        let skip = text.len().saturating_sub(n);
        text.drain(..skip);
    }
    text
}

#[cfg(test)]
mod tests {
    #[test]
    fn renders_text_and_drops_escapes() {
        let snap = b"\x1bc\x1b[?2026hone\r\n\x1b[31mred\x1b[0m\r\n\x1b[2;4H\x1b[?2026l";
        assert_eq!(super::render(snap, 20, 5, None), ["one", "red"]);
        assert_eq!(super::render(snap, 20, 5, Some(1)), ["red"]);
    }
}
