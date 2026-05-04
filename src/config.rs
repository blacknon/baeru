use crate::{
    keymap::{compile_keymap, read_keymap},
    model::{Backend, Cli, ConfigFile, EffectKind, Feature, Mode, Profile, Runtime},
    support::{env_flag, is_term_dumb},
    theme::{builtin_theme, parse_optional_rgb, read_theme},
};
use anyhow::{Context, Result};
use std::{
    collections::{BTreeSet, HashMap},
    ffi::OsString,
    fs,
    io::{self, IsTerminal},
    path::{Path, PathBuf},
};

pub(crate) fn build_runtime(cli: Cli) -> Result<Runtime> {
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
