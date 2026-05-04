use crate::{
    model::{EffectKind, Feature, Rgb, Runtime, CLI_SCRAMBLE},
    support::{env_flag, exit_with_status, print_raw_text, sleep_frame},
    theme::{gradient, lerp},
};
use anyhow::Result;
use crossterm::terminal::size;
use std::{
    ffi::OsString,
    io::{self, IsTerminal, Read, Write},
};

pub(crate) fn run_cli_backend(rt: Runtime) -> Result<()> {
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
    let settled_fg = rt
        .cli_settled_color
        .unwrap_or_else(|| rt.theme.default_fg_rgb());

    for line in head_lines {
        write_cli_colored_line(&mut stdout, line, settled_fg)?;
    }
    write!(stdout, "\x1b[?25l")?;
    for _ in 0..height {
        writeln!(stdout)?;
    }
    stdout.flush()?;

    for frame in 0..=rt.frames {
        write!(stdout, "\x1b[{height}A")?;
        let progress = frame as f32 / rt.frames.max(1) as f32;
        let fg = if frame == rt.frames {
            settled_fg
        } else {
            cli_frame_fg(rt, progress)
        };
        for (row, line) in tail_lines.iter().enumerate() {
            let rendered = if frame == rt.frames {
                line.clone()
            } else {
                render_cli_frame(line, frame, row, rt.frames, rt.effect)
            };
            write_cli_colored_line(&mut stdout, &rendered, fg)?;
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

fn cli_frame_fg(rt: &Runtime, progress: f32) -> Rgb {
    if let (Some(start), Some(end)) = (rt.cli_gradient_start, rt.cli_gradient_end) {
        let k = progress.clamp(0.0, 1.0);
        return Rgb(
            lerp(start.0, end.0, k),
            lerp(start.1, end.1, k),
            lerp(start.2, end.2, k),
        );
    }
    if let Some(color) = rt.cli_animation_color {
        return color;
    }
    gradient(&rt.theme.foreground, progress.clamp(0.0, 1.0))
        .unwrap_or_else(|| rt.theme.default_fg_rgb())
}

fn write_cli_colored_line(stdout: &mut io::StdoutLock<'_>, line: &str, fg: Rgb) -> io::Result<()> {
    write!(
        stdout,
        "\x1b[38;2;{};{};{}m{}\x1b[0m\r\n",
        fg.0, fg.1, fg.2, line
    )
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
                let mut prev = '\0';
                for next in chars.by_ref() {
                    if next == '\u{7}' || (prev == '\u{1b}' && next == '\\') {
                        break;
                    }
                    prev = next;
                }
            }
            _ => {}
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
    let chars: Vec<char> = line.chars().collect();
    let visible =
        ((frame as f32 / total_frames.max(1) as f32) * chars.len() as f32).ceil() as usize;
    chars
        .iter()
        .enumerate()
        .map(|(idx, ch)| if idx < visible { *ch } else { ' ' })
        .collect()
}

fn cli_fade_frame(line: &str, frame: usize, total_frames: usize) -> String {
    if frame * 10 >= total_frames.max(1) * 6 {
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
    let output = std::process::Command::new(&command[0])
        .args(&command[1..])
        .output()?;
    let mut text = String::new();
    text.push_str(&String::from_utf8_lossy(&output.stdout));
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok((text, output.status.code().unwrap_or(1)))
}

fn read_stdin_text() -> Result<(String, i32)> {
    let mut text = String::new();
    io::stdin().read_to_string(&mut text)?;
    Ok((text, 0))
}
