use crate::{
    keymap::{compile_keymap, read_keymap},
    model::{
        Backend, Cli, ConfigFile, EffectKind, Feature, HighlightRule, HighlightRuleConfig,
        MaskRuleConfig, Mode, OutputTransformKind, OutputTransformRule, Profile, ReplaceRuleConfig,
        Rgb, Runtime,
    },
    support::{env_flag, is_term_dumb},
    theme::{builtin_theme, parse_optional_rgb, read_theme},
};
use anyhow::{bail, Context, Result};
use regex::Regex;
use std::{
    collections::{BTreeSet, HashMap},
    env,
    ffi::OsString,
    fs,
    io::{self, IsTerminal},
    path::{Path, PathBuf},
};

pub(crate) fn build_runtime(cli: Cli) -> Result<Runtime> {
    let stdin_is_tty = io::stdin().is_terminal();
    let stdout_is_tty = io::stdout().is_terminal();
    let term_is_dumb = is_term_dumb();
    let no_color = env_flag("NO_COLOR");
    build_runtime_with_env(cli, stdin_is_tty, stdout_is_tty, term_is_dumb, no_color)
}

fn build_runtime_with_env(
    cli: Cli,
    stdin_is_tty: bool,
    stdout_is_tty: bool,
    term_is_dumb: bool,
    no_color: bool,
) -> Result<Runtime> {
    let Cli {
        selection,
        files,
        animation,
        cli_render,
        highlight,
        transform,
        command: cli_command,
    } = cli;

    if cli_command.is_empty() && stdin_is_tty {
        bail!("command is required");
    }
    let command = cli_command.clone();

    let config = match resolve_config_path(files.config_file.as_deref()) {
        Some(path) => read_config(&path)
            .with_context(|| format!("failed to read config: {}", path.display()))?,
        None => ConfigFile::default(),
    };
    let profile = find_profile(&config, &command).cloned().unwrap_or_default();

    let requested_backend = selection
        .backend
        .or(profile.backend)
        .unwrap_or(Backend::Auto);
    let backend = resolve_backend(
        requested_backend,
        &command,
        stdin_is_tty,
        stdout_is_tty,
        term_is_dumb,
    );

    let mut features = resolve_features(&profile, selection.mode, backend);
    if !stdout_is_tty || term_is_dumb {
        features.remove(&Feature::Reveal);
        features.remove(&Feature::InlineAnimation);
        features.remove(&Feature::LiveColor);
        features.remove(&Feature::Splash);
        features.remove(&Feature::LiveRender);
    }
    if no_color {
        features.remove(&Feature::LiveColor);
    }

    let effect = selection
        .effect
        .or(profile.effect)
        .unwrap_or_else(|| default_effect_for_backend(backend, &features));

    let capture_ms = profile.capture_ms.unwrap_or(animation.capture_ms);
    let duration_ms = profile.duration_ms.unwrap_or(animation.duration_ms);
    let frames = profile.frames.unwrap_or(animation.frames).max(1);
    let live_render_duration_ms = profile
        .live_render_duration_ms
        .unwrap_or(animation.live_render_duration_ms);
    let live_render_mouse_quiet_ms = profile
        .live_render_mouse_quiet_ms
        .unwrap_or(animation.live_render_mouse_quiet_ms);
    let animation_color_fade = profile
        .animation_color_fade
        .unwrap_or(animation.animation_color_fade);
    let animation_color_darken_factor = profile
        .animation_color_darken_factor
        .unwrap_or(animation.animation_color_darken_factor)
        .clamp(0.0, 1.0);

    let palette = profile.palette.unwrap_or(files.palette);
    let needs_theme = (backend == Backend::Tui
        && (features.contains(&Feature::Reveal)
            || features.contains(&Feature::LiveColor)
            || features.contains(&Feature::LiveRender)))
        || (backend == Backend::Cli && features.contains(&Feature::InlineAnimation));
    let theme = if needs_theme {
        let theme_path = files.theme_file.or(profile.theme_file);
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
        if let Some(path) = profile.keymap_file.or(files.keymap_file) {
            let km = read_keymap(&path)
                .with_context(|| format!("failed to read keymap: {}", path.display()))?;
            keymap_text.extend(km.keymap);
        }
        keymap_text.extend(profile.keymap);
        compile_keymap(&keymap_text)?
    } else {
        HashMap::new()
    };

    let max_lines = profile.max_lines.unwrap_or(cli_render.max_lines);
    let max_bytes = profile.max_bytes.unwrap_or(cli_render.max_bytes);
    let animate_over_limit = profile
        .animate_over_limit
        .unwrap_or(cli_render.animate_over_limit);
    let cli_animation_color = parse_optional_rgb(profile.cli_animation_color.as_deref())
        .context("invalid cli_animation_color")?;
    let cli_settled_color = parse_optional_rgb(profile.cli_settled_color.as_deref())
        .context("invalid cli_settled_color")?;
    let cli_gradient_start = parse_optional_rgb(profile.cli_gradient_start.as_deref())
        .context("invalid cli_gradient_start")?;
    let cli_gradient_end = parse_optional_rgb(profile.cli_gradient_end.as_deref())
        .context("invalid cli_gradient_end")?;
    let highlight_default_color = parse_highlight_color(
        profile
            .highlight_color
            .as_deref()
            .or(highlight.highlight_color.as_deref()),
    )
    .context("invalid highlight_color")?;
    let highlight_rules = compile_highlight_rules(
        CliHighlightRuleOptions {
            patterns: &highlight.highlight,
            command: &highlight.highlight_command,
            capture_tui_screenshot: highlight.highlight_capture_tui_screenshot,
            capture_cli_text: highlight.highlight_capture_cli_text,
            output_dir: highlight.highlight_output_dir.as_deref(),
            output_prefix: highlight.highlight_output_prefix.as_deref(),
            default_color: highlight_default_color,
        },
        &profile.highlight_rules,
    )
    .context("failed to compile highlight rules")?;
    let output_transforms = compile_output_transform_rules(
        &transform.replace,
        &transform.mask,
        &transform.mask_char,
        &profile.replace_rules,
        &profile.mask_rules,
    )
    .context("failed to compile output transform rules")?;

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
        live_render_duration_ms,
        live_render_mouse_quiet_ms,
        animation_color_fade,
        animation_color_darken_factor,
        max_lines,
        max_bytes,
        animate_over_limit,
        cli_animation_color,
        cli_settled_color,
        cli_gradient_start,
        cli_gradient_end,
        no_theme_after_reveal: animation.no_theme_after_reveal,
        highlight_rules,
        output_transforms,
    })
}

