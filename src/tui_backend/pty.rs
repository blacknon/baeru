use crate::{
    highlight::{dispatch_tui_triggers, evaluate_lines, filter_new_triggers, TriggerState},
    keymap::KeyMapper,
    model::{
        Theme, ALT_SCREEN_ENTER_SEQUENCES, LIVE_RENDER_INPUT_MODE_DISABLE_SEQUENCES,
        LIVE_RENDER_INPUT_MODE_ENABLE_SEQUENCES, LIVE_RENDER_MOUSE_DISABLE_SEQUENCES,
        LIVE_RENDER_MOUSE_ENABLE_SEQUENCES, LIVE_RENDER_PASTE_MODE_DISABLE_SEQUENCES,
        LIVE_RENDER_PASTE_MODE_ENABLE_SEQUENCES, LIVE_RENDER_RESET_SEQUENCES,
    },
    support::exit_with_status,
    theme::SgrRewriter,
};
use anyhow::{anyhow, Result};
use crossterm::{
    cursor::{Hide, Show},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, size, LeaveAlternateScreen},
};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
#[cfg(unix)]
use signal_hook::{
    consts::signal::{SIGINT, SIGQUIT, SIGTERM, SIGWINCH},
    iterator::Signals,
};
use std::{
    collections::HashMap,
    ffi::OsString,
    io::{self, IsTerminal, Read, Write},
    sync::mpsc,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use super::render::{
    collect_screen, collect_terminal_state, flash_highlight_markers, screen_lines, screen_to_svg,
    LiveRenderScene,
};

pub(super) struct PtySession {
    _pty: Arc<Mutex<Box<dyn portable_pty::MasterPty + Send>>>,
    _resize_watcher: ResizeWatcher,
    pub(super) child: Box<dyn portable_pty::Child + Send + Sync>,
    pub(super) reader: Box<dyn Read + Send>,
    pub(super) writer: Box<dyn Write + Send>,
}

impl PtySession {
    pub(super) fn spawn(command: &[OsString], rows: u16, cols: u16) -> Result<Self> {
        if command.is_empty() {
            return Err(anyhow!("command is required"));
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
        if let Ok(cwd) = std::env::current_dir() {
            cmd.cwd(cwd);
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

pub(super) fn continue_passthrough(
    mut session: PtySession,
    theme: Option<Theme>,
    keymap: HashMap<Vec<u8>, Vec<u8>>,
    highlight_rules: Vec<crate::model::HighlightRule>,
    initial_output: Option<Vec<u8>>,
    leave_alt_screen_on_exit: bool,
) -> Result<()> {
    let use_raw = io::stdin().is_terminal() && io::stdout().is_terminal();
    let _guard = TerminalGuard::enter(use_raw)?;
    let _signal_cleanup = SignalCleanupWatcher::spawn(SignalCleanupConfig {
        leave_alt_screen: true,
        reset_live_sequences: false,
    });
    let writer = Arc::new(Mutex::new(session.writer));
    let _input_handle = spawn_input_forwarder(writer.clone(), keymap, None);

    let mut out = io::stdout();
    let mut rewriter = theme.clone().map(SgrRewriter::new);
    let mut parser = size()
        .ok()
        .map(|(cols, rows)| vt100::Parser::new(rows, cols, 0));
    let mut trigger_state = TriggerState::default();
    if let Some(initial) = initial_output.as_deref() {
        if let Some(parser) = parser.as_mut() {
            parser.process(initial);
            maybe_flash_tui_highlights(
                &mut out,
                parser.screen(),
                theme.as_ref(),
                &highlight_rules,
                &mut trigger_state,
            )?;
        }
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
                if let Some(parser) = parser.as_mut() {
                    parser.process(&buf[..n]);
                    maybe_flash_tui_highlights(
                        &mut out,
                        parser.screen(),
                        theme.as_ref(),
                        &highlight_rules,
                        &mut trigger_state,
                    )?;
                }
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

fn maybe_flash_tui_highlights(
    out: &mut io::Stdout,
    screen: &vt100::Screen,
    theme: Option<&Theme>,
    highlight_rules: &[crate::model::HighlightRule],
    trigger_state: &mut TriggerState,
) -> Result<()> {
    if highlight_rules.is_empty() {
        return Ok(());
    }

    let rows = screen.size().0;
    let cols = screen.size().1;
    let cells = collect_screen(screen, rows, cols, theme);
    let lines = screen_lines(&cells);
    let evaluation = evaluate_lines(&lines, highlight_rules);
    let new_triggers = filter_new_triggers(trigger_state, evaluation.triggers);
    dispatch_tui_triggers(
        &new_triggers,
        Some(&screen_to_svg(&cells, Some(&evaluation.colors))),
    )?;
    if !new_triggers.is_empty() {
        flash_highlight_markers(
            out,
            LiveRenderScene {
                cells: &cells,
                state: &collect_terminal_state(screen, rows, cols),
                highlight_colors: Some(&evaluation.colors),
                theme: theme.unwrap_or(&Theme::default()),
            },
        )?;
    }
    Ok(())
}

pub(super) fn contains_alt_screen_enter_sequence(bytes: &[u8]) -> bool {
    ALT_SCREEN_ENTER_SEQUENCES.iter().any(|pattern| {
        bytes
            .windows(pattern.len())
            .any(|window| window == *pattern)
    })
}

#[derive(Default)]
pub(super) struct LivePassthrough {
    pending: Vec<u8>,
    pub(super) mouse_reporting_active: bool,
}

impl LivePassthrough {
    pub(super) fn feed<W: Write>(&mut self, out: &mut W, bytes: &[u8]) -> io::Result<()> {
        self.pending.extend_from_slice(bytes);
        let keep = longest_live_passthrough_suffix(&self.pending);
        let split_at = self.pending.len().saturating_sub(keep);
        let scan = self.pending[..split_at].to_vec();
        self.pending = self.pending[split_at..].to_vec();

        let mut wrote_any = false;
        let mut i = 0;
        while i < scan.len() {
            if let Some((pattern, mouse_state)) = live_passthrough_match(&scan[i..]) {
                out.write_all(pattern)?;
                wrote_any = true;
                if let Some(state) = mouse_state {
                    self.mouse_reporting_active = state;
                }
                i += pattern.len();
            } else {
                i += 1;
            }
        }

        if wrote_any {
            out.flush()?;
        }
        Ok(())
    }
}

pub(super) fn live_passthrough_match(bytes: &[u8]) -> Option<(&'static [u8], Option<bool>)> {
    for pattern in LIVE_RENDER_MOUSE_ENABLE_SEQUENCES {
        if bytes.starts_with(pattern) {
            return Some((pattern, Some(true)));
        }
    }
    for pattern in LIVE_RENDER_MOUSE_DISABLE_SEQUENCES {
        if bytes.starts_with(pattern) {
            return Some((pattern, Some(false)));
        }
    }
    for pattern in LIVE_RENDER_INPUT_MODE_ENABLE_SEQUENCES {
        if bytes.starts_with(pattern) {
            return Some((pattern, None));
        }
    }
    for pattern in LIVE_RENDER_INPUT_MODE_DISABLE_SEQUENCES {
        if bytes.starts_with(pattern) {
            return Some((pattern, None));
        }
    }
    for pattern in LIVE_RENDER_PASTE_MODE_ENABLE_SEQUENCES {
        if bytes.starts_with(pattern) {
            return Some((pattern, None));
        }
    }
    for pattern in LIVE_RENDER_PASTE_MODE_DISABLE_SEQUENCES {
        if bytes.starts_with(pattern) {
            return Some((pattern, None));
        }
    }
    None
}

pub(super) fn longest_live_passthrough_suffix(bytes: &[u8]) -> usize {
    let patterns = LIVE_RENDER_MOUSE_ENABLE_SEQUENCES
        .iter()
        .chain(LIVE_RENDER_MOUSE_DISABLE_SEQUENCES.iter())
        .chain(LIVE_RENDER_INPUT_MODE_ENABLE_SEQUENCES.iter())
        .chain(LIVE_RENDER_INPUT_MODE_DISABLE_SEQUENCES.iter())
        .chain(LIVE_RENDER_PASTE_MODE_ENABLE_SEQUENCES.iter())
        .chain(LIVE_RENDER_PASTE_MODE_DISABLE_SEQUENCES.iter());

    let max_len = patterns.clone().map(|p| p.len()).max().unwrap_or(0);
    let max_suffix = bytes.len().min(max_len.saturating_sub(1));
    for len in (1..=max_suffix).rev() {
        let suffix = &bytes[bytes.len() - len..];
        if patterns.clone().any(|pattern| pattern.starts_with(suffix)) {
            return len;
        }
    }
    0
}

pub(super) fn reset_live_passthrough_sequences(out: &mut io::Stdout) -> io::Result<()> {
    out.write_all(LIVE_RENDER_RESET_SEQUENCES)?;
    out.flush()
}

pub(super) fn strip_alt_screen_enter_sequences(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    for pattern in ALT_SCREEN_ENTER_SEQUENCES {
        out = strip_byte_sequence(&out, pattern);
    }
    out
}

pub(super) fn strip_byte_sequence(bytes: &[u8], needle: &[u8]) -> Vec<u8> {
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

pub(super) struct TerminalGuard {
    raw_mode_enabled: bool,
    cursor_hidden: bool,
}

const KEYMAP_PENDING_TIMEOUT: Duration = Duration::from_millis(35);

pub(super) fn spawn_input_forwarder(
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

pub(super) fn contains_noise_suppression_input(bytes: &[u8]) -> bool {
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

pub(super) fn mouse_activity_recent(
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

#[derive(Clone, Copy)]
#[cfg_attr(not(unix), allow(dead_code))]
pub(super) struct SignalCleanupConfig {
    pub(super) leave_alt_screen: bool,
    pub(super) reset_live_sequences: bool,
}

pub(super) struct SignalCleanupWatcher {
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

impl SignalCleanupWatcher {
    pub(super) fn spawn(config: SignalCleanupConfig) -> Self {
        #[cfg(unix)]
        {
            let mut signals =
                Signals::new([SIGINT, SIGTERM, SIGQUIT]).expect("failed to register exit signals");
            let handle = signals.handle();
            let join = thread::spawn(move || {
                if signals.forever().next().is_some() {
                    let mut stdout = io::stdout();
                    if config.reset_live_sequences {
                        let _ = reset_live_passthrough_sequences(&mut stdout);
                    }
                    if config.leave_alt_screen {
                        let _ = execute!(stdout, LeaveAlternateScreen);
                    }
                    crate::support::restore_terminal_state();
                    std::process::exit(130);
                }
            });
            Self {
                handle,
                join: Some(join),
            }
        }

        #[cfg(not(unix))]
        {
            let _ = config;
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

impl Drop for SignalCleanupWatcher {
    fn drop(&mut self) {
        #[cfg(unix)]
        self.handle.close();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl TerminalGuard {
    pub(super) fn enter(raw_mode: bool) -> Result<Self> {
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
