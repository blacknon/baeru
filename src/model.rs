use clap::{ArgAction, Args, Parser, ValueEnum};
use regex::Regex;
use serde::Deserialize;
use std::{
    collections::{BTreeSet, HashMap},
    ffi::OsString,
    path::PathBuf,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Backend {
    Auto,
    Tui,
    Cli,
    Raw,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Feature {
    #[serde(alias = "reveal")]
    Reveal,
    #[serde(alias = "live-color")]
    LiveColor,
    #[serde(alias = "keymap")]
    Keymap,
    #[serde(alias = "inline-animation")]
    InlineAnimation,
    #[serde(alias = "splash")]
    Splash,
    #[serde(alias = "live-render")]
    LiveRender,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Mode {
    Reveal,
    #[value(name = "color_live")]
    #[serde(alias = "color-live")]
    ColorLive,
    Splash,
    /// Experimental: rebuild the live screen from VT100 state and flash changed cells.
    #[value(name = "live_render")]
    #[serde(alias = "live-render")]
    LiveRender,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum EffectKind {
    Coalesce,
    Glitch,
    Matrix,
    Scanline,
    Scatter,
    Sweep,
    Wipe,
    Fade,
    Plain,
}

#[derive(Debug, Clone, Args)]
#[command(next_help_heading = "Selection")]
pub(crate) struct SelectionArgs {
    #[arg(short = 'b', long, value_enum)]
    pub(crate) backend: Option<Backend>,

    #[arg(short = 'm', long, value_enum)]
    pub(crate) mode: Option<Mode>,

    #[arg(short = 'E', long, value_enum)]
    pub(crate) effect: Option<EffectKind>,
}

#[derive(Debug, Clone, Args)]
#[command(next_help_heading = "Files")]
pub(crate) struct FileArgs {
    #[arg(short = 'c', long)]
    pub(crate) config_file: Option<PathBuf>,

    #[arg(short = 't', long)]
    pub(crate) theme_file: Option<PathBuf>,

    #[arg(short = 'p', long, default_value = "default")]
    pub(crate) palette: String,

    #[arg(short = 'k', long)]
    pub(crate) keymap_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Args)]
#[command(next_help_heading = "Animation")]
pub(crate) struct AnimationArgs {
    #[arg(short = 'C', long, default_value_t = 360)]
    pub(crate) capture_ms: u64,

    #[arg(short = 'd', long, default_value_t = 720)]
    pub(crate) duration_ms: u64,

    #[arg(short = 'f', long, default_value_t = 24)]
    pub(crate) frames: usize,

    #[arg(short = 'D', long, default_value_t = 90)]
    pub(crate) live_render_duration_ms: u64,

    #[arg(short = 'Q', long, default_value_t = 180)]
    pub(crate) live_render_mouse_quiet_ms: u64,

    #[arg(long, default_value_t = false)]
    pub(crate) animation_color_fade: bool,

    #[arg(long, default_value_t = 0.25)]
    pub(crate) animation_color_darken_factor: f32,

    #[arg(short = 'N', long, default_value_t = false)]
    pub(crate) no_theme_after_reveal: bool,
}

#[derive(Debug, Clone, Args)]
#[command(next_help_heading = "CLI")]
pub(crate) struct CliRenderArgs {
    #[arg(short = 'l', long, default_value_t = 200)]
    pub(crate) max_lines: usize,

    #[arg(short = 'B', long, default_value_t = 1_000_000)]
    pub(crate) max_bytes: usize,

    #[arg(short = 'o', long, default_value_t = false)]
    pub(crate) animate_over_limit: bool,
}

#[derive(Debug, Clone, Args)]
#[command(next_help_heading = "Highlight")]
pub(crate) struct HighlightArgs {
    #[arg(short = 'e', long = "highlight")]
    pub(crate) highlight: Vec<String>,

    #[arg(short = 'H', long)]
    pub(crate) highlight_color: Option<String>,

    #[arg(long = "highlight-command")]
    pub(crate) highlight_command: Vec<String>,

    #[arg(long, default_value_t = false)]
    pub(crate) highlight_capture_cli_text: bool,

    #[arg(long, default_value_t = false)]
    pub(crate) highlight_capture_tui_screenshot: bool,

    #[arg(long = "highlight-output-dir")]
    pub(crate) highlight_output_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Args)]
#[command(next_help_heading = "Transform")]
pub(crate) struct TransformArgs {
    #[arg(
        short = 'R',
        long = "replace",
        value_names = ["PATTERN", "TEXT"],
        num_args = 2,
        action = ArgAction::Append
    )]
    pub(crate) replace: Vec<String>,

    #[arg(short = 'M', long = "mask")]
    pub(crate) mask: Vec<String>,

    #[arg(long = "mask-char", default_value = "*")]
    pub(crate) mask_char: String,
}

