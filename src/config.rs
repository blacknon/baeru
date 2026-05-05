use crate::{
    keymap::{compile_keymap, read_keymap},
    model::{Backend, Cli, ConfigFile, EffectKind, Feature, Mode, Profile, Runtime},
    support::{env_flag, is_term_dumb},
    theme::{builtin_theme, parse_optional_rgb, read_theme},
};
use anyhow::{Context, Result};
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
    let backend = resolve_backend(
        requested_backend,
        &command,
        stdin_is_tty,
        stdout_is_tty,
        term_is_dumb,
    );

    let mut features = resolve_features(&profile, cli.mode, backend);
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

    let effect = cli
        .effect
        .or(profile.effect)
        .unwrap_or_else(|| default_effect_for_backend(backend, &features));

    let capture_ms = profile.capture_ms.unwrap_or(cli.capture_ms);
    let duration_ms = profile.duration_ms.unwrap_or(cli.duration_ms);
    let frames = profile.frames.unwrap_or(cli.frames).max(1);
    let live_render_duration_ms = profile
        .live_render_duration_ms
        .unwrap_or(cli.live_render_duration_ms);
    let live_render_mouse_quiet_ms = profile
        .live_render_mouse_quiet_ms
        .unwrap_or(cli.live_render_mouse_quiet_ms);
    let animation_color_fade = profile
        .animation_color_fade
        .unwrap_or(cli.animation_color_fade);
    let animation_color_darken_factor = profile
        .animation_color_darken_factor
        .unwrap_or(cli.animation_color_darken_factor)
        .clamp(0.0, 1.0);

    let palette = profile.palette.unwrap_or(cli.palette);
    let needs_theme = (backend == Backend::Tui
        && (features.contains(&Feature::Reveal)
            || features.contains(&Feature::LiveColor)
            || features.contains(&Feature::LiveRender)))
        || (backend == Backend::Cli && features.contains(&Feature::InlineAnimation));
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
    let cli_animation_color = parse_optional_rgb(profile.cli_animation_color.as_deref())
        .context("invalid cli_animation_color")?;
    let cli_settled_color = parse_optional_rgb(profile.cli_settled_color.as_deref())
        .context("invalid cli_settled_color")?;
    let cli_gradient_start = parse_optional_rgb(profile.cli_gradient_start.as_deref())
        .context("invalid cli_gradient_start")?;
    let cli_gradient_end = parse_optional_rgb(profile.cli_gradient_end.as_deref())
        .context("invalid cli_gradient_end")?;

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
        no_theme_after_reveal: cli.no_theme_after_reveal,
    })
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
    use crate::model::{MatchSpec, Profile};
    use std::{
        fs,
        path::PathBuf,
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
                backend: None,
                mode: None,
                effect: None,
                config_file: Some(config_path.clone()),
                theme_file: None,
                palette: "jirai-pink".to_string(),
                keymap_file: None,
                capture_ms: 360,
                duration_ms: 720,
                frames: 24,
                live_render_duration_ms: 90,
                live_render_mouse_quiet_ms: 180,
                animation_color_fade: false,
                animation_color_darken_factor: 0.25,
                max_lines: 200,
                max_bytes: 1_000_000,
                animate_over_limit: false,
                no_theme_after_reveal: false,
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
                backend: Some(Backend::Tui),
                mode: Some(Mode::Reveal),
                effect: None,
                config_file: None,
                theme_file: None,
                palette: "jirai-pink".to_string(),
                keymap_file: None,
                capture_ms: 360,
                duration_ms: 720,
                frames: 24,
                live_render_duration_ms: 90,
                live_render_mouse_quiet_ms: 180,
                animation_color_fade: false,
                animation_color_darken_factor: 0.25,
                max_lines: 200,
                max_bytes: 1_000_000,
                animate_over_limit: false,
                no_theme_after_reveal: false,
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
