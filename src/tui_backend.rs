use crate::{
    keymap::KeyMapper,
    model::{
        EffectKind, Feature, Rgb, Runtime, Theme, ALT_SCREEN_ENTER_SEQUENCES,
        LIVE_RENDER_INPUT_MODE_DISABLE_SEQUENCES, LIVE_RENDER_INPUT_MODE_ENABLE_SEQUENCES,
        LIVE_RENDER_MOUSE_DISABLE_SEQUENCES, LIVE_RENDER_MOUSE_ENABLE_SEQUENCES,
        LIVE_RENDER_PASTE_MODE_DISABLE_SEQUENCES, LIVE_RENDER_PASTE_MODE_ENABLE_SEQUENCES,
        LIVE_RENDER_RESET_SEQUENCES,
    },
    support::{exit_with_status, sleep_frame, spawn_direct},
    theme::SgrRewriter,
};
use anyhow::{anyhow, Result};
use crossterm::{
    cursor::{Hide, Show},
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, size, Clear, ClearType, EnterAlternateScreen,
        LeaveAlternateScreen, ScrollDown, ScrollUp,
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
    let (mut cols, mut rows) = size().unwrap_or((80, 24));
    let mut session = PtySession::spawn(&rt.command, rows, cols)?;
    let _guard = TerminalGuard::enter(true)?;
    let mouse_quiet_window = Duration::from_millis(rt.live_render_mouse_quiet_ms);

    let writer = Arc::new(Mutex::new(session.writer));
    let mouse_activity = Arc::new(Mutex::new(None));
    let _input_handle = spawn_input_forwarder(
        writer.clone(),
        rt.keymap.clone(),
        Some(mouse_activity.clone()),
    );

    let mut parser = vt100::Parser::new(rows, cols, 0);
    let mut prev: Option<Vec<Vec<StyledCell>>> = None;
    let mut frame = 0usize;
    let mut mouse_reporting_active = false;
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
                if let Ok((new_cols, new_rows)) = size() {
                    if new_cols != cols || new_rows != rows {
                        cols = new_cols;
                        rows = new_rows;
                        parser = vt100::Parser::new(rows, cols, 0);
                        prev = None;
                        execute!(
                            stdout,
                            Clear(ClearType::All),
                            crossterm::cursor::MoveTo(0, 0)
                        )?;
                    }
                }
                mouse_reporting_active = forward_live_passthrough_sequences(
                    &mut stdout,
                    &buf[..n],
                    mouse_reporting_active,
                )?;
                parser.process(&buf[..n]);
                let current = collect_screen(parser.screen(), rows, cols, Some(&rt.theme));
                let state = collect_terminal_state(parser.screen(), rows, cols);
                let scroll_hint =
                    detect_scroll_hint(prev.as_ref(), &current, rows as usize, cols as usize);
                let mut changed =
                    diff_screen(prev.as_ref(), &current, rows as usize, cols as usize);
                apply_scroll_hint(&mut changed, scroll_hint, rows as usize, cols as usize);
                animate_live_render_update(
                    &mut stdout,
                    &current,
                    &state,
                    &changed,
                    &rt.theme,
                    LiveRenderFrame {
                        scroll_hint,
                        effect: if mouse_reporting_active
                            && mouse_activity_recent(&mouse_activity, mouse_quiet_window)
                        {
                            EffectKind::Plain
                        } else {
                            rt.effect
                        },
                        frame,
                        ratio: 1.0,
                    },
                    rt.live_render_duration_ms,
                )?;
                prev = Some(current);
                frame = frame.wrapping_add(1);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }

    let status = session.child.wait()?;
    reset_live_passthrough_sequences(&mut stdout)?;
    exit_with_status(status.exit_code() as i32);
}

