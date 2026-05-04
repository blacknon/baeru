use clap::{Parser, ValueEnum};
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
#[serde(rename_all = "kebab-case")]
pub(crate) enum Feature {
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
pub(crate) enum Mode {
    Reveal,
    ColorLive,
    Splash,
    /// Experimental: rebuild the live screen from VT100 state and flash changed cells.
    LiveRender,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum EffectKind {
    Coalesce,
    Sweep,
    Fade,
    Plain,
}

#[derive(Debug, Parser)]
#[command(name = "baeru", version, about = "Make existing terminal apps glow up")]
pub(crate) struct Cli {
    #[arg(long, value_enum)]
    pub(crate) backend: Option<Backend>,

    #[arg(long, value_enum)]
    pub(crate) mode: Option<Mode>,

    #[arg(long, value_enum)]
    pub(crate) effect: Option<EffectKind>,

    #[arg(long)]
    pub(crate) config_file: Option<PathBuf>,

    #[arg(long)]
    pub(crate) theme_file: Option<PathBuf>,

    #[arg(long, default_value = "jirai-pink")]
    pub(crate) palette: String,

    #[arg(long)]
    pub(crate) keymap_file: Option<PathBuf>,

    #[arg(long, default_value_t = 360)]
    pub(crate) capture_ms: u64,

    #[arg(long, default_value_t = 720)]
    pub(crate) duration_ms: u64,

    #[arg(long, default_value_t = 24)]
    pub(crate) frames: usize,

    #[arg(long, default_value_t = 200)]
    pub(crate) max_lines: usize,

    #[arg(long, default_value_t = 1_000_000)]
    pub(crate) max_bytes: usize,

    #[arg(long, default_value_t = false)]
    pub(crate) animate_over_limit: bool,

    #[arg(long, default_value_t = false)]
    pub(crate) no_theme_after_reveal: bool,

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
    pub(crate) max_lines: Option<usize>,
    pub(crate) max_bytes: Option<usize>,
    pub(crate) animate_over_limit: Option<bool>,
    pub(crate) cli_animation_color: Option<String>,
    pub(crate) cli_settled_color: Option<String>,
    pub(crate) cli_gradient_start: Option<String>,
    pub(crate) cli_gradient_end: Option<String>,
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
    pub(crate) max_lines: usize,
    pub(crate) max_bytes: usize,
    pub(crate) animate_over_limit: bool,
    pub(crate) cli_animation_color: Option<Rgb>,
    pub(crate) cli_settled_color: Option<Rgb>,
    pub(crate) cli_gradient_start: Option<Rgb>,
    pub(crate) cli_gradient_end: Option<Rgb>,
    pub(crate) no_theme_after_reveal: bool,
}

pub(crate) const CLI_SCRAMBLE: &[char] = &[
    '░', '▒', '▓', '█', '◆', '◇', '○', '●', '◌', '◍', '·', '*', '+', '#', '%', '@', '/', '\\', '|',
    '-',
];

pub(crate) const ALT_SCREEN_ENTER_SEQUENCES: &[&[u8]] =
    &[b"\x1b[?1049h", b"\x1b[?1047h", b"\x1b[?47h"];