fn parse_highlight_color(value: Option<&str>) -> Result<Rgb> {
    Ok(parse_optional_rgb(value)?.unwrap_or(Rgb(255, 255, 0)))
}

struct CliHighlightRuleOptions<'a> {
    patterns: &'a [String],
    command: &'a [String],
    capture_tui_screenshot: bool,
    capture_cli_text: bool,
    output_dir: Option<&'a Path>,
    output_prefix: Option<&'a str>,
    default_color: Rgb,
}

fn compile_highlight_rules(
    cli_options: CliHighlightRuleOptions<'_>,
    profile_rules: &[HighlightRuleConfig],
) -> Result<Vec<HighlightRule>> {
    let mut rules = Vec::new();
    for pattern in cli_options.patterns {
        rules.push(HighlightRule {
            key: pattern.clone(),
            pattern: pattern.clone(),
            regex: Regex::new(pattern)
                .with_context(|| format!("invalid highlight regex: {pattern}"))?,
            color: cli_options.default_color,
            command: (!cli_options.command.is_empty()).then(|| {
                cli_options
                    .command
                    .iter()
                    .map(|part| OsString::from(part.as_str()))
                    .collect::<Vec<_>>()
            }),
            capture_tui_screenshot: cli_options.capture_tui_screenshot,
            capture_cli_text: cli_options.capture_cli_text,
            output_dir: cli_options.output_dir.map(Path::to_path_buf),
            output_prefix: cli_options.output_prefix.map(str::to_string),
        });
    }
    for rule in profile_rules {
        let key = rule.key.clone().unwrap_or_else(|| rule.pattern.clone());
        let color = parse_highlight_color(rule.color.as_deref().or(Some("#ffff00")))?;
        rules.push(HighlightRule {
            key,
            pattern: rule.pattern.clone(),
            regex: Regex::new(&rule.pattern)
                .with_context(|| format!("invalid highlight regex: {}", rule.pattern))?,
            color,
            command: rule.command.as_ref().map(|cmd| {
                cmd.iter()
                    .map(|part| OsString::from(part.as_str()))
                    .collect::<Vec<_>>()
            }),
            capture_tui_screenshot: rule.capture_tui_screenshot,
            capture_cli_text: rule.capture_cli_text,
            output_dir: rule.output_dir.clone(),
            output_prefix: rule.output_prefix.clone(),
        });
    }
    Ok(rules)
}