fn animate_live_render_update(
    out: &mut io::Stdout,
    cells: &[Vec<StyledCell>],
    state: &TerminalState,
    changed: &[Vec<bool>],
    theme: &Theme,
    base_frame: LiveRenderFrame,
    duration_ms: u64,
) -> io::Result<()> {
    let ratios = live_render_effect_ratios(base_frame.effect);
    let total_ms = duration_ms.clamp(45, 120);
    let per_frame_ms = (total_ms / ratios.len().max(1) as u64).max(1);
    let full_redraw = full_screen_changed(cells);
    let no_highlight = no_screen_changed(cells);

    for (idx, ratio) in ratios.iter().copied().enumerate() {
        let redraw_mask = if idx + 1 == ratios.len() {
            &full_redraw
        } else {
            changed
        };
        let highlight_mask = if idx + 1 == ratios.len() {
            &no_highlight
        } else {
            changed
        };
        draw_live_screen(
            out,
            cells,
            state,
            redraw_mask,
            highlight_mask,
            theme,
            LiveRenderFrame {
                scroll_hint: if idx == 0 {
                    base_frame.scroll_hint
                } else {
                    None
                },
                effect: base_frame.effect,
                frame: base_frame.frame + idx,
                ratio,
            },
        )?;
        if idx + 1 < ratios.len() {
            thread::sleep(Duration::from_millis(per_frame_ms));
        }
    }

    Ok(())
}

fn live_render_effect_ratios(effect: EffectKind) -> &'static [f32] {
    match effect {
        EffectKind::Plain => &[1.0],
        EffectKind::Fade => &[0.35, 1.0],
        EffectKind::Sweep => &[0.25, 0.65, 1.0],
        EffectKind::Coalesce => &[0.12, 0.38, 0.72, 1.0],
        EffectKind::Matrix => &[0.08, 0.24, 0.45, 0.72, 1.0],
    }
}

fn full_screen_changed(cells: &[Vec<StyledCell>]) -> Vec<Vec<bool>> {
    cells.iter().map(|row| vec![true; row.len()]).collect()
}

