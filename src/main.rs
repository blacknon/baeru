use anyhow::{anyhow, Context, Result};
use clap::{Parser, ValueEnum};
use crossterm::{
    cursor::{Hide, Show},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, size, Clear, ClearType},
};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::Deserialize;
use std::{
    collections::{BTreeSet, HashMap},
    ffi::OsString,
    fs,
    io::{self, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Backend {
    Auto,
    Tui,
    Cli,
    Raw,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Feature {
    #[serde(alias = "reveal")]
    Reveal,
    #[serde(alias = "live_color")]
    LiveColor,
    #[serde(alias = "keymap")]
    Keymap,
    #[serde(alias = "inline_animation")]
    InlineAnimation,
    #[serde(alias = "splash")]
    Splash,
    #[serde(alias = "live_render")]
    LiveRender,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Mode {
    Reveal,
    ColorLive,
    Splash,
    /// Experimental: rebuild the live screen from VT100 state and flash changed cells.
    LiveRender,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum EffectKind {
    Coalesce,
    Sweep,
    Fade,
    Plain,
}

#[derive(Debug, Parser)]
#[command(name = "baeru", version, about = "Make existing terminal apps glow up")]
struct Cli {
    #[arg(long, value_enum)]
    backend: Option<Backend>,

    #[arg(long, value_enum)]
    mode: Option<Mode>,

    #[arg(long, value_enum)]
    effect: Option<EffectKind>,

    #[arg(long)]
    config_file: Option<PathBuf>,

    #[arg(long)]
    theme_file: Option<PathBuf>,

    #[arg(long, default_value = "jirai-pink")]
    palette: String,

    #[arg(long)]
    keymap_file: Option<PathBuf>,

    #[arg(long, default_value_t = 360)]
    capture_ms: u64,

    #[arg(long, default_value_t = 720)]
    duration_ms: u64,

    #[arg(long, default_value_t = 24)]
    frames: usize,

    #[arg(long, default_value_t = 200)]
    max_lines: usize,

    #[arg(long, default_value_t = 1_000_000)]
    max_bytes: usize,

    #[arg(long, default_value_t = false)]
    animate_over_limit: bool,

    #[arg(long, default_value_t = false)]
    no_theme_after_reveal: bool,

    #[arg(last = true, allow_hyphen_values = true)]
    command: Vec<OsString>,
}

#[derive(Debug, Deserialize, Default)]
struct ConfigFile {
    #[serde(default)]
    profiles: Vec<Profile>,
}

#[derive(Debug, Deserialize, Default, Clone)]
struct Profile {
    #[serde(rename = "name")]
    _name: Option<String>,
    #[serde(default)]
    r#match: MatchSpec,
    backend: Option<Backend>,
    #[serde(default)]
    features: Vec<Feature>,
    mode: Option<Mode>,
    effect: Option<EffectKind>,
    theme_file: Option<PathBuf>,
    palette: Option<String>,
    keymap_file: Option<PathBuf>,
    #[serde(default)]
    keymap: HashMap<String, String>,
    capture_ms: Option<u64>,
    duration_ms: Option<u64>,
    frames: Option<usize>,
    max_lines: Option<usize>,
    max_bytes: Option<usize>,
    animate_over_limit: Option<bool>,
}

#[derive(Debug, Deserialize, Default, Clone)]
struct MatchSpec {
    command: Option<String>,
    path: Option<PathBuf>,
    #[serde(default)]
    args_prefix: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
struct KeymapFile {
    #[serde(default)]
    keymap: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Theme {
    #[serde(rename = "name")]
    _name: Option<String>,
    default_fg: Option<String>,
    default_bg: Option<String>,
    #[serde(default)]
    force_default: bool,
    #[serde(default)]
    foreground: Vec<ColorStop>,
    #[serde(default)]
    background: Vec<ColorStop>,
}

#[derive(Debug, Clone, Deserialize)]
struct ColorStop {
    at: f32,
    color: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rgb(u8, u8, u8);

#[derive(Debug, Clone)]
struct Runtime {
    backend: Backend,
    features: BTreeSet<Feature>,
    effect: EffectKind,
    command: Vec<OsString>,
    theme: Theme,
    keymap: HashMap<Vec<u8>, Vec<u8>>,
    capture_ms: u64,
    duration_ms: u64,
    frames: usize,
    max_lines: usize,
    max_bytes: usize,
    animate_over_limit: bool,
    no_theme_after_reveal: bool,
}

const CLI_SCRAMBLE: &[char] = &[
    '░', '▒', '▓', '█', '◆', '◇', '○', '●', '◌', '◍', '·', '*', '+', '#', '%', '@', '/', '\\', '|',
    '-',
];

fn main() -> Result<()> {
    let cli = Cli::parse();
    let rt = build_runtime(cli)?;
    match rt.backend {
        Backend::Tui => run_tui_backend(rt),
        Backend::Cli => run_cli_backend(rt),
        Backend::Raw => run_raw_backend(rt),
        Backend::Auto => Err(anyhow!("unresolved backend")),
    }
}

fn restore_terminal_state() {
    if io::stdout().is_terminal() {
        let _ = execute!(io::stdout(), Show, crossterm::style::ResetColor);
        let _ = disable_raw_mode();
    }
}

fn exit_with_status(code: i32) -> ! {
    restore_terminal_state();
    std::process::exit(code);
}

fn build_runtime(cli: Cli) -> Result<Runtime> {
    let stdin_is_tty = io::stdin().is_terminal();
    let stdout_is_tty = io::stdout().is_terminal();
    let command = if cli.command.is_empty() && stdin_is_tty {
        vec![OsString::from("htop")]
    } else {
        cli.command.clone()
    };

    let config = match resolve_config_path(cli.config_file.as_deref()) {
        Some(path) => read_config(&path)
            .with_context(|| format!("failed to read config: {}", path.display()))?,
        None => ConfigFile::default(),
    };
    let profile = find_profile(&config, &command).cloned().unwrap_or_default();

    let requested_backend = cli.backend.or(profile.backend).unwrap_or(Backend::Auto);
    let backend = resolve_backend(requested_backend, &command, stdin_is_tty, stdout_is_tty);

    let mut features = resolve_features(&profile, cli.mode, backend);
    if !stdout_is_tty || is_term_dumb() {
        features.remove(&Feature::Reveal);
        features.remove(&Feature::InlineAnimation);
        features.remove(&Feature::LiveColor);
        features.remove(&Feature::Splash);
        features.remove(&Feature::LiveRender);
    }
    if env_flag("NO_COLOR") {
        features.remove(&Feature::LiveColor);
    }

    let effect = cli
        .effect
        .or(profile.effect)
        .unwrap_or_else(|| default_effect_for_backend(backend));

    let capture_ms = profile.capture_ms.unwrap_or(cli.capture_ms);
    let duration_ms = profile.duration_ms.unwrap_or(cli.duration_ms);
    let frames = profile.frames.unwrap_or(cli.frames).max(1);

    let palette = profile.palette.unwrap_or(cli.palette);
    let needs_theme = backend == Backend::Tui
        && (features.contains(&Feature::Reveal)
            || features.contains(&Feature::LiveColor)
            || features.contains(&Feature::LiveRender));
    let theme = if needs_theme {
        let theme_path = cli.theme_file.or(profile.theme_file);
        if let Some(path) = theme_path {
            read_theme(&path)
                .with_context(|| format!("failed to read theme: {}", path.display()))?
        } else {
            builtin_theme(&palette)
        }
    } else {
        builtin_theme("default")
    };

    let keymap = if features.contains(&Feature::Keymap) {
        let mut keymap_text = HashMap::new();
        if let Some(path) = profile.keymap_file.or(cli.keymap_file) {
            let km = read_keymap(&path)
                .with_context(|| format!("failed to read keymap: {}", path.display()))?;
            keymap_text.extend(km.keymap);
        }
        keymap_text.extend(profile.keymap);
        compile_keymap(&keymap_text)?
    } else {
        HashMap::new()
    };

    let max_lines = profile.max_lines.unwrap_or(cli.max_lines);
    let max_bytes = profile.max_bytes.unwrap_or(cli.max_bytes);
    let animate_over_limit = profile.animate_over_limit.unwrap_or(cli.animate_over_limit);

    Ok(Runtime {
        backend,
        features,
        effect,
        command,
        theme,
        keymap,
        capture_ms,
        duration_ms,
        frames,
        max_lines,
        max_bytes,
        animate_over_limit,
        no_theme_after_reveal: cli.no_theme_after_reveal,
    })
}

fn resolve_config_path(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return Some(path.to_path_buf());
    }

    let local = PathBuf::from("baeru.yml");
    if local.exists() {
        return Some(local);
    }

    None
}

fn resolve_backend(
    requested: Backend,
    command: &[OsString],
    stdin_is_tty: bool,
    stdout_is_tty: bool,
) -> Backend {
    match requested {
        Backend::Tui | Backend::Cli | Backend::Raw => requested,
        Backend::Auto => {
            if !stdout_is_tty || is_term_dumb() {
                Backend::Raw
            } else if command.is_empty() && !stdin_is_tty {
                Backend::Cli
            } else {
                Backend::Tui
            }
        }
    }
}

fn resolve_features(
    profile: &Profile,
    cli_mode: Option<Mode>,
    backend: Backend,
) -> BTreeSet<Feature> {
    let mut features = BTreeSet::new();
    if !profile.features.is_empty() {
        features.extend(profile.features.iter().copied());
    } else if let Some(mode) = profile.mode {
        features.extend(mode_to_features(mode));
    }
    if let Some(mode) = cli_mode {
        features.extend(mode_to_features(mode));
    }
    if features.is_empty() {
        match backend {
            Backend::Tui => {
                features.insert(Feature::Reveal);
                features.insert(Feature::LiveColor);
            }
            Backend::Cli => {
                features.insert(Feature::InlineAnimation);
            }
            Backend::Raw | Backend::Auto => {}
        }
    }
    features
}

fn mode_to_features(mode: Mode) -> Vec<Feature> {
    match mode {
        Mode::Reveal => vec![Feature::Reveal, Feature::LiveColor],
        Mode::ColorLive => vec![Feature::LiveColor],
        Mode::Splash => vec![Feature::Splash],
        Mode::LiveRender => vec![Feature::LiveRender],
    }
}

fn default_effect_for_backend(backend: Backend) -> EffectKind {
    match backend {
        Backend::Cli | Backend::Tui => EffectKind::Coalesce,
        Backend::Raw | Backend::Auto => EffectKind::Plain,
    }
}

fn read_config(path: &Path) -> Result<ConfigFile> {
    let s = fs::read_to_string(path)?;
    Ok(serde_yaml::from_str(&s)?)
}

fn read_theme(path: &Path) -> Result<Theme> {
    let s = fs::read_to_string(path)?;
    Ok(serde_yaml::from_str(&s)?)
}

fn read_keymap(path: &Path) -> Result<KeymapFile> {
    let s = fs::read_to_string(path)?;
    Ok(serde_yaml::from_str(&s)?)
}

fn find_profile<'a>(config: &'a ConfigFile, command: &[OsString]) -> Option<&'a Profile> {
    let cmd_path = command.first()?;
    let cmd_str = cmd_path.to_string_lossy();
    let basename = Path::new(cmd_str.as_ref())
        .file_name()
        .map(|x| x.to_string_lossy().to_string())
        .unwrap_or_else(|| cmd_str.to_string());
    let args: Vec<String> = command
        .iter()
        .skip(1)
        .map(|arg| arg.to_string_lossy().to_string())
        .collect();

    config.profiles.iter().find(|p| {
        let path_match = p
            .r#match
            .path
            .as_ref()
            .is_some_and(|path| path.as_os_str() == cmd_path);
        let command_match = p
            .r#match
            .command
            .as_ref()
            .is_some_and(|cmd| cmd == &basename || cmd == cmd_str.as_ref());
        let args_match = p.r#match.args_prefix.is_empty()
            || args
                .iter()
                .map(String::as_str)
                .take(p.r#match.args_prefix.len())
                .eq(p.r#match.args_prefix.iter().map(String::as_str));
        (path_match || command_match) && args_match
    })
}

fn builtin_theme(name: &str) -> Theme {
    match name {
        "matrix" | "matrix-green" => Theme {
            _name: Some("matrix-green".to_string()),
            default_fg: Some("#b6ffd0".to_string()),
            default_bg: Some("#001008".to_string()),
            force_default: true,
            foreground: vec![
                stop(0.00, "#004d26"),
                stop(0.35, "#00aa55"),
                stop(0.70, "#33ff99"),
                stop(1.00, "#eafff2"),
            ],
            background: vec![
                stop(0.00, "#001008"),
                stop(0.50, "#003018"),
                stop(1.00, "#006633"),
            ],
        },
        _ => Theme {
            _name: Some("jirai-pink".to_string()),
            default_fg: Some("#ffcdeb".to_string()),
            default_bg: Some("#120018".to_string()),
            force_default: true,
            foreground: vec![
                stop(0.00, "#84205c"),
                stop(0.30, "#ff45ac"),
                stop(0.62, "#ff8fd6"),
                stop(0.84, "#ffcdeb"),
                stop(1.00, "#fff2fa"),
            ],
            background: vec![
                stop(0.00, "#120018"),
                stop(0.28, "#26002a"),
                stop(0.55, "#570a41"),
                stop(0.78, "#962369"),
                stop(1.00, "#ff8fcf"),
            ],
        },
    }
}

fn stop(at: f32, color: &str) -> ColorStop {
    ColorStop {
        at,
        color: color.to_string(),
    }
}

fn run_tui_backend(rt: Runtime) -> Result<()> {
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

fn run_cli_backend(rt: Runtime) -> Result<()> {
    let stdout_is_tty = io::stdout().is_terminal();
    let (text, exit_code) = if rt.command.is_empty() {
        read_stdin_text()?
    } else {
        capture_command_text(&rt.command)?
    };

    if !stdout_is_tty
        || !rt.features.contains(&Feature::InlineAnimation)
        || rt.effect == EffectKind::Plain
        || env_flag("NO_COLOR")
        || text.is_empty()
    {
        print_raw_text(&text)?;
        exit_with_status(exit_code);
    }

    if text.len() > rt.max_bytes && !rt.animate_over_limit {
        print_raw_text(&text)?;
        exit_with_status(exit_code);
    }

    if !animate_cli_output(&text, &rt)? {
        print_raw_text(&text)?;
    }
    exit_with_status(exit_code);
}

fn run_raw_backend(rt: Runtime) -> Result<()> {
    if rt.command.is_empty() {
        let mut stdin = io::stdin().lock();
        let mut stdout = io::stdout().lock();
        io::copy(&mut stdin, &mut stdout)?;
        stdout.flush()?;
        return Ok(());
    }
    spawn_direct(&rt.command)
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
    let writer_for_input = writer.clone();
    let keymap = rt.keymap.clone();
    let _input_handle = thread::spawn(move || -> io::Result<()> {
        let mut stdin = io::stdin();
        let mut buf = [0u8; 1024];
        let mapper = KeyMapper::new(keymap);
        loop {
            let n = stdin.read(&mut buf)?;
            if n == 0 {
                break;
            }
            let out = mapper.map_bytes(&buf[..n]);
            let mut w = writer_for_input.lock().unwrap();
            w.write_all(&out)?;
            w.flush()?;
        }
        Ok(())
    });

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
                let current = collect_screen(parser.screen(), rows, cols, &rt.theme);
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

    let screen = collect_screen(parser.screen(), rows, cols, &rt.theme);
    animate_styled_reveal(&screen, rows, cols, rt.frames, rt.duration_ms, rt.effect)?;

    let theme = if rt.no_theme_after_reveal || !rt.features.contains(&Feature::LiveColor) {
        None
    } else {
        Some(rt.theme.clone())
    };
    let keymap = if rt.features.contains(&Feature::Keymap) {
        rt.keymap.clone()
    } else {
        HashMap::new()
    };
    continue_passthrough(session, theme, keymap, Some(captured))
}

fn run_pty_passthrough(
    rt: Runtime,
    theme: Option<Theme>,
    keymap: HashMap<Vec<u8>, Vec<u8>>,
) -> Result<()> {
    let (cols, rows) = size().unwrap_or((80, 24));
    let session = PtySession::spawn(&rt.command, rows, cols)?;
    continue_passthrough(session, theme, keymap, None)
}

fn spawn_direct(command: &[OsString]) -> Result<()> {
    let mut cmd = std::process::Command::new(&command[0]);
    cmd.args(&command[1..]);
    let status = cmd.status()?;
    exit_with_status(status.code().unwrap_or(1));
}

struct PtySession {
    _pty: Box<dyn portable_pty::MasterPty>,
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
        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        Ok(Self {
            _pty: pair.master,
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
) -> Result<()> {
    let use_raw = io::stdin().is_terminal() && io::stdout().is_terminal();
    let _guard = TerminalGuard::enter(use_raw)?;
    let writer = Arc::new(Mutex::new(session.writer));
    let writer_for_input = writer.clone();

    let _input_handle = thread::spawn(move || -> io::Result<()> {
        let mut stdin = io::stdin();
        let mut buf = [0u8; 1024];
        let mapper = KeyMapper::new(keymap);
        loop {
            let n = stdin.read(&mut buf)?;
            if n == 0 {
                break;
            }
            let out = mapper.map_bytes(&buf[..n]);
            let mut w = writer_for_input.lock().unwrap();
            w.write_all(&out)?;
            w.flush()?;
        }
        Ok(())
    });

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
    exit_with_status(status.exit_code() as i32);
}

struct TerminalGuard {
    raw_mode_enabled: bool,
    cursor_hidden: bool,
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
    theme: &Theme,
) -> Vec<Vec<StyledCell>> {
    let mut result = Vec::new();
    for row in 0..rows {
        let mut out_row = Vec::new();
        for col in 0..cols {
            if let Some(cell) = screen.cell(row, col) {
                let fg = theme.map_vt_color(cell.fgcolor(), true);
                let bg = theme.map_vt_color(cell.bgcolor(), false);
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
    fn blank(theme: &Theme) -> Self {
        Self {
            text: " ".to_string(),
            fg: theme.default_fg_rgb(),
            bg: theme.default_bg_rgb(),
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            inverse: false,
            wide_continuation: false,
        }
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
            write!(out, "\x1b[0m\r\n")?;
        }
        out.flush()?;
        sleep_frame(duration_ms, frames);
    }
    execute!(out, crossterm::cursor::MoveTo(0, 0))?;
    for row in cells {
        let mut style_state = None;
        for cell in row {
            if !cell.wide_continuation {
                write_cell(&mut out, cell, &cell.text, &mut style_state)?;
            }
        }
        write!(out, "\x1b[0m\r\n")?;
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
    const SYMBOLS: [&str; 8] = ["░", "▒", "▓", "◆", "◇", "✦", "✧", "♡"];
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
        write!(out, "\x1b[0m\r\n")?;
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

fn sleep_frame(duration_ms: u64, frames: usize) {
    thread::sleep(Duration::from_millis(
        (duration_ms / frames.max(1) as u64).max(1),
    ));
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
            let symbols = ['░', '▒', '▓', '◆', '◇', '✦', '✧'];
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

impl Theme {
    fn default_fg_rgb(&self) -> Rgb {
        parse_hex(self.default_fg.as_deref().unwrap_or("#ffffff")).unwrap_or(Rgb(255, 255, 255))
    }

    fn default_bg_rgb(&self) -> Rgb {
        parse_hex(self.default_bg.as_deref().unwrap_or("#000000")).unwrap_or(Rgb(0, 0, 0))
    }

    fn map_vt_color(&self, color: vt100::Color, foreground: bool) -> Rgb {
        match color {
            vt100::Color::Default => {
                if foreground {
                    self.default_fg_rgb()
                } else {
                    self.default_bg_rgb()
                }
            }
            vt100::Color::Idx(idx) => self.map_rgb(indexed_color(idx), foreground),
            vt100::Color::Rgb(r, g, b) => self.map_rgb(Rgb(r, g, b), foreground),
        }
    }

    fn map_rgb(&self, rgb: Rgb, foreground: bool) -> Rgb {
        let intensity = ((rgb.0 as f32 * 0.299 + rgb.1 as f32 * 0.587 + rgb.2 as f32 * 0.114)
            / 255.0)
            .clamp(0.0, 1.0);
        let stops = if foreground {
            &self.foreground
        } else {
            &self.background
        };
        gradient(stops, intensity).unwrap_or(rgb)
    }
}

fn gradient(stops: &[ColorStop], t: f32) -> Option<Rgb> {
    if stops.is_empty() {
        return None;
    }
    let mut parsed: Vec<(f32, Rgb)> = stops
        .iter()
        .filter_map(|s| parse_hex(&s.color).map(|c| (s.at, c)))
        .collect();
    if parsed.is_empty() {
        return None;
    }
    parsed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    if t <= parsed[0].0 {
        return Some(parsed[0].1);
    }
    for pair in parsed.windows(2) {
        let (a_t, a) = pair[0];
        let (b_t, b) = pair[1];
        if t <= b_t {
            let k = ((t - a_t) / (b_t - a_t).max(0.0001)).clamp(0.0, 1.0);
            return Some(Rgb(lerp(a.0, b.0, k), lerp(a.1, b.1, k), lerp(a.2, b.2, k)));
        }
    }
    Some(parsed.last().unwrap().1)
}

fn lerp(a: u8, b: u8, k: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * k)
        .round()
        .clamp(0.0, 255.0) as u8
}

fn parse_hex(s: &str) -> Option<Rgb> {
    let hex = s.strip_prefix('#').unwrap_or(s);
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Rgb(r, g, b))
}

fn indexed_color(idx: u8) -> Rgb {
    const BASIC: [Rgb; 16] = [
        Rgb(0, 0, 0),
        Rgb(205, 49, 49),
        Rgb(13, 188, 121),
        Rgb(229, 229, 16),
        Rgb(36, 114, 200),
        Rgb(188, 63, 188),
        Rgb(17, 168, 205),
        Rgb(229, 229, 229),
        Rgb(102, 102, 102),
        Rgb(241, 76, 76),
        Rgb(35, 209, 139),
        Rgb(245, 245, 67),
        Rgb(59, 142, 234),
        Rgb(214, 112, 214),
        Rgb(41, 184, 219),
        Rgb(255, 255, 255),
    ];
    if idx < 16 {
        return BASIC[idx as usize];
    }
    if idx >= 232 {
        let value = 8 + (idx - 232) * 10;
        return Rgb(value, value, value);
    }
    let n = idx - 16;
    let r = n / 36;
    let g = (n % 36) / 6;
    let b = n % 6;
    let convert = |x: u8| if x == 0 { 0 } else { 55 + x * 40 };
    Rgb(convert(r), convert(g), convert(b))
}

struct SgrRewriter {
    theme: Theme,
    state: EscState,
    buf: Vec<u8>,
}

#[derive(Clone, Copy)]
enum EscState {
    Ground,
    Esc,
    Csi,
}

impl SgrRewriter {
    fn new(theme: Theme) -> Self {
        Self {
            theme,
            state: EscState::Ground,
            buf: Vec::new(),
        }
    }

    fn feed(&mut self, input: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(input.len() + 64);
        for &byte in input {
            match self.state {
                EscState::Ground => {
                    if byte == 0x1b {
                        self.buf.clear();
                        self.buf.push(byte);
                        self.state = EscState::Esc;
                    } else {
                        out.push(byte);
                    }
                }
                EscState::Esc => {
                    self.buf.push(byte);
                    if byte == b'[' {
                        self.state = EscState::Csi;
                    } else {
                        out.extend_from_slice(&self.buf);
                        self.state = EscState::Ground;
                    }
                }
                EscState::Csi => {
                    self.buf.push(byte);
                    if (0x40..=0x7e).contains(&byte) {
                        if byte == b'm' {
                            out.extend(self.rewrite_sgr());
                        } else {
                            out.extend_from_slice(&self.buf);
                        }
                        self.state = EscState::Ground;
                        self.buf.clear();
                    }
                }
            }
        }
        out
    }

    fn rewrite_sgr(&self) -> Vec<u8> {
        let body = &self.buf[2..self.buf.len() - 1];
        let text = String::from_utf8_lossy(body);
        let params: Vec<i32> = if text.is_empty() {
            vec![0]
        } else {
            text.split(';')
                .map(|p| {
                    if p.is_empty() {
                        0
                    } else {
                        p.parse().unwrap_or(-1)
                    }
                })
                .collect()
        };
        let mut out = Vec::new();
        let mut i = 0;
        while i < params.len() {
            let p = params[i];
            match p {
                0 => {
                    out.extend_from_slice(b"\x1b[0m");
                    if self.theme.force_default {
                        let fg = self.theme.default_fg_rgb();
                        let bg = self.theme.default_bg_rgb();
                        out.extend_from_slice(
                            format!(
                                "\x1b[38;2;{};{};{}m\x1b[48;2;{};{};{}m",
                                fg.0, fg.1, fg.2, bg.0, bg.1, bg.2
                            )
                            .as_bytes(),
                        );
                    }
                }
                30..=37 | 90..=97 => {
                    let idx = if p >= 90 {
                        (p - 90 + 8) as u8
                    } else {
                        (p - 30) as u8
                    };
                    let rgb = self.theme.map_rgb(indexed_color(idx), true);
                    out.extend_from_slice(
                        format!("\x1b[38;2;{};{};{}m", rgb.0, rgb.1, rgb.2).as_bytes(),
                    );
                }
                40..=47 | 100..=107 => {
                    let idx = if p >= 100 {
                        (p - 100 + 8) as u8
                    } else {
                        (p - 40) as u8
                    };
                    let rgb = self.theme.map_rgb(indexed_color(idx), false);
                    out.extend_from_slice(
                        format!("\x1b[48;2;{};{};{}m", rgb.0, rgb.1, rgb.2).as_bytes(),
                    );
                }
                39 => {
                    let fg = self.theme.default_fg_rgb();
                    out.extend_from_slice(
                        format!("\x1b[38;2;{};{};{}m", fg.0, fg.1, fg.2).as_bytes(),
                    );
                }
                49 => {
                    let bg = self.theme.default_bg_rgb();
                    out.extend_from_slice(
                        format!("\x1b[48;2;{};{};{}m", bg.0, bg.1, bg.2).as_bytes(),
                    );
                }
                38 | 48 => {
                    let is_fg = p == 38;
                    if i + 2 < params.len() && params[i + 1] == 5 {
                        let idx = params[i + 2].clamp(0, 255) as u8;
                        let rgb = self.theme.map_rgb(indexed_color(idx), is_fg);
                        out.extend_from_slice(sgr_rgb(is_fg, rgb).as_bytes());
                        i += 2;
                    } else if i + 4 < params.len() && params[i + 1] == 2 {
                        let original = Rgb(
                            params[i + 2].clamp(0, 255) as u8,
                            params[i + 3].clamp(0, 255) as u8,
                            params[i + 4].clamp(0, 255) as u8,
                        );
                        let rgb = self.theme.map_rgb(original, is_fg);
                        out.extend_from_slice(sgr_rgb(is_fg, rgb).as_bytes());
                        i += 4;
                    } else {
                        out.extend_from_slice(format!("\x1b[{}m", p).as_bytes());
                    }
                }
                _ => out.extend_from_slice(format!("\x1b[{}m", p).as_bytes()),
            }
            i += 1;
        }
        out
    }
}

fn sgr_rgb(fg: bool, rgb: Rgb) -> String {
    if fg {
        format!("\x1b[38;2;{};{};{}m", rgb.0, rgb.1, rgb.2)
    } else {
        format!("\x1b[48;2;{};{};{}m", rgb.0, rgb.1, rgb.2)
    }
}

struct KeyMapper {
    rules: Vec<(Vec<u8>, Vec<u8>)>,
}

impl KeyMapper {
    fn new(map: HashMap<Vec<u8>, Vec<u8>>) -> Self {
        let mut rules: Vec<_> = map.into_iter().collect();
        rules.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));
        Self { rules }
    }

    fn map_bytes(&self, input: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < input.len() {
            if let Some((from, to)) = self
                .rules
                .iter()
                .find(|(from, _)| input[i..].starts_with(from))
            {
                out.extend_from_slice(to);
                i += from.len();
            } else {
                out.push(input[i]);
                i += 1;
            }
        }
        out
    }
}

fn compile_keymap(map: &HashMap<String, String>) -> Result<HashMap<Vec<u8>, Vec<u8>>> {
    let mut out = HashMap::new();
    for (k, v) in map {
        out.insert(key_to_bytes(k)?, key_to_bytes(v)?);
    }
    Ok(out)
}

fn key_to_bytes(s: &str) -> Result<Vec<u8>> {
    let normalized = s.trim().to_lowercase().replace('_', "-");
    let bytes = match normalized.as_str() {
        "up" => b"\x1b[A".to_vec(),
        "down" => b"\x1b[B".to_vec(),
        "right" => b"\x1b[C".to_vec(),
        "left" => b"\x1b[D".to_vec(),
        "home" => b"\x1b[H".to_vec(),
        "end" => b"\x1b[F".to_vec(),
        "page-up" | "pgup" => b"\x1b[5~".to_vec(),
        "page-down" | "pgdn" => b"\x1b[6~".to_vec(),
        "enter" | "return" => b"\r".to_vec(),
        "esc" | "escape" => b"\x1b".to_vec(),
        "tab" => b"\t".to_vec(),
        "backspace" => vec![0x7f],
        "f1" => b"\x1bOP".to_vec(),
        "f2" => b"\x1bOQ".to_vec(),
        "f3" => b"\x1bOR".to_vec(),
        "f4" => b"\x1bOS".to_vec(),
        "f5" => b"\x1b[15~".to_vec(),
        "f6" => b"\x1b[17~".to_vec(),
        "f7" => b"\x1b[18~".to_vec(),
        "f8" => b"\x1b[19~".to_vec(),
        "f9" => b"\x1b[20~".to_vec(),
        "f10" => b"\x1b[21~".to_vec(),
        _ if normalized.starts_with("ctrl-") && normalized.len() == 6 => {
            let ch = normalized.as_bytes()[5];
            if ch.is_ascii_alphabetic() {
                vec![ch & 0x1f]
            } else {
                return Err(anyhow!("unsupported control key: {s}"));
            }
        }
        _ if s.len() == 1 => s.as_bytes().to_vec(),
        _ if s.starts_with("\\x") => parse_hex_bytes(s)?,
        _ => return Err(anyhow!("unsupported key name: {s}")),
    };
    Ok(bytes)
}

fn parse_hex_bytes(s: &str) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for part in s.split("\\x").filter(|p| !p.is_empty()) {
        if part.len() < 2 {
            return Err(anyhow!("invalid hex byte: {s}"));
        }
        out.push(u8::from_str_radix(&part[..2], 16)?);
    }
    Ok(out)
}

fn print_raw_text(text: &str) -> Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(text.as_bytes())?;
    stdout.flush()?;
    Ok(())
}

fn animate_cli_output(text: &str, rt: &Runtime) -> Result<bool> {
    let plain = strip_ansi_for_animation(text);
    let lines = split_lines_preserve_tail(&plain);
    let (_, rows) = size()?;
    let viewport_height = rows.saturating_sub(1) as usize;
    if viewport_height == 0 {
        return Ok(false);
    }

    let height = viewport_height
        .min(rt.max_lines.max(1))
        .min(lines.len().max(1));
    let split_at = lines.len().saturating_sub(height);
    let (head_lines, tail_lines) = lines.split_at(split_at);
    debug_cli_animation(
        lines.len(),
        head_lines.len(),
        tail_lines.len(),
        viewport_height,
        height,
    );
    let mut stdout = io::stdout().lock();

    for line in head_lines {
        writeln!(stdout, "{line}")?;
    }
    write!(stdout, "\x1b[?25l")?;
    for _ in 0..height {
        writeln!(stdout)?;
    }
    stdout.flush()?;

    for frame in 0..=rt.frames {
        write!(stdout, "\x1b[{height}A")?;
        for (row, line) in tail_lines.iter().enumerate() {
            let rendered = if frame == rt.frames {
                line.clone()
            } else {
                render_cli_frame(line, frame, row, rt.frames, rt.effect)
            };
            write!(stdout, "{rendered}\r\n")?;
        }
        for _ in tail_lines.len()..height {
            writeln!(stdout)?;
        }
        stdout.flush()?;
        sleep_frame(rt.duration_ms, rt.frames.saturating_add(1));
    }

    write!(stdout, "\x1b[?25h")?;
    stdout.flush()?;
    Ok(true)
}

fn debug_cli_animation(
    total_lines: usize,
    head_lines: usize,
    tail_lines: usize,
    viewport_height: usize,
    height: usize,
) {
    if std::env::var_os("BAERU_DEBUG_CLI").is_none() {
        return;
    }
    let _ = writeln!(
        io::stderr(),
        "[baeru cli] total_lines={total_lines} head_lines={head_lines} tail_lines={tail_lines} viewport_height={viewport_height} height={height}"
    );
}

fn split_lines_preserve_tail(text: &str) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut lines: Vec<String> = text.lines().map(|line| line.to_string()).collect();
    if text.ends_with('\n') {
        lines.push(String::new());
    }
    lines
}

fn strip_ansi_for_animation(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            out.push(ch);
            continue;
        }

        match chars.peek().copied() {
            Some('[') => {
                chars.next();
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                let mut saw_esc = false;
                for next in chars.by_ref() {
                    if saw_esc && next == '\\' {
                        break;
                    }
                    saw_esc = next == '\u{1b}';
                    if next == '\u{7}' {
                        break;
                    }
                }
            }
            _ => {
                let _ = chars.next();
            }
        }
    }

    out
}

fn render_cli_frame(
    line: &str,
    frame: usize,
    row: usize,
    total_frames: usize,
    effect: EffectKind,
) -> String {
    match effect {
        EffectKind::Plain => line.to_string(),
        EffectKind::Sweep => cli_sweep_frame(line, frame, total_frames),
        EffectKind::Fade => cli_fade_frame(line, frame, total_frames),
        EffectKind::Coalesce => cli_coalesce_frame(line, frame, row, total_frames),
    }
}

fn cli_sweep_frame(line: &str, frame: usize, total_frames: usize) -> String {
    let progress = frame as f32 / total_frames.max(1) as f32;
    let chars: Vec<char> = line.chars().collect();
    let visible = (progress * chars.len() as f32).ceil() as usize;
    chars
        .iter()
        .enumerate()
        .map(|(idx, ch)| if idx < visible { *ch } else { ' ' })
        .collect()
}

fn cli_fade_frame(line: &str, frame: usize, total_frames: usize) -> String {
    let progress = frame as f32 / total_frames.max(1) as f32;
    if progress >= 0.7 {
        line.to_string()
    } else {
        line.chars()
            .map(|ch| if ch.is_whitespace() { ch } else { ' ' })
            .collect()
    }
}

fn cli_coalesce_frame(line: &str, frame: usize, row: usize, total_frames: usize) -> String {
    let progress = frame as f32 / total_frames.max(1) as f32;
    let eased = 1.0 - (1.0 - progress).powi(3);
    let chars: Vec<char> = line.chars().collect();
    let len = chars.len().max(1);

    chars
        .iter()
        .enumerate()
        .map(|(col, &ch)| {
            if ch.is_whitespace() {
                return ch;
            }
            let jitter = pseudo_random_01(row as u64, col as u64) * 0.35;
            let threshold = (col as f32 / len as f32) * 0.45 + jitter;
            if eased >= threshold {
                ch
            } else {
                CLI_SCRAMBLE[(row * 31 + col * 17 + frame * 7) % CLI_SCRAMBLE.len()]
            }
        })
        .collect()
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

fn capture_command_text(command: &[OsString]) -> Result<(String, i32)> {
    let mut cmd = std::process::Command::new(&command[0]);
    cmd.args(&command[1..]);
    let output = cmd.output()?;
    let mut text = String::new();
    text.push_str(&String::from_utf8_lossy(&output.stdout));
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok((text, output.status.code().unwrap_or(1)))
}

fn read_stdin_text() -> Result<(String, i32)> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    Ok((input, 0))
}

fn env_flag(name: &str) -> bool {
    std::env::var_os(name).is_some()
}

fn is_term_dumb() -> bool {
    std::env::var("TERM").is_ok_and(|term| term == "dumb")
}
