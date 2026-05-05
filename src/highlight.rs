use crate::model::{HighlightRule, Rgb};
use anyhow::Result;
use serde::Serialize;
use std::{
    collections::{hash_map::DefaultHasher, HashMap},
    ffi::OsString,
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    process::Stdio,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone)]
pub(crate) struct HighlightEvaluation {
    pub(crate) colors: Vec<Vec<Option<Rgb>>>,
    pub(crate) triggers: Vec<HighlightTrigger>,
}

#[derive(Debug, Clone)]
pub(crate) struct HighlightTrigger {
    pub(crate) key: String,
    pub(crate) pattern: String,
    pub(crate) matched_texts: Vec<String>,
    pub(crate) command: Option<Vec<OsString>>,
    pub(crate) capture_cli_text: bool,
    pub(crate) capture_tui_screenshot: bool,
    pub(crate) output_dir: Option<PathBuf>,
    pub(crate) output_prefix: Option<String>,
    pub(crate) fingerprint: u64,
}

#[derive(Debug, Default)]
pub(crate) struct TriggerState {
    seen: HashMap<String, u64>,
}

#[derive(Debug, Serialize)]
struct TriggerManifest<'a> {
    backend: &'a str,
    key: &'a str,
    pattern: &'a str,
    matches: &'a [String],
    capture_kind: Option<&'a str>,
    capture_path: Option<String>,
}

pub(crate) fn evaluate_lines(lines: &[String], rules: &[HighlightRule]) -> HighlightEvaluation {
    let mut colors = lines
        .iter()
        .map(|line| vec![None; line.chars().count()])
        .collect::<Vec<_>>();
    let mut triggers = Vec::new();

    for rule in rules {
        let mut matched_texts = Vec::new();
        for (row, line) in lines.iter().enumerate() {
            for found in rule.regex.find_iter(line) {
                matched_texts.push(found.as_str().to_string());
                let start = byte_to_char_idx(line, found.start());
                let end = byte_to_char_idx(line, found.end());
                for col in start..end.min(colors[row].len()) {
                    colors[row][col] = Some(rule.color);
                }
            }
        }

        if !matched_texts.is_empty() {
            triggers.push(HighlightTrigger {
                key: rule.key.clone(),
                pattern: rule.pattern.clone(),
                matched_texts: matched_texts.clone(),
                command: rule.command.clone(),
                capture_cli_text: rule.capture_cli_text,
                capture_tui_screenshot: rule.capture_tui_screenshot,
                output_dir: rule.output_dir.clone(),
                output_prefix: rule.output_prefix.clone(),
                fingerprint: fingerprint(&rule.key, &matched_texts),
            });
        }
    }

    HighlightEvaluation { colors, triggers }
}

pub(crate) fn filter_new_triggers(
    state: &mut TriggerState,
    triggers: Vec<HighlightTrigger>,
) -> Vec<HighlightTrigger> {
    let mut fresh = Vec::new();
    for trigger in triggers {
        let changed = state
            .seen
            .get(&trigger.key)
            .is_none_or(|prev| *prev != trigger.fingerprint);
        if changed {
            state.seen.insert(trigger.key.clone(), trigger.fingerprint);
            fresh.push(trigger);
        }
    }
    fresh
}

pub(crate) fn dispatch_cli_triggers(triggers: &[HighlightTrigger], full_text: &str) -> Result<()> {
    for trigger in triggers {
        let output_dir = ensure_output_dir(trigger.output_dir.as_deref())?;
        let capture_path = if trigger.capture_cli_text {
            Some(write_capture_file(
                &output_dir,
                trigger,
                "cli",
                "cli_text",
                "txt",
                full_text.as_bytes(),
            )?)
        } else {
            None
        };
        let manifest_path = write_manifest(&output_dir, "cli", trigger, capture_path.as_deref())?;
        run_trigger_command(trigger, "cli", &manifest_path, capture_path.as_deref())?;
    }
    Ok(())
}

pub(crate) fn dispatch_tui_triggers(
    triggers: &[HighlightTrigger],
    screenshot_svg: Option<&str>,
) -> Result<()> {
    for trigger in triggers {
        let output_dir = ensure_output_dir(trigger.output_dir.as_deref())?;
        let capture_path = if trigger.capture_tui_screenshot {
            screenshot_svg
                .map(|svg| {
                    write_capture_file(
                        &output_dir,
                        trigger,
                        "tui",
                        "tui_svg",
                        "svg",
                        svg.as_bytes(),
                    )
                })
                .transpose()?
        } else {
            None
        };
        let manifest_path = write_manifest(&output_dir, "tui", trigger, capture_path.as_deref())?;
        run_trigger_command(trigger, "tui", &manifest_path, capture_path.as_deref())?;
    }
    Ok(())
}