fn compile_output_transform_rules(
    cli_replace: &[String],
    cli_mask: &[String],
    cli_mask_char: &str,
    profile_replace: &[ReplaceRuleConfig],
    profile_mask: &[MaskRuleConfig],
) -> Result<Vec<OutputTransformRule>> {
    let mut rules = Vec::new();
    let default_mask_char = parse_mask_char(cli_mask_char)?;

    if !cli_replace.len().is_multiple_of(2) {
        anyhow::bail!("--replace expects PATTERN TEXT pairs");
    }
    for pair in cli_replace.chunks_exact(2) {
        let pattern = pair[0].clone();
        let replacement = pair[1].clone();
        rules.push(OutputTransformRule {
            regex: Regex::new(&pattern)
                .with_context(|| format!("invalid replace regex: {pattern}"))?,
            kind: OutputTransformKind::Replace(replacement),
        });
    }

    for pattern in cli_mask {
        rules.push(OutputTransformRule {
            regex: Regex::new(pattern).with_context(|| format!("invalid mask regex: {pattern}"))?,
            kind: OutputTransformKind::Mask(default_mask_char),
        });
    }

    for rule in profile_replace {
        rules.push(OutputTransformRule {
            regex: Regex::new(&rule.pattern)
                .with_context(|| format!("invalid replace regex: {}", rule.pattern))?,
            kind: OutputTransformKind::Replace(rule.replacement.clone()),
        });
    }

    for rule in profile_mask {
        let mask_char = parse_mask_char(rule.mask_char.as_deref().unwrap_or("*"))?;
        rules.push(OutputTransformRule {
            regex: Regex::new(&rule.pattern)
                .with_context(|| format!("invalid mask regex: {}", rule.pattern))?,
            kind: OutputTransformKind::Mask(mask_char),
        });
    }

    Ok(rules)
}

fn parse_mask_char(value: &str) -> Result<char> {
    let mut chars = value.chars();
    let ch = chars
        .next()
        .with_context(|| "mask char must not be empty".to_string())?;
    if chars.next().is_some() {
        anyhow::bail!("mask char must be a single character");
    }
    Ok(ch)
}

fn resolve_config_path(explicit: Option<&Path>) -> Option<PathBuf> {
    let cwd = env::current_dir().ok();
    let xdg_config_home = env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    let home = env::var_os("HOME").map(PathBuf::from);
    resolve_config_path_from(
        explicit,
        cwd.as_deref(),
        xdg_config_home.as_deref(),
        home.as_deref(),
    )
}

fn resolve_config_path_from(
    explicit: Option<&Path>,
    cwd: Option<&Path>,
    xdg_config_home: Option<&Path>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return Some(path.to_path_buf());
    }

    let mut candidates = Vec::new();
    if let Some(cwd) = cwd {
        candidates.push(cwd.join("baeru.yml"));
    }
    if let Some(xdg) = xdg_config_home {
        candidates.push(xdg.join("baeru").join("baeru.yml"));
    }
    if let Some(home) = home {
        candidates.push(home.join(".config").join("baeru").join("baeru.yml"));
        candidates.push(home.join(".baeru.yml"));
    }

    candidates.into_iter().find(|path| path.exists())
}

