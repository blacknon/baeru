use crate::{
    keymap::KeyMapper,
    model::{EffectKind, Feature, Rgb, Runtime, Theme, ALT_SCREEN_ENTER_SEQUENCES},
    support::{exit_with_status, sleep_frame, spawn_direct},
    theme::{indexed_color, SgrRewriter},
};
use anyhow::{anyhow, Result};
use crossterm::{
    cursor::{Hide, Show},
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, size, Clear, ClearType, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
#[cfg(unix)]
use signal_hook::{consts::signal::SIGWINCH, iterator::Signals};
use std::{
    collections::HashMap,
    ffi::OsString,
    io::{self, IsTerminal, Read, Write},
    sync::mpsc,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

pub(crate) fn run_tui_backend(rt: Runtime) -> Result<()> {
    if rt.features.contains(&Feature::LiveRender) {
        return run_live_render(rt);
    }
    if rt.features.contains(&Feature::Splash) {
        return run_splash(rt);
    }
    if rt.features.contains(&Feature::Reveal) {
        return run_reveal(rt);
    }
    let theme = rt
        .features
        .contains(&Feature::LiveColor)
        .then_some(rt.theme.clone());
    let keymap = if rt.features.contains(&Feature::Keymap) {
        rt.keymap.clone()
    } else {
        HashMap::new()
    };
    run_pty_passthrough(rt, theme, keymap)
}

fn run_splash(rt: Runtime) -> Result<()> {
    let guard = TerminalGuard::enter(true)?;
    let mut out = io::stdout();
    execute!(out, Hide, Clear(ClearType::All))?;
    let logo = "baeru";
    for frame in 0..rt.frames {
        let ratio = frame as f32 / rt.frames.saturating_sub(1).max(1) as f32;
        execute!(out, crossterm::cursor::MoveTo(0, 0), Clear(ClearType::All))?;
        write!(out, "{}\r\n", coalesce_text(logo, ratio, rt.effect))?;
        write!(out, "making terminal apps glow up...\r\n")?;
        out.flush()?;
        sleep_frame(rt.duration_ms, rt.frames);
    }
    execute!(out, Show, Clear(ClearType::All))?;
    drop(guard);
    spawn_direct(&rt.command)
}

fn run_live_render(rt: Runtime) -> Result<()> {
    let (cols, rows) = size().unwrap_or((80, 24));
    let mut session = PtySession::spawn(&rt.command, rows, cols)?;
    let _guard = TerminalGuard::enter(true)?;

    let writer = Arc::new(Mutex::new(session.writer));
    let _input_handle = spawn_input_forwarder(writer.clone(), rt.keymap.clone());

    let mut parser = vt100::Parser::new(rows, cols, 0);
    let mut prev: Option<Vec<Vec<StyledCell>>> = None;
    let mut buf = [0u8; 8192];
    let mut stdout = io::stdout();
    execute!(
        stdout,
        Clear(ClearType::All),
        crossterm::cursor::MoveTo(0, 0)
    )?;

    loop {
        match session.reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                parser.process(&buf[..n]);
                let current = collect_screen(parser.screen(), rows, cols, Some(&rt.theme));
                let changed = diff_screen(prev.as_ref(), &current, rows as usize, cols as usize);
                draw_live_screen(
                    &mut stdout,
                    &current,
                    &changed,
                    rows as usize,
                    cols as usize,
                    &rt.theme,
                )?;
                prev = Some(current);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }

    let status = session.child.wait()?;
    exit_with_status(status.exit_code() as i32);
}

fn run_reveal(rt: Runtime) -> Result<()> {
    let (cols, rows) = size().unwrap_or((80, 24));
    let mut session = PtySession::spawn(&rt.command, rows, cols)?;
    let mut parser = vt100::Parser::new(rows, cols, 0);
    let deadline = Instant::now() + Duration::from_millis(rt.capture_ms);
    let mut buf = [0u8; 8192];
    let mut captured = Vec::new();
    while Instant::now() < deadline {
        match session.reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                parser.process(&buf[..n]);
                captured.extend_from_slice(&buf[..n]);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }

    let emulate_alt_screen = contains_alt_screen_enter_sequence(&captured);
    if emulate_alt_screen {
        execute!(io::stdout(), EnterAlternateScreen)?;
    }

    let theme = if rt.no_theme_after_reveal || !rt.features.contains(&Feature::LiveColor) {
        None
    } else {
        Some(rt.theme.clone())
    };
    let screen = collect_screen(parser.screen(), rows, cols, theme.as_ref());
    animate_styled_reveal(&screen, rows, cols, rt.frames, rt.duration_ms, rt.effect)?;

    let keymap = if rt.features.contains(&Feature::Keymap) {
        rt.keymap.clone()
    } else {
        HashMap::new()
    };
    let initial_output = if emulate_alt_screen {
        strip_alt_screen_enter_sequences(&captured)
    } else {
        captured
    };
    continue_passthrough(
        session,
        theme,
        keymap,
        Some(initial_output),
        emulate_alt_screen,
    )
}

fn run_pty_passthrough(
    rt: Runtime,
    theme: Option<Theme>,
    keymap: HashMap<Vec<u8>, Vec<u8>>,
) -> Result<()> {
    let (cols, rows) = size().unwrap_or((80, 24));
    let session = PtySession::spawn(&rt.command, rows, cols)?;
    continue_passthrough(session, theme, keymap, None, false)
}

struct PtySession {
    _pty: Arc<Mutex<Box<dyn portable_pty::MasterPty + Send>>>,
    _resize_watcher: ResizeWatcher,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
}

impl PtySession {
    fn spawn(command: &[OsString], rows: u16, cols: u16) -> Result<Self> {
        if command.is_empty() {
            return Err(anyhow!("no command specified"));
        }
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let mut cmd = CommandBuilder::new(&command[0]);
        for arg in &command[1..] {
            cmd.arg(arg);
        }
        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);
        let master = pair.master;
        let reader = master.try_clone_reader()?;
        let writer = master.take_writer()?;
        let pty = Arc::new(Mutex::new(master));
        let resize_watcher = ResizeWatcher::spawn(pty.clone());
        Ok(Self {
            _pty: pty,
            _resize_watcher: resize_watcher,
            child,
            reader,
            writer,
        })
    }
}

fn continue_passthrough(
    mut session: PtySession,
    theme: Option<Theme>,
    keymap: HashMap<Vec<u8>, Vec<u8>>,
    initial_output: Option<Vec<u8>>,
    leave_alt_screen_on_exit: bool,
) -> Result<()> {
    let use_raw = io::stdin().is_terminal() && io::stdout().is_terminal();
    let _guard = TerminalGuard::enter(use_raw)?;
    let writer = Arc::new(Mutex::new(session.writer));
    let _input_handle = spawn_input_forwarder(writer.clone(), keymap);

    let mut out = io::stdout();
    let mut rewriter = theme.map(SgrRewriter::new);
    if let Some(initial) = initial_output.as_deref() {
        if let Some(rw) = rewriter.as_mut() {
            let bytes = rw.feed(initial);
            out.write_all(&bytes)?;
        } else {
            out.write_all(initial)?;
        }
        out.flush()?;
    }
    let mut buf = [0u8; 8192];
    loop {
        match session.reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if let Some(rw) = rewriter.as_mut() {
                    let bytes = rw.feed(&buf[..n]);
                    out.write_all(&bytes)?;
                } else {
                    out.write_all(&buf[..n])?;
                }
                out.flush()?;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }

    let status = session.child.wait()?;
    if leave_alt_screen_on_exit {
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
    exit_with_status(status.exit_code() as i32);
}

fn contains_alt_screen_enter_sequence(bytes: &[u8]) -> bool {
    ALT_SCREEN_ENTER_SEQUENCES.iter().any(|pattern| {
        bytes
            .windows(pattern.len())
            .any(|window| window == *pattern)
    })
}

fn strip_alt_screen_enter_sequences(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    for pattern in ALT_SCREEN_ENTER_SEQUENCES {
        out = strip_byte_sequence(&out, pattern);
    }
    out
}

fn strip_byte_sequence(bytes: &[u8], needle: &[u8]) -> Vec<u8> {
    if needle.is_empty() {
        return bytes.to_vec();
    }

    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if i + needle.len() <= bytes.len() && &bytes[i..i + needle.len()] == needle {
            i += needle.len();
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

struct TerminalGuard {
    raw_mode_enabled: bool,
    cursor_hidden: bool,
}

const KEYMAP_PENDING_TIMEOUT: Duration = Duration::from_millis(35);

fn spawn_input_forwarder(
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    keymap: HashMap<Vec<u8>, Vec<u8>>,
) -> thread::JoinHandle<io::Result<()>> {
    thread::spawn(move || -> io::Result<()> {
        let (tx, rx) = mpsc::channel::<Vec<u8>>();

        let _reader_handle = thread::spawn(move || -> io::Result<()> {
            let mut stdin = io::stdin();
            let mut buf = [0u8; 1024];
            loop {
                let n = stdin.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                if tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
            Ok(())
        });

        let mut mapper = KeyMapper::new(keymap);
        loop {
            match rx.recv_timeout(KEYMAP_PENDING_TIMEOUT) {
                Ok(chunk) => {
                    let mapped = mapper.push_bytes(&chunk);
                    if !mapped.is_empty() {
                        let mut w = writer.lock().unwrap();
                        w.write_all(&mapped)?;
                        w.flush()?;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let mapped = mapper.flush_pending();
                    if !mapped.is_empty() {
                        let mut w = writer.lock().unwrap();
                        w.write_all(&mapped)?;
                        w.flush()?;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let mapped = mapper.flush_pending();
                    if !mapped.is_empty() {
                        let mut w = writer.lock().unwrap();
                        w.write_all(&mapped)?;
                        w.flush()?;
                    }
                    break;
                }
            }
        }
        Ok(())
    })
}

struct ResizeWatcher {
    #[cfg(unix)]
    handle: signal_hook::iterator::Handle,
    join: Option<thread::JoinHandle<()>>,
}

impl ResizeWatcher {
    fn spawn(pty: Arc<Mutex<Box<dyn portable_pty::MasterPty + Send>>>) -> Self {
        #[cfg(unix)]
        {
            let mut signals = Signals::new([SIGWINCH]).expect("failed to register SIGWINCH");
            let handle = signals.handle();
            let join = thread::spawn(move || {
                for _ in signals.forever() {
                    if let Ok((cols, rows)) = size() {
                        let _ = pty.lock().unwrap().resize(PtySize {
                            rows,
                            cols,
                            pixel_width: 0,
                            pixel_height: 0,
                        });
                    }
                }
            });
            Self {
                handle,
                join: Some(join),
            }
        }

        #[cfg(not(unix))]
        {
            let _ = pty;
            Self { join: None }
        }
    }
}

impl Drop for ResizeWatcher {
    fn drop(&mut self) {
        #[cfg(unix)]
        self.handle.close();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl TerminalGuard {
    fn enter(raw_mode: bool) -> Result<Self> {
        if raw_mode {
            enable_raw_mode()?;
        }
        execute!(io::stdout(), Hide)?;
        Ok(Self {
            raw_mode_enabled: raw_mode,
            cursor_hidden: true,
        })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if self.cursor_hidden {
            let _ = execute!(io::stdout(), Show, crossterm::style::ResetColor);
        }
        if self.raw_mode_enabled {
            let _ = disable_raw_mode();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StyledCell {
    text: String,
    fg: Rgb,
    bg: Rgb,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    inverse: bool,
    wide_continuation: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RenderStyle {
    fg: Rgb,
    bg: Rgb,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
}

fn collect_screen(
    screen: &vt100::Screen,
    rows: u16,
    cols: u16,
    theme: Option<&Theme>,
) -> Vec<Vec<StyledCell>> {
    let mut result = Vec::new();
    for row in 0..rows {
        let mut out_row = Vec::new();
        for col in 0..cols {
            if let Some(cell) = screen.cell(row, col) {
                let fg = display_vt_color(theme, cell.fgcolor(), true);
                let bg = display_vt_color(theme, cell.bgcolor(), false);
                out_row.push(StyledCell {
                    text: if cell.has_contents() {
                        cell.contents().to_string()
                    } else {
                        " ".to_string()
                    },
                    fg,
                    bg,
                    bold: cell.bold(),
                    dim: cell.dim(),
                    italic: cell.italic(),
                    underline: cell.underline(),
                    inverse: cell.inverse(),
                    wide_continuation: cell.is_wide_continuation(),
                });
            } else {
                out_row.push(StyledCell::blank(theme));
            }
        }
        result.push(out_row);
    }
    result
}

impl StyledCell {
    fn blank(theme: Option<&Theme>) -> Self {
        Self {
            text: " ".to_string(),
            fg: theme.map_or(Rgb(255, 255, 255), Theme::default_fg_rgb),
            bg: theme.map_or(Rgb(0, 0, 0), Theme::default_bg_rgb),
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            inverse: false,
            wide_continuation: false,
        }
    }
}

fn display_vt_color(theme: Option<&Theme>, color: vt100::Color, foreground: bool) -> Rgb {
    if let Some(theme) = theme {
        return theme.map_vt_color(color, foreground);
    }

    match color {
        vt100::Color::Default => {
            if foreground {
                Rgb(255, 255, 255)
            } else {
                Rgb(0, 0, 0)
            }
        }
        vt100::Color::Idx(idx) => indexed_color(idx),
        vt100::Color::Rgb(r, g, b) => Rgb(r, g, b),
    }
}

fn animate_styled_reveal(
    cells: &[Vec<StyledCell>],
    rows: u16,
    cols: u16,
    frames: usize,
    duration_ms: u64,
    effect: EffectKind,
) -> Result<()> {
    let _guard = TerminalGuard::enter(true)?;
    let mut out = io::stdout();
    execute!(out, Clear(ClearType::All), crossterm::cursor::MoveTo(0, 0))?;
    for frame in 0..frames {
        let ratio = frame as f32 / frames.saturating_sub(1).max(1) as f32;
        execute!(out, crossterm::cursor::MoveTo(0, 0))?;
        for (r, row) in cells.iter().enumerate().take(rows as usize) {
            let mut style_state = None;
            for (c, cell) in row.iter().enumerate().take(cols as usize) {
                if cell.wide_continuation {
                    continue;
                }
                let text = reveal_text_for_cell(cell, r, c, frame, ratio, effect);
                write_cell(&mut out, cell, text, &mut style_state)?;
            }
            write!(out, "\x1b[0m")?;
            if r + 1 < rows as usize {
                write!(out, "\r\n")?;
            }
        }
        out.flush()?;
        sleep_frame(duration_ms, frames);
    }
    execute!(out, crossterm::cursor::MoveTo(0, 0))?;
    for (r, row) in cells.iter().enumerate() {
        let mut style_state = None;
        for cell in row {
            if !cell.wide_continuation {
                write_cell(&mut out, cell, &cell.text, &mut style_state)?;
            }
        }
        write!(out, "\x1b[0m")?;
        if r + 1 < rows as usize {
            write!(out, "\r\n")?;
        }
    }
    out.flush()?;
    Ok(())
}

fn reveal_text_for_cell(
    cell: &StyledCell,
    row: usize,
    col: usize,
    frame: usize,
    ratio: f32,
    effect: EffectKind,
) -> &str {
    match effect {
        EffectKind::Plain => &cell.text,
        EffectKind::Sweep => {
            let width_gate = (col as f32 + 1.0) / ((col + 8) as f32);
            if ratio >= width_gate.clamp(0.0, 1.0) {
                &cell.text
            } else {
                " "
            }
        }
        EffectKind::Fade => {
            if ratio >= 0.65 || cell.text.trim().is_empty() {
                &cell.text
            } else {
                " "
            }
        }
        EffectKind::Coalesce => {
            let appear_at = ((row * 17 + col * 7) % 100) as f32 / 100.0;
            let reveal = ((ratio - appear_at * 0.55) / 0.45).clamp(0.0, 1.0);
            if reveal >= 0.98 {
                &cell.text
            } else if reveal <= 0.05 || cell.text.trim().is_empty() {
                " "
            } else {
                reveal_noise_symbol(row, col, frame)
            }
        }
    }
}

fn reveal_noise_symbol(row: usize, col: usize, frame: usize) -> &'static str {
    // Use width-stable ASCII symbols here. Ambiguous-width Unicode glyphs
    // can wobble in some terminals/fonts during full-screen redraw.
    const SYMBOLS: [&str; 8] = [".", ":", "+", "*", "#", "%", "@", "="];
    SYMBOLS[(row + col + frame) % SYMBOLS.len()]
}

fn diff_screen(
    prev: Option<&Vec<Vec<StyledCell>>>,
    current: &[Vec<StyledCell>],
    rows: usize,
    cols: usize,
) -> Vec<Vec<bool>> {
    let mut changed = vec![vec![false; cols]; rows];
    let Some(prev) = prev else {
        for row in &mut changed {
            for cell in row {
                *cell = true;
            }
        }
        return changed;
    };
    for (r, row) in changed.iter_mut().enumerate().take(rows) {
        for (c, cell_changed) in row.iter_mut().enumerate().take(cols) {
            *cell_changed = prev.get(r).and_then(|prev_row| prev_row.get(c))
                != current.get(r).and_then(|current_row| current_row.get(c));
        }
    }
    changed
}

fn draw_live_screen(
    out: &mut io::Stdout,
    cells: &[Vec<StyledCell>],
    changed: &[Vec<bool>],
    rows: usize,
    cols: usize,
    theme: &Theme,
) -> io::Result<()> {
    execute!(out, crossterm::cursor::MoveTo(0, 0))?;
    let flash_fg = theme.default_bg_rgb();
    let flash_bg = theme.default_fg_rgb();
    for r in 0..rows {
        let mut style_state = None;
        for c in 0..cols {
            let cell = &cells[r][c];
            if cell.wide_continuation {
                continue;
            }
            if changed[r][c] && !cell.text.trim().is_empty() {
                let mut flash = cell.clone();
                flash.fg = flash_fg;
                flash.bg = flash_bg;
                flash.bold = true;
                write_cell(out, &flash, &cell.text, &mut style_state)?;
            } else {
                write_cell(out, cell, &cell.text, &mut style_state)?;
            }
        }
        write!(out, "\x1b[0m")?;
        if r + 1 < rows {
            write!(out, "\r\n")?;
        }
    }
    out.flush()
}

fn write_cell(
    out: &mut io::Stdout,
    cell: &StyledCell,
    text: &str,
    style_state: &mut Option<RenderStyle>,
) -> io::Result<()> {
    let (fg, bg) = if cell.inverse {
        (cell.bg, cell.fg)
    } else {
        (cell.fg, cell.bg)
    };
    let next_style = RenderStyle {
        fg,
        bg,
        bold: cell.bold,
        dim: cell.dim,
        italic: cell.italic,
        underline: cell.underline,
    };
    if style_state.as_ref() != Some(&next_style) {
        write!(
            out,
            "\x1b[0m\x1b[38;2;{};{};{}m\x1b[48;2;{};{};{}m",
            next_style.fg.0,
            next_style.fg.1,
            next_style.fg.2,
            next_style.bg.0,
            next_style.bg.1,
            next_style.bg.2
        )?;
        if next_style.bold {
            write!(out, "\x1b[1m")?;
        }
        if next_style.dim {
            write!(out, "\x1b[2m")?;
        }
        if next_style.italic {
            write!(out, "\x1b[3m")?;
        }
        if next_style.underline {
            write!(out, "\x1b[4m")?;
        }
        *style_state = Some(next_style);
    }
    write!(out, "{}", text)
}

fn coalesce_text(text: &str, ratio: f32, effect: EffectKind) -> String {
    match effect {
        EffectKind::Plain => text.to_string(),
        EffectKind::Sweep => {
            let count = text.chars().count().max(1);
            let visible = (ratio * count as f32).ceil() as usize;
            text.chars()
                .enumerate()
                .map(|(idx, ch)| if idx < visible { ch } else { ' ' })
                .collect()
        }
        EffectKind::Fade => {
            if ratio >= 0.6 {
                text.to_string()
            } else {
                " ".repeat(text.chars().count())
            }
        }
        EffectKind::Coalesce => {
            let symbols = ['.', ':', '+', '*', '#', '%', '@'];
            text.chars()
                .enumerate()
                .map(|(idx, ch)| {
                    let threshold = (idx as f32 / text.chars().count().max(1) as f32) * 0.5;
                    if ratio > threshold {
                        ch
                    } else {
                        symbols[(idx + (ratio * 100.0) as usize) % symbols.len()]
                    }
                })
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_alternate_screen_sequences() {
        assert!(contains_alt_screen_enter_sequence(b"\x1b[?1049hhello"));
        assert!(contains_alt_screen_enter_sequence(b"\x1b[?47hhello"));
        assert!(!contains_alt_screen_enter_sequence(b"\x1b[31mhello"));
    }

    #[test]
    fn strips_alternate_screen_sequences_from_capture() {
        let stripped = strip_alt_screen_enter_sequences(b"\x1b[?1049habc\x1b[?47hdef");

        assert_eq!(stripped, b"abcdef".to_vec());
    }

    #[test]
    fn strip_byte_sequence_removes_all_occurrences() {
        let stripped = strip_byte_sequence(b"xxSTARTmiddlexxSTARTtail", b"START");

        assert_eq!(stripped, b"xxmiddlexxtail".to_vec());
    }
}