fn run_trigger_command(
    trigger: &HighlightTrigger,
    backend: &str,
    manifest_path: &Path,
    capture_path: Option<&Path>,
) -> Result<()> {
    let Some(command) = trigger.command.as_ref() else {
        return Ok(());
    };
    if command.is_empty() {
        return Ok(());
    }

    let matches_json = serde_json::to_string(&trigger.matched_texts)?;
    let mut cmd = std::process::Command::new(&command[0]);
    cmd.args(&command[1..])
        .env("BAERU_HIGHLIGHT_BACKEND", backend)
        .env("BAERU_HIGHLIGHT_KEY", &trigger.key)
        .env("BAERU_HIGHLIGHT_PATTERN", &trigger.pattern)
        .env("BAERU_HIGHLIGHT_MATCHES_JSON", matches_json)
        .env("BAERU_HIGHLIGHT_EVENT_JSON", manifest_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null());
    if let Some(path) = capture_path {
        cmd.env("BAERU_HIGHLIGHT_CAPTURE_PATH", path);
        cmd.env(
            "BAERU_HIGHLIGHT_CAPTURE_KIND",
            if backend == "cli" {
                "cli_text"
            } else {
                "tui_svg"
            },
        );
    }
    if let Ok(cwd) = std::env::current_dir() {
        cmd.current_dir(cwd);
    }
    let _child = cmd.spawn()?;
    Ok(())
}

fn ensure_output_dir(specified: Option<&Path>) -> Result<PathBuf> {
    let dir = match specified {
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir()?.join("tmp").join("baeru-artifacts"),
    };
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn write_capture_file(
    dir: &Path,
    trigger: &HighlightTrigger,
    backend: &str,
    capture_kind: &str,
    ext: &str,
    bytes: &[u8],
) -> Result<PathBuf> {
    let suffix = timestamp_suffix();
    let path = dir.join(format_output_filename(
        trigger,
        backend,
        capture_kind,
        ext,
        suffix,
    ));
    fs::write(&path, bytes)?;
    Ok(path)
}

fn write_manifest(
    dir: &Path,
    backend: &str,
    trigger: &HighlightTrigger,
    capture_path: Option<&Path>,
) -> Result<PathBuf> {
    let suffix = timestamp_suffix();
    let manifest = TriggerManifest {
        backend,
        key: &trigger.key,
        pattern: &trigger.pattern,
        matches: &trigger.matched_texts,
        capture_kind: capture_path.map(|_| {
            if backend == "cli" {
                "cli_text"
            } else {
                "tui_svg"
            }
        }),
        capture_path: capture_path.map(|path| path.display().to_string()),
    };
    let path = dir.join(format_output_filename(
        trigger, backend, "event", "json", suffix,
    ));
    fs::write(&path, serde_json::to_vec_pretty(&manifest)?)?;
    Ok(path)
}

fn format_output_filename(
    trigger: &HighlightTrigger,
    backend: &str,
    capture_kind: &str,
    ext: &str,
    timestamp: u128,
) -> String {
    let prefix = render_output_prefix(trigger, backend, capture_kind, ext, timestamp);
    let base = match capture_kind {
        "event" => format!("{}-{}-event", sanitize_key(&trigger.key), timestamp),
        _ => format!("{}-{}", sanitize_key(&trigger.key), timestamp),
    };
    format!("{prefix}{base}.{ext}")
}

fn render_output_prefix(
    trigger: &HighlightTrigger,
    backend: &str,
    capture_kind: &str,
    ext: &str,
    timestamp: u128,
) -> String {
    let Some(template) = trigger.output_prefix.as_deref() else {
        return String::new();
    };

    let mut rendered = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '{' {
            rendered.push(ch);
            continue;
        }

        let mut token = String::new();
        let mut closed = false;
        while let Some(next) = chars.next() {
            if next == '}' {
                closed = true;
                break;
            }
            token.push(next);
        }

        if !closed {
            rendered.push('{');
            rendered.push_str(&token);
            break;
        }

        match token.as_str() {
            "key" => rendered.push_str(&sanitize_key(&trigger.key)),
            "backend" => rendered.push_str(backend),
            "capture_kind" => rendered.push_str(capture_kind),
            "ext" => rendered.push_str(ext),
            "timestamp" => rendered.push_str(&timestamp.to_string()),
            token if token.starts_with("env:") => {
                if let Ok(value) = std::env::var(&token[4..]) {
                    rendered.push_str(&sanitize_segment(&value));
                }
            }
            _ => {
                rendered.push('{');
                rendered.push_str(&token);
                rendered.push('}');
            }
        }
    }

    sanitize_segment(&rendered)
}