fn resolve_backend(
    requested: Backend,
    command: &[OsString],
    stdin_is_tty: bool,
    stdout_is_tty: bool,
    term_is_dumb: bool,
) -> Backend {
    match requested {
        Backend::Tui | Backend::Cli | Backend::Raw => requested,
        Backend::Auto => {
            if !stdout_is_tty || term_is_dumb {
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
        Mode::Reveal => vec![Feature::Reveal],
        Mode::ColorLive => vec![Feature::LiveColor],
        Mode::Splash => vec![Feature::Splash],
        Mode::LiveRender => vec![Feature::LiveRender],
    }
}

fn default_effect_for_backend(backend: Backend, _features: &BTreeSet<Feature>) -> EffectKind {
    match backend {
        Backend::Tui => EffectKind::Fade,
        Backend::Cli => EffectKind::Coalesce,
        Backend::Raw | Backend::Auto => EffectKind::Plain,
    }
}

fn read_config(path: &Path) -> Result<ConfigFile> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        AnimationArgs, CliRenderArgs, FileArgs, HighlightArgs, MatchSpec, Profile, SelectionArgs,
        TransformArgs,
    };
    use std::{
        fs,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn find_profile_matches_command_and_args_prefix() {
        let config = ConfigFile {
            profiles: vec![
                Profile {
                    _name: Some("git-status".to_string()),
                    r#match: MatchSpec {
                        command: Some("git".to_string()),
                        path: None,
                        args_prefix: vec!["status".to_string()],
                    },
                    backend: Some(Backend::Cli),
                    ..Profile::default()
                },
                Profile {
                    _name: Some("git-generic".to_string()),
                    r#match: MatchSpec {
                        command: Some("git".to_string()),
                        path: None,
                        args_prefix: vec![],
                    },
                    backend: Some(Backend::Tui),
                    ..Profile::default()
                },
            ],
        };

        let command = vec![
            OsString::from("git"),
            OsString::from("status"),
            OsString::from("--short"),
        ];

        let profile = find_profile(&config, &command).expect("profile should match");
        assert_eq!(profile._name.as_deref(), Some("git-status"));
        assert_eq!(profile.backend, Some(Backend::Cli));
    }

    #[test]
    fn resolve_features_defaults_by_backend() {
        let profile = Profile::default();

        let cli_features = resolve_features(&profile, None, Backend::Cli);
        assert!(cli_features.contains(&Feature::InlineAnimation));

        let tui_features = resolve_features(&profile, None, Backend::Tui);
        assert!(tui_features.contains(&Feature::Reveal));
        assert!(tui_features.contains(&Feature::LiveColor));
    }

    #[test]
    fn default_effect_is_fade_for_tui() {
        let profile = Profile::default();
        let features = resolve_features(&profile, None, Backend::Tui);

        assert_eq!(
            default_effect_for_backend(Backend::Tui, &features),
            EffectKind::Fade
        );
    }

    #[test]
    fn cli_mode_adds_live_color_feature() {
        let profile = Profile::default();
        let features = resolve_features(&profile, Some(Mode::ColorLive), Backend::Tui);

        assert!(features.contains(&Feature::LiveColor));
    }

    #[test]
    fn reveal_mode_does_not_imply_live_color_feature() {
        let profile = Profile::default();
        let features = resolve_features(&profile, Some(Mode::Reveal), Backend::Tui);

        assert!(features.contains(&Feature::Reveal));
        assert!(!features.contains(&Feature::LiveColor));
    }

    #[test]
    fn read_config_parses_cli_color_overrides() {
        let path = unique_temp_file("baeru-config-test.yml");
        fs::write(
            &path,
            r##"
profiles:
  - name: ls-inline
    match:
      command: ls
    backend: cli
    features:
      - inline_animation
    cli_settled_color: "#b6ffd0"
    cli_gradient_start: "#004d26"
    cli_gradient_end: "#eafff2"
    animation_color_fade: true
    animation_color_darken_factor: 0.2
"##,
        )
        .expect("fixture config should be written");

        let config = read_config(&path).expect("config should parse");
        assert_eq!(config.profiles.len(), 1);
        let profile = &config.profiles[0];
        assert_eq!(profile._name.as_deref(), Some("ls-inline"));
        assert_eq!(profile.backend, Some(Backend::Cli));
        assert_eq!(profile.cli_settled_color.as_deref(), Some("#b6ffd0"));
        assert_eq!(profile.cli_gradient_end.as_deref(), Some("#eafff2"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn read_config_parses_highlight_rules() {
        let path = unique_temp_file("baeru-highlight-config.yml");
        fs::write(
            &path,
            r##"
profiles:
  - name: alerts
    match:
      command: journalctl
    backend: cli
    highlight_color: "#ffff00"
    highlight_rules:
      - key: error
        pattern: "(?i)error"
        color: "#ffcc00"
        capture_cli_text: true
        command: ["echo", "matched"]
"##,
        )
        .expect("fixture config should be written");

        let config = read_config(&path).expect("config should parse");
        let profile = &config.profiles[0];
        assert_eq!(profile.highlight_color.as_deref(), Some("#ffff00"));
        assert_eq!(profile.highlight_rules.len(), 1);
        assert_eq!(profile.highlight_rules[0].key.as_deref(), Some("error"));
        assert!(profile.highlight_rules[0].capture_cli_text);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn read_config_parses_replace_and_mask_rules() {
        let path = unique_temp_file("baeru-transform-config.yml");
        fs::write(
            &path,
            r##"
profiles:
  - name: sanitize
    match:
      command: env
    backend: cli
    replace_rules:
      - pattern: "TOKEN=.*"
        replacement: "TOKEN=[redacted]"
    mask_rules:
      - pattern: "(?i)password=.*"
        mask_char: "#"
"##,
        )
        .expect("fixture config should be written");

        let config = read_config(&path).expect("config should parse");
        let profile = &config.profiles[0];
        assert_eq!(profile.replace_rules.len(), 1);
        assert_eq!(profile.replace_rules[0].replacement, "TOKEN=[redacted]");
        assert_eq!(profile.mask_rules.len(), 1);
        assert_eq!(profile.mask_rules[0].mask_char.as_deref(), Some("#"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn build_runtime_with_env_applies_cli_profile_and_colors() {
        let config_path = unique_temp_file("baeru-runtime-config.yml");
        let theme_path = unique_temp_file("baeru-runtime-theme.yml");
        fs::write(
            &theme_path,
            r##"
name: fixture
default_fg: "#eeeeee"
default_bg: "#111111"
force_default: true
foreground:
  - { at: 0.0, color: "#001122" }
  - { at: 1.0, color: "#ddeeff" }
background:
  - { at: 0.0, color: "#000000" }
  - { at: 1.0, color: "#ffffff" }
"##,
        )
        .expect("fixture theme should be written");
        fs::write(
            &config_path,
            format!(
                r##"
profiles:
  - name: ls-inline
    match:
      command: ls
    backend: cli
    features:
      - inline_animation
    effect: sweep
    theme_file: '{}'
    cli_settled_color: "#b6ffd0"
    cli_gradient_start: "#004d26"
    cli_gradient_end: "#eafff2"
    animation_color_fade: true
    animation_color_darken_factor: 0.2
    duration_ms: 900
    frames: 20
"##,
                theme_path.display()
            ),
        )
        .expect("fixture config should be written");

        let runtime = build_runtime_with_env(
            Cli {
                selection: SelectionArgs {
                    backend: None,
                    mode: None,
                    effect: None,
                },
                files: FileArgs {
                    config_file: Some(config_path.clone()),
                    theme_file: None,
                    palette: "jirai-pink".to_string(),
                    keymap_file: None,
                },
                animation: AnimationArgs {
                    capture_ms: 360,
                    duration_ms: 720,
                    frames: 24,
                    live_render_duration_ms: 90,
                    live_render_mouse_quiet_ms: 180,
                    animation_color_fade: false,
                    animation_color_darken_factor: 0.25,
                    no_theme_after_reveal: false,
                },
                cli_render: CliRenderArgs {
                    max_lines: 200,
                    max_bytes: 1_000_000,
                    animate_over_limit: false,
                },
                highlight: HighlightArgs {
                    highlight: vec![],
                    highlight_color: None,
                    highlight_command: vec![],
                    highlight_capture_cli_text: false,
                    highlight_capture_tui_screenshot: false,
                    highlight_output_dir: None,
                    highlight_output_prefix: None,
                },
                transform: TransformArgs {
                    replace: vec![],
                    mask: vec![],
                    mask_char: "*".to_string(),
                },
                command: vec![OsString::from("ls")],
            },
            true,
            true,
            false,
            false,
        )
        .expect("runtime should build");

        assert_eq!(runtime.backend, Backend::Cli);
        assert!(runtime.features.contains(&Feature::InlineAnimation));
        assert_eq!(runtime.effect, EffectKind::Sweep);
        assert_eq!(runtime.frames, 20);
        assert_eq!(runtime.duration_ms, 900);
        assert!(runtime.animation_color_fade);
        assert_eq!(runtime.animation_color_darken_factor, 0.2);
        assert_eq!(
            runtime.cli_settled_color,
            Some(crate::model::Rgb(0xb6, 0xff, 0xd0))
        );
        assert_eq!(
            runtime.cli_gradient_start,
            Some(crate::model::Rgb(0x00, 0x4d, 0x26))
        );
        assert_eq!(
            runtime.cli_gradient_end,
            Some(crate::model::Rgb(0xea, 0xff, 0xf2))
        );

        let _ = fs::remove_file(config_path);
        let _ = fs::remove_file(theme_path);
    }

    #[test]
    fn build_runtime_with_env_disables_live_color_for_no_color() {
        let runtime = build_runtime_with_env(
            Cli {
                selection: SelectionArgs {
                    backend: Some(Backend::Tui),
                    mode: Some(Mode::Reveal),
                    effect: None,
                },
                files: FileArgs {
                    config_file: None,
                    theme_file: None,
                    palette: "jirai-pink".to_string(),
                    keymap_file: None,
                },
                animation: AnimationArgs {
                    capture_ms: 360,
                    duration_ms: 720,
                    frames: 24,
                    live_render_duration_ms: 90,
                    live_render_mouse_quiet_ms: 180,
                    animation_color_fade: false,
                    animation_color_darken_factor: 0.25,
                    no_theme_after_reveal: false,
                },
                cli_render: CliRenderArgs {
                    max_lines: 200,
                    max_bytes: 1_000_000,
                    animate_over_limit: false,
                },
                highlight: HighlightArgs {
                    highlight: vec![],
                    highlight_color: None,
                    highlight_command: vec![],
                    highlight_capture_cli_text: false,
                    highlight_capture_tui_screenshot: false,
                    highlight_output_dir: None,
                    highlight_output_prefix: None,
                },
                transform: TransformArgs {
                    replace: vec![],
                    mask: vec![],
                    mask_char: "*".to_string(),
                },
                command: vec![OsString::from("htop")],
            },
            true,
            true,
            false,
            true,
        )
        .expect("runtime should build");

        assert!(runtime.features.contains(&Feature::Reveal));
        assert!(!runtime.features.contains(&Feature::LiveColor));
    }

    #[test]
    fn build_runtime_with_env_errors_when_tty_has_no_command() {
        let err = build_runtime_with_env(
            Cli {
                selection: SelectionArgs {
                    backend: None,
                    mode: None,
                    effect: None,
                },
                files: FileArgs {
                    config_file: None,
                    theme_file: None,
                    palette: "default".to_string(),
                    keymap_file: None,
                },
                animation: AnimationArgs {
                    capture_ms: 360,
                    duration_ms: 720,
                    frames: 24,
                    live_render_duration_ms: 90,
                    live_render_mouse_quiet_ms: 180,
                    animation_color_fade: false,
                    animation_color_darken_factor: 0.25,
                    no_theme_after_reveal: false,
                },
                cli_render: CliRenderArgs {
                    max_lines: 200,
                    max_bytes: 1_000_000,
                    animate_over_limit: false,
                },
                highlight: HighlightArgs {
                    highlight: vec![],
                    highlight_color: None,
                    highlight_command: vec![],
                    highlight_capture_cli_text: false,
                    highlight_capture_tui_screenshot: false,
                    highlight_output_dir: None,
                    highlight_output_prefix: None,
                },
                transform: TransformArgs {
                    replace: vec![],
                    mask: vec![],
                    mask_char: "*".to_string(),
                },
                command: vec![],
            },
            true,
            true,
            false,
            false,
        )
        .expect_err("runtime should reject empty tty command");

        assert_eq!(err.to_string(), "command is required");
    }

    #[test]
    fn build_runtime_with_env_applies_cli_highlight_actions() {
        let runtime = build_runtime_with_env(
            Cli {
                selection: SelectionArgs {
                    backend: Some(Backend::Cli),
                    mode: None,
                    effect: None,
                },
                files: FileArgs {
                    config_file: None,
                    theme_file: None,
                    palette: "default".to_string(),
                    keymap_file: None,
                },
                animation: AnimationArgs {
                    capture_ms: 360,
                    duration_ms: 720,
                    frames: 24,
                    live_render_duration_ms: 90,
                    live_render_mouse_quiet_ms: 180,
                    animation_color_fade: false,
                    animation_color_darken_factor: 0.25,
                    no_theme_after_reveal: false,
                },
                cli_render: CliRenderArgs {
                    max_lines: 200,
                    max_bytes: 1_000_000,
                    animate_over_limit: false,
                },
                highlight: HighlightArgs {
                    highlight: vec!["error".to_string()],
                    highlight_color: Some("#ffcc00".to_string()),
                    highlight_command: vec!["echo".to_string(), "matched".to_string()],
                    highlight_capture_cli_text: true,
                    highlight_capture_tui_screenshot: true,
                    highlight_output_dir: Some(PathBuf::from("./captures")),
                    highlight_output_prefix: Some("alerts-{key}-".to_string()),
                },
                transform: TransformArgs {
                    replace: vec![],
                    mask: vec![],
                    mask_char: "*".to_string(),
                },
                command: vec![OsString::from("journalctl")],
            },
            true,
            true,
            false,
            false,
        )
        .expect("runtime should build");

        let rule = runtime
            .highlight_rules
            .first()
            .expect("cli highlight rule should exist");
        assert_eq!(
            rule.command.as_ref().expect("command should exist").len(),
            2
        );
        assert!(rule.capture_cli_text);
        assert!(rule.capture_tui_screenshot);
        assert_eq!(rule.output_dir.as_deref(), Some(Path::new("./captures")));
        assert_eq!(rule.output_prefix.as_deref(), Some("alerts-{key}-"));
        assert_eq!(rule.color, Rgb(0xff, 0xcc, 0x00));
    }

    #[test]
    fn resolve_config_path_prefers_explicit_then_local_then_xdg_then_home() {
        let base = unique_temp_dir("baeru-config-locations");
        let cwd = base.join("cwd");
        let xdg = base.join("xdg");
        let home = base.join("home");
        fs::create_dir_all(&cwd).expect("cwd dir should exist");
        fs::create_dir_all(xdg.join("baeru")).expect("xdg dir should exist");
        fs::create_dir_all(home.join(".config").join("baeru"))
            .expect("home config dir should exist");

        let explicit = base.join("explicit.yml");
        let local = cwd.join("baeru.yml");
        let xdg_path = xdg.join("baeru").join("baeru.yml");
        let home_config = home.join(".config").join("baeru").join("baeru.yml");
        let home_dot = home.join(".baeru.yml");

        fs::write(&home_dot, "profiles: []").expect("home dot config should be written");
        assert_eq!(
            resolve_config_path_from(None, Some(&cwd), None, Some(&home)),
            Some(home_dot.clone())
        );

        fs::write(&home_config, "profiles: []").expect("home config should be written");
        assert_eq!(
            resolve_config_path_from(None, Some(&cwd), None, Some(&home)),
            Some(home_config.clone())
        );

        fs::write(&xdg_path, "profiles: []").expect("xdg config should be written");
        assert_eq!(
            resolve_config_path_from(None, Some(&cwd), Some(&xdg), Some(&home)),
            Some(xdg_path.clone())
        );

        fs::write(&local, "profiles: []").expect("local config should be written");
        assert_eq!(
            resolve_config_path_from(None, Some(&cwd), Some(&xdg), Some(&home)),
            Some(local.clone())
        );

        fs::write(&explicit, "profiles: []").expect("explicit config should be written");
        assert_eq!(
            resolve_config_path_from(Some(&explicit), Some(&cwd), Some(&xdg), Some(&home)),
            Some(explicit.clone())
        );

        let _ = fs::remove_dir_all(base);
    }

    fn unique_temp_file(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should move forward")
            .as_nanos();
        std::env::temp_dir().join(format!("{name}-{nanos}"))
    }

    fn unique_temp_dir(name: &str) -> PathBuf {
        unique_temp_file(name)
    }
}