#[derive(Debug, Clone, Parser)]
#[command(name = "baeru", version, about = "Make existing terminal apps glow up")]
pub(crate) struct Cli {
    #[command(flatten)]
    pub(crate) selection: SelectionArgs,

    #[command(flatten)]
    pub(crate) files: FileArgs,

    #[command(flatten)]
    pub(crate) animation: AnimationArgs,

    #[command(flatten)]
    pub(crate) cli_render: CliRenderArgs,

    #[command(flatten)]
    pub(crate) highlight: HighlightArgs,

    #[command(flatten)]
    pub(crate) transform: TransformArgs,

    #[arg(last = true, allow_hyphen_values = true)]
    pub(crate) command: Vec<OsString>,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct ConfigFile {
    #[serde(default)]
    pub(crate) profiles: Vec<Profile>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub(crate) struct Profile {
    #[serde(rename = "name")]
    pub(crate) _name: Option<String>,
    #[serde(default)]
    pub(crate) r#match: MatchSpec,
    pub(crate) backend: Option<Backend>,
    #[serde(default)]
    pub(crate) features: Vec<Feature>,
    pub(crate) mode: Option<Mode>,
    pub(crate) effect: Option<EffectKind>,
    pub(crate) theme_file: Option<PathBuf>,
    pub(crate) palette: Option<String>,
    pub(crate) keymap_file: Option<PathBuf>,
    #[serde(default)]
    pub(crate) keymap: HashMap<String, String>,
    pub(crate) capture_ms: Option<u64>,
    pub(crate) duration_ms: Option<u64>,
    pub(crate) frames: Option<usize>,
    pub(crate) live_render_duration_ms: Option<u64>,
    pub(crate) live_render_mouse_quiet_ms: Option<u64>,
    pub(crate) animation_color_fade: Option<bool>,
    pub(crate) animation_color_darken_factor: Option<f32>,
    pub(crate) max_lines: Option<usize>,
    pub(crate) max_bytes: Option<usize>,
    pub(crate) animate_over_limit: Option<bool>,
    pub(crate) cli_animation_color: Option<String>,
    pub(crate) cli_settled_color: Option<String>,
    pub(crate) cli_gradient_start: Option<String>,
    pub(crate) cli_gradient_end: Option<String>,
    pub(crate) highlight_color: Option<String>,
    #[serde(default)]
    pub(crate) highlight_rules: Vec<HighlightRuleConfig>,
    #[serde(default)]
    pub(crate) replace_rules: Vec<ReplaceRuleConfig>,
    #[serde(default)]
    pub(crate) mask_rules: Vec<MaskRuleConfig>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub(crate) struct MatchSpec {
    pub(crate) command: Option<String>,
    pub(crate) path: Option<PathBuf>,
    #[serde(default)]
    pub(crate) args_prefix: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct KeymapFile {
    #[serde(default)]
    pub(crate) keymap: HashMap<String, String>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub(crate) struct HighlightRuleConfig {
    pub(crate) key: Option<String>,
    pub(crate) pattern: String,
    pub(crate) color: Option<String>,
    pub(crate) command: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) capture_tui_screenshot: bool,
    #[serde(default)]
    pub(crate) capture_cli_text: bool,
    pub(crate) output_dir: Option<PathBuf>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub(crate) struct ReplaceRuleConfig {
    pub(crate) pattern: String,
    pub(crate) replacement: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub(crate) struct MaskRuleConfig {
    pub(crate) pattern: String,
    pub(crate) mask_char: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Theme {
    #[serde(rename = "name")]
    pub(crate) _name: Option<String>,
    pub(crate) default_fg: Option<String>,
    pub(crate) default_bg: Option<String>,
    #[serde(default)]
    pub(crate) force_default: bool,
    #[serde(default)]
    pub(crate) palette_map: HashMap<u8, String>,
    #[serde(default)]
    pub(crate) background_palette_map: HashMap<u8, String>,
    #[serde(default)]
    pub(crate) foreground: Vec<ColorStop>,
    #[serde(default)]
    pub(crate) background: Vec<ColorStop>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ColorStop {
    pub(crate) at: f32,
    pub(crate) color: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rgb(pub(crate) u8, pub(crate) u8, pub(crate) u8);

#[derive(Debug, Clone)]
pub(crate) struct Runtime {
    pub(crate) backend: Backend,
    pub(crate) features: BTreeSet<Feature>,
    pub(crate) effect: EffectKind,
    pub(crate) command: Vec<OsString>,
    pub(crate) theme: Theme,
    pub(crate) keymap: HashMap<Vec<u8>, Vec<u8>>,
    pub(crate) capture_ms: u64,
    pub(crate) duration_ms: u64,
    pub(crate) frames: usize,
    pub(crate) live_render_duration_ms: u64,
    pub(crate) live_render_mouse_quiet_ms: u64,
    pub(crate) animation_color_fade: bool,
    pub(crate) animation_color_darken_factor: f32,
    pub(crate) max_lines: usize,
    pub(crate) max_bytes: usize,
    pub(crate) animate_over_limit: bool,
    pub(crate) cli_animation_color: Option<Rgb>,
    pub(crate) cli_settled_color: Option<Rgb>,
    pub(crate) cli_gradient_start: Option<Rgb>,
    pub(crate) cli_gradient_end: Option<Rgb>,
    pub(crate) no_theme_after_reveal: bool,
    pub(crate) highlight_rules: Vec<HighlightRule>,
    pub(crate) output_transforms: Vec<OutputTransformRule>,
}

#[derive(Debug, Clone)]
pub(crate) struct HighlightRule {
    pub(crate) key: String,
    pub(crate) pattern: String,
    pub(crate) regex: Regex,
    pub(crate) color: Rgb,
    pub(crate) command: Option<Vec<OsString>>,
    pub(crate) capture_tui_screenshot: bool,
    pub(crate) capture_cli_text: bool,
    pub(crate) output_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub(crate) struct OutputTransformRule {
    pub(crate) regex: Regex,
    pub(crate) kind: OutputTransformKind,
}

#[derive(Debug, Clone)]
pub(crate) enum OutputTransformKind {
    Replace(String),
    Mask(char),
}

pub(crate) const CLI_SCRAMBLE: &[char] = &[
    '░', '▒', '▓', '█', '◆', '◇', '○', '●', '◌', '◍', '·', '*', '+', '#', '%', '@', '/', '\\', '|',
    '-',
];

pub(crate) const ALT_SCREEN_ENTER_SEQUENCES: &[&[u8]] =
    &[b"\x1b[?1049h", b"\x1b[?1047h", b"\x1b[?47h"];

pub(crate) const LIVE_RENDER_MOUSE_ENABLE_SEQUENCES: &[&[u8]] = &[
    b"\x1b[?1000h",
    b"\x1b[?1002h",
    b"\x1b[?1003h",
    b"\x1b[?1005h",
    b"\x1b[?1006h",
    b"\x1b[?1007h",
    b"\x1b[?1015h",
];

pub(crate) const LIVE_RENDER_MOUSE_DISABLE_SEQUENCES: &[&[u8]] = &[
    b"\x1b[?1000l",
    b"\x1b[?1002l",
    b"\x1b[?1003l",
    b"\x1b[?1005l",
    b"\x1b[?1006l",
    b"\x1b[?1007l",
    b"\x1b[?1015l",
];

pub(crate) const LIVE_RENDER_INPUT_MODE_ENABLE_SEQUENCES: &[&[u8]] = &[b"\x1b[?1h", b"\x1b="];

pub(crate) const LIVE_RENDER_INPUT_MODE_DISABLE_SEQUENCES: &[&[u8]] = &[b"\x1b[?1l", b"\x1b>"];

pub(crate) const LIVE_RENDER_PASTE_MODE_ENABLE_SEQUENCES: &[&[u8]] = &[b"\x1b[?2004h"];

pub(crate) const LIVE_RENDER_PASTE_MODE_DISABLE_SEQUENCES: &[&[u8]] = &[b"\x1b[?2004l"];

pub(crate) const LIVE_RENDER_RESET_SEQUENCES: &[u8] =
    b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1007l\x1b[?1015l\x1b[?1l\x1b>\x1b[?2004l";