fn sanitize_key(key: &str) -> String {
    sanitize_segment(key)
}

fn sanitize_segment(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "highlight".to_string()
    } else {
        out
    }
}

fn timestamp_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn byte_to_char_idx(text: &str, byte_idx: usize) -> usize {
    text[..byte_idx.min(text.len())].chars().count()
}

fn fingerprint(key: &str, matches: &[String]) -> u64 {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    for matched in matches {
        matched.hash(&mut hasher);
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::HighlightRule;
    use regex::Regex;

    fn rule(pattern: &str) -> HighlightRule {
        HighlightRule {
            key: pattern.to_string(),
            pattern: pattern.to_string(),
            regex: Regex::new(pattern).unwrap(),
            color: Rgb(255, 255, 0),
            command: None,
            capture_tui_screenshot: false,
            capture_cli_text: false,
            output_dir: None,
            output_prefix: None,
        }
    }

    #[test]
    fn format_output_filename_keeps_default_when_prefix_is_missing() {
        let trigger = HighlightTrigger {
            key: "error".to_string(),
            pattern: "error".to_string(),
            matched_texts: vec!["error".to_string()],
            command: None,
            capture_cli_text: true,
            capture_tui_screenshot: false,
            output_dir: None,
            output_prefix: None,
            fingerprint: 1,
        };

        assert_eq!(
            format_output_filename(&trigger, "cli", "cli_text", "txt", 1234),
            "error-1234.txt"
        );
        assert_eq!(
            format_output_filename(&trigger, "cli", "event", "json", 1234),
            "error-1234-event.json"
        );
    }

    #[test]
    fn format_output_filename_renders_template_variables_in_prefix() {
        let trigger = HighlightTrigger {
            key: "error/fatal".to_string(),
            pattern: "error".to_string(),
            matched_texts: vec!["error".to_string()],
            command: None,
            capture_cli_text: true,
            capture_tui_screenshot: false,
            output_dir: None,
            output_prefix: Some("run-{backend}-{capture_kind}-{key}-{timestamp}-".to_string()),
            fingerprint: 1,
        };

        assert_eq!(
            format_output_filename(&trigger, "cli", "cli_text", "txt", 1234),
            "run-cli-cli_text-error_fatal-1234-error_fatal-1234.txt"
        );
    }

    #[test]
    fn render_output_prefix_supports_env_variables() {
        let trigger = HighlightTrigger {
            key: "warn".to_string(),
            pattern: "warn".to_string(),
            matched_texts: vec!["warn".to_string()],
            command: None,
            capture_cli_text: true,
            capture_tui_screenshot: false,
            output_dir: None,
            output_prefix: Some("job-{env:BAERU_TEST_PREFIX}-".to_string()),
            fingerprint: 1,
        };

        std::env::set_var("BAERU_TEST_PREFIX", "nightly build");
        let rendered = render_output_prefix(&trigger, "cli", "cli_text", "txt", 1234);
        std::env::remove_var("BAERU_TEST_PREFIX");

        assert_eq!(rendered, "job-nightly_build-");
    }

    #[test]
    fn evaluate_lines_marks_matching_chars() {
        let lines = vec!["alpha beta".to_string()];
        let evaluation = evaluate_lines(&lines, &[rule("beta")]);
        assert!(evaluation.colors[0][6].is_some());
        assert!(evaluation.colors[0][9].is_some());
        assert_eq!(evaluation.triggers.len(), 1);
    }

    #[test]
    fn filter_new_triggers_suppresses_same_fingerprint() {
        let lines = vec!["warn warn".to_string()];
        let evaluation = evaluate_lines(&lines, &[rule("warn")]);
        let mut state = TriggerState::default();
        assert_eq!(
            filter_new_triggers(&mut state, evaluation.triggers.clone()).len(),
            1
        );
        assert_eq!(
            filter_new_triggers(&mut state, evaluation.triggers.clone()).len(),
            0
        );
    }
}