fn no_screen_changed(cells: &[Vec<StyledCell>]) -> Vec<Vec<bool>> {
    cells.iter().map(|row| vec![false; row.len()]).collect()
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
    let _input_handle = spawn_input_forwarder(writer.clone(), keymap, None);

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

fn forward_live_passthrough_sequences(
    out: &mut io::Stdout,
    bytes: &[u8],
    mut mouse_reporting_active: bool,
) -> io::Result<bool> {
    let mut wrote_any = false;
    for pattern in LIVE_RENDER_MOUSE_ENABLE_SEQUENCES {
        for _ in bytes
            .windows(pattern.len())
            .filter(|window| *window == *pattern)
        {
            out.write_all(pattern)?;
            wrote_any = true;
            mouse_reporting_active = true;
        }
    }
    for pattern in LIVE_RENDER_MOUSE_DISABLE_SEQUENCES {
        for _ in bytes
            .windows(pattern.len())
            .filter(|window| *window == *pattern)
        {
            out.write_all(pattern)?;
            wrote_any = true;
            mouse_reporting_active = false;
        }
    }
    for pattern in LIVE_RENDER_INPUT_MODE_ENABLE_SEQUENCES {
        for _ in bytes
            .windows(pattern.len())
            .filter(|window| *window == *pattern)
        {
            out.write_all(pattern)?;
            wrote_any = true;
        }
    }
    for pattern in LIVE_RENDER_INPUT_MODE_DISABLE_SEQUENCES {
        for _ in bytes
            .windows(pattern.len())
            .filter(|window| *window == *pattern)
        {
            out.write_all(pattern)?;
            wrote_any = true;
        }
    }
    for pattern in LIVE_RENDER_PASTE_MODE_ENABLE_SEQUENCES {
        for _ in bytes
            .windows(pattern.len())
            .filter(|window| *window == *pattern)
        {
            out.write_all(pattern)?;
            wrote_any = true;
        }
    }
    for pattern in LIVE_RENDER_PASTE_MODE_DISABLE_SEQUENCES {
        for _ in bytes
            .windows(pattern.len())
            .filter(|window| *window == *pattern)
        {
            out.write_all(pattern)?;
            wrote_any = true;
        }
    }
    if wrote_any {
        out.flush()?;
    }
    Ok(mouse_reporting_active)
}

fn reset_live_passthrough_sequences(out: &mut io::Stdout) -> io::Result<()> {
    out.write_all(LIVE_RENDER_RESET_SEQUENCES)?;
    out.flush()
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
    mouse_activity: Option<Arc<Mutex<Option<Instant>>>>,
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
                    if let Some(activity) = mouse_activity.as_ref() {
                        if contains_noise_suppression_input(&chunk) {
                            *activity.lock().unwrap() = Some(Instant::now());
                        }
                    }
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

fn contains_noise_suppression_input(bytes: &[u8]) -> bool {
    contains_up_down_input(bytes) || contains_mouse_scroll_or_drag(bytes)
}

fn contains_up_down_input(bytes: &[u8]) -> bool {
    bytes.windows(3).any(|window| {
        window == b"\x1b[A" || window == b"\x1b[B" || window == b"\x1bOA" || window == b"\x1bOB"
    })
}

fn contains_mouse_scroll_or_drag(bytes: &[u8]) -> bool {
    // SGR mouse mode: wheel events use button codes 64/65, drag starts at 32.
    let text = String::from_utf8_lossy(bytes);
    if text.contains("\x1b[<64;")
        || text.contains("\x1b[<65;")
        || text.contains("\x1b[<32;")
        || text.contains("\x1b[<33;")
        || text.contains("\x1b[<34;")
        || text.contains("\x1b[<35;")
    {
        return true;
    }

    // X10/normal mouse mode: ESC [ M Cb Cx Cy
    // Wheel up/down are encoded as 96/97, drag starts at 64.
    bytes.windows(6).any(|window| {
        window.starts_with(b"\x1b[M") && matches!(window[3], 96 | 97 | 64 | 65 | 66 | 67)
    })
}

fn mouse_activity_recent(
    mouse_activity: &Arc<Mutex<Option<Instant>>>,
    quiet_window: Duration,
) -> bool {
    mouse_activity
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|instant| instant.elapsed() <= quiet_window)
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
    fg: DisplayColor,
    bg: DisplayColor,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    inverse: bool,
    wide_continuation: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RenderStyle {
    fg: DisplayColor,
    bg: DisplayColor,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DisplayColor {
    Default,
    Indexed(u8),
    Rgb(Rgb),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TerminalState {
    cursor_row: u16,
    cursor_col: u16,
    cursor_visible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScrollHint {
    Up(u16),
    Down(u16),
}

#[derive(Debug, Clone, Copy)]
struct LiveRenderFrame {
    scroll_hint: Option<ScrollHint>,
    effect: EffectKind,
    frame: usize,
    ratio: f32,
}

fn collect_terminal_state(screen: &vt100::Screen, rows: u16, cols: u16) -> TerminalState {
    let (cursor_row, cursor_col) = screen.cursor_position();
    TerminalState {
        cursor_row: cursor_row.min(rows.saturating_sub(1)),
        cursor_col: cursor_col.min(cols.saturating_sub(1)),
        cursor_visible: !screen.hide_cursor(),
    }
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
        let preserve_terminal = theme_preserves_terminal_colors(theme);
        Self {
            text: " ".to_string(),
            fg: if preserve_terminal {
                DisplayColor::Default
            } else {
                theme.map_or(DisplayColor::Rgb(Rgb(255, 255, 255)), |theme| {
                    DisplayColor::Rgb(theme.default_fg_rgb())
                })
            },
            bg: if preserve_terminal {
                DisplayColor::Default
            } else {
                theme.map_or(DisplayColor::Rgb(Rgb(0, 0, 0)), |theme| {
                    DisplayColor::Rgb(theme.default_bg_rgb())
                })
            },
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            inverse: false,
            wide_continuation: false,
        }
    }
}

fn theme_preserves_terminal_colors(theme: Option<&Theme>) -> bool {
    theme.is_none_or(|theme| {
        !theme.force_default
            && theme.palette_map.is_empty()
            && theme.background_palette_map.is_empty()
            && theme.foreground.is_empty()
            && theme.background.is_empty()
    })
}

fn display_vt_color(theme: Option<&Theme>, color: vt100::Color, foreground: bool) -> DisplayColor {
    if let Some(theme) = theme {
        if theme_preserves_terminal_colors(Some(theme)) {
            return match color {
                vt100::Color::Default => DisplayColor::Default,
                vt100::Color::Idx(idx) => DisplayColor::Indexed(idx),
                vt100::Color::Rgb(r, g, b) => DisplayColor::Rgb(Rgb(r, g, b)),
            };
        }

        return DisplayColor::Rgb(theme.map_vt_color(color, foreground));
    }

    match color {
        vt100::Color::Default => DisplayColor::Default,
        vt100::Color::Idx(idx) => DisplayColor::Indexed(idx),
        vt100::Color::Rgb(r, g, b) => DisplayColor::Rgb(Rgb(r, g, b)),
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
                let text = reveal_text_for_cell(cell, r, c, rows as usize, frame, ratio, effect);
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
    total_rows: usize,
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
        EffectKind::Matrix => {
            let state = matrix_cell_state(row, col, total_rows.max(1), ratio);
            if state >= 1.0 {
                &cell.text
            } else if cell.text.trim().is_empty() || state < 0.0 {
                " "
            } else {
                reveal_matrix_symbol(row, col, frame)
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

fn reveal_matrix_symbol(row: usize, col: usize, frame: usize) -> &'static str {
    const SYMBOLS: [&str; 8] = ["0", "1", "|", ":", ".", "+", "*", "#"];
    SYMBOLS[(row * 19 + col * 11 + frame * 5) % SYMBOLS.len()]
}

fn pseudo_random_01(a: u64, b: u64) -> f32 {
    let mut x = a.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ b.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    (x % 10_000) as f32 / 10_000.0
}

fn matrix_cell_state(row: usize, col: usize, total_rows: usize, progress: f32) -> f32 {
    let offset = pseudo_random_01(col as u64, 13) * 6.0;
    let trail = 3.5 + pseudo_random_01(col as u64, 29) * 4.5;
    let span = total_rows as f32 + trail + 6.0;
    let head = progress.clamp(0.0, 1.0) * span - offset;
    let distance = head - row as f32;
    if distance < 0.0 {
        -1.0
    } else if distance <= trail {
        0.0
    } else {
        1.0
    }
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
    state: &TerminalState,
    redraw: &[Vec<bool>],
    highlight: &[Vec<bool>],
    theme: &Theme,
    animation: LiveRenderFrame,
) -> io::Result<()> {
    match animation.scroll_hint {
        Some(ScrollHint::Up(lines)) => execute!(out, ScrollUp(lines))?,
        Some(ScrollHint::Down(lines)) => execute!(out, ScrollDown(lines))?,
        None => {}
    }
    let rows = cells.len();
    for r in 0..rows {
        if !row_has_changes(redraw, r) {
            continue;
        }

        // Redraw the whole dirty row so the terminal keeps the row background
        // in sync. Span-only redraw leaves untouched spaces transparent and the
        // underlying shell can bleed through.
        execute!(out, crossterm::cursor::MoveTo(0, r as u16))?;
        let mut style_state = None;
        for c in 0..cells[r].len() {
            let cell = &cells[r][c];
            if cell.wide_continuation {
                continue;
            }
            if highlight[r][c] && !cell.text.trim().is_empty() {
                let mut highlighted = cell.clone();
                if !theme_preserves_terminal_colors(Some(theme)) {
                    highlighted.bold = true;
                    highlighted.fg = DisplayColor::Rgb(theme.default_fg_rgb());
                }
                let text = live_render_text_for_cell(
                    cell,
                    r,
                    c,
                    cells.len(),
                    animation.frame,
                    animation.ratio,
                    animation.effect,
                );
                write_cell(out, &highlighted, &text, &mut style_state)?;
            } else {
                write_cell(out, cell, &cell.text, &mut style_state)?;
            }
        }
        write!(out, "\x1b[0m")?;
    }
    execute!(
        out,
        crossterm::cursor::MoveTo(state.cursor_col, state.cursor_row)
    )?;
    if state.cursor_visible {
        execute!(out, Show)?;
    } else {
        execute!(out, Hide)?;
    }
    out.flush()
}

fn live_render_text_for_cell(
    cell: &StyledCell,
    row: usize,
    col: usize,
    total_rows: usize,
    frame: usize,
    ratio: f32,
    effect: EffectKind,
) -> String {
    if cell.text.trim().is_empty() {
        return cell.text.clone();
    }

    match effect {
        EffectKind::Plain => cell.text.clone(),
        EffectKind::Sweep => {
            let width_gate = (col as f32 + 1.0) / ((col + 8) as f32);
            if ratio >= width_gate.clamp(0.0, 1.0) {
                cell.text.clone()
            } else {
                reveal_noise_symbol(row, col, frame).to_string()
            }
        }
        EffectKind::Fade => {
            if ratio >= 0.6 {
                cell.text.clone()
            } else {
                reveal_noise_symbol(row, col, frame).to_string()
            }
        }
        EffectKind::Coalesce => {
            let appear_at = ((row * 17 + col * 7) % 100) as f32 / 100.0;
            let reveal = ((ratio - appear_at * 0.55) / 0.45).clamp(0.0, 1.0);
            if reveal >= 0.98 {
                cell.text.clone()
            } else {
                reveal_noise_symbol(row, col, frame).to_string()
            }
        }
        EffectKind::Matrix => {
            let state = matrix_cell_state(row, col, total_rows.max(1), ratio);
            if state >= 1.0 {
                cell.text.clone()
            } else {
                reveal_matrix_symbol(row, col, frame).to_string()
            }
        }
    }
}

fn row_has_changes(changed: &[Vec<bool>], row: usize) -> bool {
    changed
        .get(row)
        .is_some_and(|cells| cells.iter().copied().any(|is_changed| is_changed))
}

fn detect_scroll_hint(
    prev: Option<&Vec<Vec<StyledCell>>>,
    current: &[Vec<StyledCell>],
    rows: usize,
    cols: usize,
) -> Option<ScrollHint> {
    let prev = prev?;
    if rows < 2 || cols == 0 {
        return None;
    }

    for offset in 1..rows {
        let scrolled_up = (0..rows.saturating_sub(offset)).all(|r| {
            prev.get(r + offset)
                .zip(current.get(r))
                .is_some_and(|(a, b)| rows_equal(a, b, cols))
        });
        if scrolled_up {
            return Some(ScrollHint::Up(offset as u16));
        }
    }

    for offset in 1..rows {
        let scrolled_down = (offset..rows).all(|r| {
            prev.get(r - offset)
                .zip(current.get(r))
                .is_some_and(|(a, b)| rows_equal(a, b, cols))
        });
        if scrolled_down {
            return Some(ScrollHint::Down(offset as u16));
        }
    }

    None
}

fn rows_equal(a: &[StyledCell], b: &[StyledCell], cols: usize) -> bool {
    a.iter().take(cols).eq(b.iter().take(cols))
}

fn apply_scroll_hint(
    changed: &mut [Vec<bool>],
    scroll_hint: Option<ScrollHint>,
    rows: usize,
    cols: usize,
) {
    let Some(hint) = scroll_hint else {
        return;
    };

    match hint {
        ScrollHint::Up(lines) => {
            let preserved = rows.saturating_sub(lines as usize);
            for row in changed.iter_mut().take(preserved) {
                for cell in row.iter_mut().take(cols) {
                    *cell = false;
                }
            }
        }
        ScrollHint::Down(lines) => {
            let start = (lines as usize).min(rows);
            for row in changed
                .iter_mut()
                .skip(start)
                .take(rows.saturating_sub(start))
            {
                for cell in row.iter_mut().take(cols) {
                    *cell = false;
                }
            }
        }
    }
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
        write!(out, "\x1b[0m")?;
        write_display_color(out, true, next_style.fg)?;
        write_display_color(out, false, next_style.bg)?;
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

fn write_display_color(out: &mut io::Stdout, fg: bool, color: DisplayColor) -> io::Result<()> {
    match color {
        DisplayColor::Default => {
            if fg {
                write!(out, "\x1b[39m")
            } else {
                write!(out, "\x1b[49m")
            }
        }
        DisplayColor::Indexed(idx) => {
            let code = if fg {
                if idx < 8 {
                    30 + idx
                } else if idx < 16 {
                    90 + (idx - 8)
                } else {
                    return write!(out, "\x1b[38;5;{}m", idx);
                }
            } else if idx < 8 {
                40 + idx
            } else if idx < 16 {
                100 + (idx - 8)
            } else {
                return write!(out, "\x1b[48;5;{}m", idx);
            };
            write!(out, "\x1b[{}m", code)
        }
        DisplayColor::Rgb(rgb) => {
            if fg {
                write!(out, "\x1b[38;2;{};{};{}m", rgb.0, rgb.1, rgb.2)
            } else {
                write!(out, "\x1b[48;2;{};{};{}m", rgb.0, rgb.1, rgb.2)
            }
        }
    }
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
        EffectKind::Matrix => text
            .chars()
            .enumerate()
            .map(|(idx, ch)| {
                if ch.is_whitespace() {
                    return ch;
                }
                let state = matrix_cell_state(0, idx, 1, ratio);
                if state >= 1.0 {
                    ch
                } else {
                    ['0', '1', '|', ':', '.', '+', '*', '#'][(idx + (ratio * 100.0) as usize) % 8]
                }
            })
            .collect(),
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
    fn live_render_passthrough_detects_mouse_enable_sequences() {
        let bytes = b"\x1b[?1002hhello\x1b[?1006h";

        let mut found = Vec::new();
        for pattern in LIVE_RENDER_MOUSE_ENABLE_SEQUENCES {
            for _ in bytes
                .windows(pattern.len())
                .filter(|window| *window == *pattern)
            {
                found.push(*pattern);
            }
        }

        assert!(found.contains(&b"\x1b[?1002h".as_slice()));
        assert!(found.contains(&b"\x1b[?1006h".as_slice()));
    }

    #[test]
    fn live_render_passthrough_detects_mouse_disable_sequences() {
        let bytes = b"\x1b[?1002lhello\x1b[?1006l";

        let mut found = Vec::new();
        for pattern in LIVE_RENDER_MOUSE_DISABLE_SEQUENCES {
            for _ in bytes
                .windows(pattern.len())
                .filter(|window| *window == *pattern)
            {
                found.push(*pattern);
            }
        }

        assert!(found.contains(&b"\x1b[?1002l".as_slice()));
        assert!(found.contains(&b"\x1b[?1006l".as_slice()));
    }

    #[test]
    fn live_render_passthrough_detects_input_mode_sequences() {
        let bytes = b"\x1b[?1hhello\x1b=\x1b[?1l\x1b>";

        let mut found_enable = Vec::new();
        for pattern in LIVE_RENDER_INPUT_MODE_ENABLE_SEQUENCES {
            for _ in bytes
                .windows(pattern.len())
                .filter(|window| *window == *pattern)
            {
                found_enable.push(*pattern);
            }
        }

        let mut found_disable = Vec::new();
        for pattern in LIVE_RENDER_INPUT_MODE_DISABLE_SEQUENCES {
            for _ in bytes
                .windows(pattern.len())
                .filter(|window| *window == *pattern)
            {
                found_disable.push(*pattern);
            }
        }

        assert!(found_enable.contains(&b"\x1b[?1h".as_slice()));
        assert!(found_enable.contains(&b"\x1b=".as_slice()));
        assert!(found_disable.contains(&b"\x1b[?1l".as_slice()));
        assert!(found_disable.contains(&b"\x1b>".as_slice()));
    }

    #[test]
    fn live_render_passthrough_detects_bracketed_paste_mode_sequences() {
        let bytes = b"\x1b[?2004hhello\x1b[?2004l";

        let mut found_enable = Vec::new();
        for pattern in LIVE_RENDER_PASTE_MODE_ENABLE_SEQUENCES {
            for _ in bytes
                .windows(pattern.len())
                .filter(|window| *window == *pattern)
            {
                found_enable.push(*pattern);
            }
        }

        let mut found_disable = Vec::new();
        for pattern in LIVE_RENDER_PASTE_MODE_DISABLE_SEQUENCES {
            for _ in bytes
                .windows(pattern.len())
                .filter(|window| *window == *pattern)
            {
                found_disable.push(*pattern);
            }
        }

        assert!(found_enable.contains(&b"\x1b[?2004h".as_slice()));
        assert!(found_disable.contains(&b"\x1b[?2004l".as_slice()));
    }

    #[test]
    fn noise_suppression_input_matches_up_down_and_wheel_or_drag() {
        assert!(contains_noise_suppression_input(b"\x1b[A"));
        assert!(contains_noise_suppression_input(b"\x1b[B"));
        assert!(contains_noise_suppression_input(b"\x1b[<64;10;5M"));
        assert!(contains_noise_suppression_input(b"\x1b[<32;10;5M"));
        assert!(contains_noise_suppression_input(b"\x1b[M`!!"));
        assert!(contains_noise_suppression_input(b"\x1b[M@!!"));
        assert!(!contains_noise_suppression_input(b"\x1b[C"));
        assert!(!contains_noise_suppression_input(b"\x1b[<0;10;5M"));
    }

    #[test]
    fn strip_byte_sequence_removes_all_occurrences() {
        let stripped = strip_byte_sequence(b"xxSTARTmiddlexxSTARTtail", b"START");

        assert_eq!(stripped, b"xxmiddlexxtail".to_vec());
    }

    #[test]
    fn row_has_changes_detects_dirty_rows() {
        let changed = vec![vec![false, true, false], vec![false, false, false]];

        assert!(row_has_changes(&changed, 0));
        assert!(!row_has_changes(&changed, 1));
        assert!(!row_has_changes(&changed, 2));
    }

    #[test]
    fn live_render_effect_ratios_match_effect_style() {
        assert_eq!(live_render_effect_ratios(EffectKind::Plain), &[1.0]);
        assert_eq!(live_render_effect_ratios(EffectKind::Fade), &[0.35, 1.0]);
        assert_eq!(
            live_render_effect_ratios(EffectKind::Sweep),
            &[0.25, 0.65, 1.0]
        );
        assert_eq!(
            live_render_effect_ratios(EffectKind::Coalesce),
            &[0.12, 0.38, 0.72, 1.0]
        );
        assert_eq!(
            live_render_effect_ratios(EffectKind::Matrix),
            &[0.08, 0.24, 0.45, 0.72, 1.0]
        );
    }

    #[test]
    fn live_render_coalesce_uses_noise_before_settling() {
        let cell = cell_with("X", &StyledCell::blank(None));

        let early = live_render_text_for_cell(&cell, 0, 0, 1, 0, 0.12, EffectKind::Coalesce);
        let late = live_render_text_for_cell(&cell, 0, 0, 1, 3, 1.0, EffectKind::Coalesce);

        assert_ne!(early, " ");
        assert_eq!(late, "X");
    }

    #[test]
    fn live_render_matrix_uses_noise_before_settling() {
        let cell = cell_with("X", &StyledCell::blank(None));

        let early = live_render_text_for_cell(&cell, 0, 0, 1, 0, 0.08, EffectKind::Matrix);
        let late = live_render_text_for_cell(&cell, 0, 0, 1, 4, 1.0, EffectKind::Matrix);

        assert_ne!(early, " ");
        assert_eq!(late, "X");
    }

    #[test]
    fn full_screen_changed_marks_every_cell_dirty() {
        let blank = StyledCell::blank(None);
        let cells = vec![
            vec![cell_with("a", &blank), cell_with("b", &blank)],
            vec![cell_with("c", &blank)],
        ];

        let changed = full_screen_changed(&cells);

        assert_eq!(changed, vec![vec![true, true], vec![true]]);
    }

    #[test]
    fn no_screen_changed_marks_every_cell_clean() {
        let blank = StyledCell::blank(None);
        let cells = vec![
            vec![cell_with("a", &blank), cell_with("b", &blank)],
            vec![cell_with("c", &blank)],
        ];

        let changed = no_screen_changed(&cells);

        assert_eq!(changed, vec![vec![false, false], vec![false]]);
    }

    #[test]
    fn default_theme_preserves_terminal_default_and_indexed_colors() {
        let theme = crate::theme::builtin_theme("default");

        assert_eq!(
            display_vt_color(Some(&theme), vt100::Color::Default, true),
            DisplayColor::Default
        );
        assert_eq!(
            display_vt_color(Some(&theme), vt100::Color::Idx(6), true),
            DisplayColor::Indexed(6)
        );
    }

    #[test]
    fn themed_palette_maps_to_rgb_for_live_render() {
        let theme = crate::theme::builtin_theme("matrix-green");

        assert_eq!(
            display_vt_color(Some(&theme), vt100::Color::Default, false),
            DisplayColor::Rgb(theme.default_bg_rgb())
        );
        assert!(matches!(
            display_vt_color(Some(&theme), vt100::Color::Idx(2), true),
            DisplayColor::Rgb(_)
        ));
    }

    #[test]
    fn default_theme_live_render_highlight_keeps_original_cell_color() {
        let theme = crate::theme::builtin_theme("default");
        let mut cell = StyledCell::blank(Some(&theme));
        cell.text = "X".to_string();
        cell.fg = DisplayColor::Indexed(6);

        let mut highlighted = cell.clone();
        if !theme_preserves_terminal_colors(Some(&theme)) {
            highlighted.bold = true;
            highlighted.fg = DisplayColor::Rgb(theme.default_fg_rgb());
        }

        assert_eq!(highlighted.fg, DisplayColor::Indexed(6));
        assert!(!highlighted.bold);
    }

    #[test]
    fn detect_scroll_hint_finds_single_line_up_scroll() {
        let blank = StyledCell::blank(None);
        let prev = vec![
            vec![cell_with("a", &blank), cell_with("a", &blank)],
            vec![cell_with("b", &blank), cell_with("b", &blank)],
            vec![cell_with("c", &blank), cell_with("c", &blank)],
        ];
        let current = vec![
            vec![cell_with("b", &blank), cell_with("b", &blank)],
            vec![cell_with("c", &blank), cell_with("c", &blank)],
            vec![cell_with("d", &blank), cell_with("d", &blank)],
        ];

        assert_eq!(
            detect_scroll_hint(Some(&prev), &current, 3, 2),
            Some(ScrollHint::Up(1))
        );
    }

    #[test]
    fn apply_scroll_hint_keeps_only_new_edge_row_dirty() {
        let mut changed = vec![vec![true; 3], vec![true; 3], vec![true; 3]];

        apply_scroll_hint(&mut changed, Some(ScrollHint::Up(1)), 3, 3);

        assert_eq!(changed[0], vec![false; 3]);
        assert_eq!(changed[1], vec![false; 3]);
        assert_eq!(changed[2], vec![true; 3]);
    }

    #[test]
    fn detect_scroll_hint_finds_multi_line_up_scroll() {
        let blank = StyledCell::blank(None);
        let prev = vec![
            vec![cell_with("a", &blank)],
            vec![cell_with("b", &blank)],
            vec![cell_with("c", &blank)],
            vec![cell_with("d", &blank)],
        ];
        let current = vec![
            vec![cell_with("c", &blank)],
            vec![cell_with("d", &blank)],
            vec![cell_with("x", &blank)],
            vec![cell_with("y", &blank)],
        ];

        assert_eq!(
            detect_scroll_hint(Some(&prev), &current, 4, 1),
            Some(ScrollHint::Up(2))
        );
    }

    #[test]
    fn collect_terminal_state_clamps_cursor_to_screen_bounds() {
        let mut parser = vt100::Parser::new(2, 3, 0);
        parser.process(b"\x1b[99;99H");
        parser.process(b"\x1b[?25l");

        let state = collect_terminal_state(parser.screen(), 2, 3);

        assert_eq!(state.cursor_row, 1);
        assert_eq!(state.cursor_col, 2);
        assert!(!state.cursor_visible);
    }

    fn cell_with(text: &str, blank: &StyledCell) -> StyledCell {
        let mut cell = blank.clone();
        cell.text = text.to_string();
        cell
    }
}
