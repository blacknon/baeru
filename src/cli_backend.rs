use crate::{
    highlight::{dispatch_cli_triggers, evaluate_lines},
    model::{EffectKind, Feature, Rgb, Runtime, CLI_SCRAMBLE},
    support::{env_flag, exit_with_status, print_raw_text, sleep_frame},
    theme::{darken, gradient, indexed_color, lerp},
};
use anyhow::Result;
use crossterm::terminal::size;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::{
    ffi::OsString,
    io::{self, IsTerminal, Read, Write},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliDisplayColor {
    Default,
    Indexed(u8),
    Rgb(Rgb),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CliStyle {
    fg: CliDisplayColor,
    bg: CliDisplayColor,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
}

impl Default for CliStyle {
    fn default() -> Self {
        Self {
            fg: CliDisplayColor::Default,
            bg: CliDisplayColor::Default,
            bold: false,
            dim: false,
            italic: false,
            underline: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CliStyledChar {
    ch: char,
    style: CliStyle,
}

pub(crate) fn run_cli_backend(rt: Runtime) -> Result<()> {
    let stdout_is_tty = io::stdout().is_terminal();
    let (text, exit_code) = if rt.command.is_empty() {
        read_stdin_text()?
    } else {
        capture_command_text(&rt.command, stdout_is_tty)?
    };

    let plain_lines = split_ansi_plain_lines_preserve_tail(&text);
    let evaluation = evaluate_lines(&plain_lines, &rt.highlight_rules);
    dispatch_cli_triggers(&evaluation.triggers, &text)?;

    if !stdout_is_tty
        || !rt.features.contains(&Feature::InlineAnimation)
        || env_flag("NO_COLOR")
        || text.is_empty()
    {
        if stdout_is_tty {
            render_cli_without_animation(&text, &rt, &evaluation.colors)?;
        } else {
            print_raw_text(&text)?;
        }
        exit_with_status(exit_code);
    }

    if text.len() > rt.max_bytes && !rt.animate_over_limit {
        render_cli_without_animation(&text, &rt, &evaluation.colors)?;
        exit_with_status(exit_code);
    }

    if rt.effect == EffectKind::Plain {
        render_cli_without_animation(&text, &rt, &evaluation.colors)?;
        exit_with_status(exit_code);
    }

    if !animate_cli_output(&text, &rt, &plain_lines, &evaluation.colors)? {
        render_cli_without_animation(&text, &rt, &evaluation.colors)?;
    }
    exit_with_status(exit_code);
}

fn animate_cli_output(
    text: &str,
    rt: &Runtime,
    plain_lines: &[String],
    highlight_colors: &[Vec<Option<Rgb>>],
) -> Result<bool> {
    if cli_preserves_source_ansi(rt) {
        let styled_lines = split_ansi_lines_preserve_tail(text);
        return animate_cli_styled_output(&styled_lines, rt, highlight_colors);
    }

    let (_, rows) = size()?;
    let viewport_height = rows.saturating_sub(1) as usize;
    if viewport_height == 0 {
        return Ok(false);
    }

    let height = viewport_height
        .min(rt.max_lines.max(1))
        .min(plain_lines.len().max(1));
    let split_at = plain_lines.len().saturating_sub(height);
    let (head_lines, tail_lines) = plain_lines.split_at(split_at);
    let (head_highlights, tail_highlights) = highlight_colors.split_at(split_at);
    debug_cli_animation(
        plain_lines.len(),
        head_lines.len(),
        tail_lines.len(),
        viewport_height,
        height,
    );
    let mut stdout = io::stdout().lock();
    let settled_fg = rt
        .cli_settled_color
        .unwrap_or_else(|| rt.theme.default_fg_rgb());

    for (line, highlights) in head_lines.iter().zip(head_highlights) {
        write_cli_colored_line(&mut stdout, line, highlights, settled_fg)?;
    }
    write!(stdout, "\x1b[?25l")?;
    for _ in 0..height {
        writeln!(stdout)?;
    }
    stdout.flush()?;

    for frame in 0..=rt.frames {
        write!(stdout, "\x1b[{height}A")?;
        let progress = frame as f32 / rt.frames.max(1) as f32;
        let fg = cli_frame_fg(rt, settled_fg, progress);
        for (row, (line, highlights)) in tail_lines.iter().zip(tail_highlights).enumerate() {
            let rendered = if frame == rt.frames {
                line.clone()
            } else {
                render_cli_frame(line, frame, row, tail_lines.len(), rt.frames, rt.effect)
            };
            write_cli_colored_line(&mut stdout, &rendered, highlights, fg)?;
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

fn animate_cli_styled_output(
    lines: &[Vec<CliStyledChar>],
    rt: &Runtime,
    highlight_colors: &[Vec<Option<Rgb>>],
) -> Result<bool> {
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
    let (head_highlights, tail_highlights) = highlight_colors.split_at(split_at);
    debug_cli_animation(
        lines.len(),
        head_lines.len(),
        tail_lines.len(),
        viewport_height,
        height,
    );
    let mut stdout = io::stdout().lock();

    for (line, highlights) in head_lines.iter().zip(head_highlights) {
        write_cli_styled_line(
            &mut stdout,
            line,
            &plain_line_from_styled(line),
            highlights,
            rt,
            1.0,
        )?;
    }
    write!(stdout, "\x1b[?25l")?;
    for _ in 0..height {
        writeln!(stdout)?;
    }
    stdout.flush()?;

    for frame in 0..=rt.frames {
        write!(stdout, "\x1b[{height}A")?;
        let progress = frame as f32 / rt.frames.max(1) as f32;
        for (row, (line, highlights)) in tail_lines.iter().zip(tail_highlights).enumerate() {
            let plain = plain_line_from_styled(line);
            let rendered = if frame == rt.frames {
                plain
            } else {
                render_cli_frame(&plain, frame, row, tail_lines.len(), rt.frames, rt.effect)
            };
            write_cli_styled_line(&mut stdout, line, &rendered, highlights, rt, progress)?;
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

fn render_cli_without_animation(
    text: &str,
    rt: &Runtime,
    highlight_colors: &[Vec<Option<Rgb>>],
) -> Result<()> {
    let mut stdout = io::stdout().lock();
    if cli_preserves_source_ansi(rt) {
        let lines = split_ansi_lines_preserve_tail(text);
        for (line, highlights) in lines.iter().zip(highlight_colors) {
            write_cli_styled_line(
                &mut stdout,
                line,
                &plain_line_from_styled(line),
                highlights,
                rt,
                1.0,
            )?;
        }
    } else {
        let lines = split_ansi_plain_lines_preserve_tail(text);
        let settled_fg = rt
            .cli_settled_color
            .unwrap_or_else(|| rt.theme.default_fg_rgb());
        for (line, highlights) in lines.iter().zip(highlight_colors) {
            write_cli_colored_line(&mut stdout, line, highlights, settled_fg)?;
        }
    }
    stdout.flush()?;
    Ok(())
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

fn cli_frame_fg(rt: &Runtime, settled_fg: Rgb, progress: f32) -> Rgb {
    let target = if let (Some(start), Some(end)) = (rt.cli_gradient_start, rt.cli_gradient_end) {
        let k = progress.clamp(0.0, 1.0);
        Rgb(
            lerp(start.0, end.0, k),
            lerp(start.1, end.1, k),
            lerp(start.2, end.2, k),
        )
    } else if let Some(color) = rt.cli_animation_color {
        color
    } else if !rt.theme.foreground.is_empty() {
        gradient(&rt.theme.foreground, progress.clamp(0.0, 1.0)).unwrap_or(settled_fg)
    } else {
        settled_fg
    };

    fade_rgb_for_progress(rt, target, progress)
}

fn write_cli_colored_line(
    stdout: &mut io::StdoutLock<'_>,
    line: &str,
    highlights: &[Option<Rgb>],
    fg: Rgb,
) -> io::Result<()> {
    let mut highlighted = false;
    write!(stdout, "\x1b[38;2;{};{};{}m", fg.0, fg.1, fg.2)?;
    for (idx, ch) in line.chars().enumerate() {
        let next = highlights.get(idx).copied().flatten();
        match (highlighted, next) {
            (false, Some(rgb)) => {
                write!(stdout, "\x1b[48;2;{};{};{}m", rgb.0, rgb.1, rgb.2)?;
                highlighted = true;
            }
            (true, None) => {
                write!(stdout, "\x1b[49m")?;
                highlighted = false;
            }
            (true, Some(rgb)) => {
                write!(stdout, "\x1b[48;2;{};{};{}m", rgb.0, rgb.1, rgb.2)?;
            }
            (false, None) => {}
        }
        write!(stdout, "{ch}")?;
    }
    write!(stdout, "\x1b[0m\x1b[K\r\n")
}

fn write_cli_styled_line(
    stdout: &mut io::StdoutLock<'_>,
    source: &[CliStyledChar],
    rendered: &str,
    highlights: &[Option<Rgb>],
    rt: &Runtime,
    progress: f32,
) -> io::Result<()> {
    let mut style_state = None;
    for (idx, ch) in rendered.chars().enumerate() {
        let source_cell = source.get(idx).copied().unwrap_or(CliStyledChar {
            ch: ' ',
            style: CliStyle::default(),
        });
        let style = if ch == ' ' && !source_cell.ch.is_whitespace() {
            None
        } else {
            Some(merge_cli_highlight_style(
                source_cell.style,
                highlights.get(idx).copied().flatten(),
            ))
        };
        if style_state != style {
            write!(stdout, "\x1b[0m")?;
            if let Some(style) = style {
                write_cli_style(stdout, style, rt, progress)?;
            }
            style_state = style;
        }
        write!(stdout, "{ch}")?;
    }
    write!(stdout, "\x1b[0m\x1b[K\r\n")
}

fn merge_cli_highlight_style(style: CliStyle, highlight: Option<Rgb>) -> CliStyle {
    let mut merged = style;
    if let Some(rgb) = highlight {
        merged.bg = CliDisplayColor::Rgb(rgb);
    }
    merged
}

fn write_cli_style(
    stdout: &mut io::StdoutLock<'_>,
    style: CliStyle,
    rt: &Runtime,
    progress: f32,
) -> io::Result<()> {
    write_cli_display_color(stdout, true, style.fg, rt, progress)?;
    write_cli_display_color(stdout, false, style.bg, rt, progress)?;
    if style.bold {
        write!(stdout, "\x1b[1m")?;
    }
    if style.dim {
        write!(stdout, "\x1b[2m")?;
    }
    if style.italic {
        write!(stdout, "\x1b[3m")?;
    }
    if style.underline {
        write!(stdout, "\x1b[4m")?;
    }
    Ok(())
}

fn write_cli_display_color(
    stdout: &mut io::StdoutLock<'_>,
    fg: bool,
    color: CliDisplayColor,
    rt: &Runtime,
    progress: f32,
) -> io::Result<()> {
    if rt.animation_color_fade && progress < 1.0 {
        return match color {
            CliDisplayColor::Default => {
                if fg {
                    write!(stdout, "\x1b[39m\x1b[2m")
                } else {
                    write!(stdout, "\x1b[49m")
                }
            }
            CliDisplayColor::Indexed(idx) => {
                let faded = fade_rgb_for_progress(rt, indexed_color(idx), progress);
                if fg {
                    write!(stdout, "\x1b[38;2;{};{};{}m", faded.0, faded.1, faded.2)
                } else {
                    write!(stdout, "\x1b[48;2;{};{};{}m", faded.0, faded.1, faded.2)
                }
            }
            CliDisplayColor::Rgb(rgb) => {
                let faded = fade_rgb_for_progress(rt, rgb, progress);
                if fg {
                    write!(stdout, "\x1b[38;2;{};{};{}m", faded.0, faded.1, faded.2)
                } else {
                    write!(stdout, "\x1b[48;2;{};{};{}m", faded.0, faded.1, faded.2)
                }
            }
        };
    }

    match color {
        CliDisplayColor::Default => {
            if fg {
                write!(stdout, "\x1b[39m")
            } else {
                write!(stdout, "\x1b[49m")
            }
        }
        CliDisplayColor::Indexed(idx) => {
            let code = if fg {
                if idx < 8 {
                    30 + idx
                } else if idx < 16 {
                    90 + (idx - 8)
                } else {
                    return write!(stdout, "\x1b[38;5;{}m", idx);
                }
            } else if idx < 8 {
                40 + idx
            } else if idx < 16 {
                100 + (idx - 8)
            } else {
                return write!(stdout, "\x1b[48;5;{}m", idx);
            };
            write!(stdout, "\x1b[{code}m")
        }
        CliDisplayColor::Rgb(rgb) => {
            if fg {
                write!(stdout, "\x1b[38;2;{};{};{}m", rgb.0, rgb.1, rgb.2)
            } else {
                write!(stdout, "\x1b[48;2;{};{};{}m", rgb.0, rgb.1, rgb.2)
            }
        }
    }
}

fn fade_rgb_for_progress(rt: &Runtime, target: Rgb, progress: f32) -> Rgb {
    if !rt.animation_color_fade {
        return target;
    }
    let start = darken(target, rt.animation_color_darken_factor);
    let k = progress.clamp(0.0, 1.0);
    Rgb(
        lerp(start.0, target.0, k),
        lerp(start.1, target.1, k),
        lerp(start.2, target.2, k),
    )
}

fn split_ansi_lines_preserve_tail(text: &str) -> Vec<Vec<CliStyledChar>> {
    let screen = parse_cli_screen(text);
    if screen.is_empty() {
        vec![Vec::new()]
    } else {
        screen
    }
}

fn split_ansi_plain_lines_preserve_tail(text: &str) -> Vec<String> {
    split_ansi_lines_preserve_tail(text)
        .into_iter()
        .map(|line| plain_line_from_styled(&line))
        .collect()
}

fn parse_cli_screen(text: &str) -> Vec<Vec<CliStyledChar>> {
    let mut screen = CliScreen::new();
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    let mut seq = String::new();
                    for next in chars.by_ref() {
                        if ('@'..='~').contains(&next) {
                            if next == 'm' {
                                apply_sgr_sequence(&mut screen.style, &seq);
                            } else {
                                apply_cli_csi(&mut screen, next, parse_csi_params(&seq));
                            }
                            break;
                        }
                        seq.push(next);
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
            continue;
        }

        match ch {
            '\r' => {
                if matches!(chars.peek(), Some('\n')) {
                    chars.next();
                }
                screen.newline();
            }
            '\n' => screen.newline(),
            _ => screen.put_char(ch),
        }
    }

    screen.finish()
}

fn apply_sgr_sequence(style: &mut CliStyle, seq: &str) {
    let params: Vec<u16> = if seq.is_empty() {
        vec![0]
    } else {
        seq.split(';')
            .map(|part| part.parse::<u16>().unwrap_or(0))
            .collect()
    };
    let mut idx = 0;
    while idx < params.len() {
        match params[idx] {
            0 => *style = CliStyle::default(),
            1 => style.bold = true,
            2 => style.dim = true,
            3 => style.italic = true,
            4 => style.underline = true,
            22 => {
                style.bold = false;
                style.dim = false;
            }
            23 => style.italic = false,
            24 => style.underline = false,
            30..=37 => style.fg = CliDisplayColor::Indexed((params[idx] - 30) as u8),
            39 => style.fg = CliDisplayColor::Default,
            40..=47 => style.bg = CliDisplayColor::Indexed((params[idx] - 40) as u8),
            49 => style.bg = CliDisplayColor::Default,
            90..=97 => style.fg = CliDisplayColor::Indexed((params[idx] - 90 + 8) as u8),
            100..=107 => style.bg = CliDisplayColor::Indexed((params[idx] - 100 + 8) as u8),
            38 | 48 => {
                let is_fg = params[idx] == 38;
                if let Some((color, consumed)) = parse_extended_sgr_color(&params[idx + 1..]) {
                    if is_fg {
                        style.fg = color;
                    } else {
                        style.bg = color;
                    }
                    idx += consumed;
                }
            }
            _ => {}
        }
        idx += 1;
    }
}

fn apply_cli_csi(screen: &mut CliScreen, final_byte: char, params: Vec<usize>) {
    match final_byte {
        'A' => screen.move_up(params.first().copied().unwrap_or(1)),
        'B' => screen.move_down(params.first().copied().unwrap_or(1)),
        'C' => screen.move_right(params.first().copied().unwrap_or(1)),
        'D' => screen.move_left(params.first().copied().unwrap_or(1)),
        'G' => screen.set_column(params.first().copied().unwrap_or(1)),
        'H' | 'f' => {
            let row = params.first().copied().unwrap_or(1);
            let col = params.get(1).copied().unwrap_or(1);
            screen.set_position(row, col);
        }
        'K' => screen.erase_line_from_cursor(),
        _ => {}
    }
}

fn parse_csi_params(seq: &str) -> Vec<usize> {
    if seq.is_empty() {
        return vec![1];
    }
    seq.split(';')
        .map(|part| {
            if part.is_empty() {
                1
            } else {
                part.parse::<usize>().unwrap_or(1)
            }
        })
        .collect()
}

fn parse_extended_sgr_color(params: &[u16]) -> Option<(CliDisplayColor, usize)> {
    match params {
        [5, idx, ..] => Some((CliDisplayColor::Indexed(*idx as u8), 2)),
        [2, r, g, b, ..] => Some((CliDisplayColor::Rgb(Rgb(*r as u8, *g as u8, *b as u8)), 4)),
        _ => None,
    }
}

fn plain_line_from_styled(line: &[CliStyledChar]) -> String {
    line.iter().map(|cell| cell.ch).collect()
}

fn cli_preserves_source_ansi(rt: &Runtime) -> bool {
    rt.cli_animation_color.is_none()
        && rt.cli_settled_color.is_none()
        && rt.cli_gradient_start.is_none()
        && rt.cli_gradient_end.is_none()
        && !rt.theme.force_default
        && rt.theme.palette_map.is_empty()
        && rt.theme.background_palette_map.is_empty()
        && rt.theme.foreground.is_empty()
        && rt.theme.background.is_empty()
}

struct CliScreen {
    lines: Vec<Vec<CliStyledChar>>,
    cursor_row: usize,
    cursor_col: usize,
    style: CliStyle,
}

impl CliScreen {
    fn new() -> Self {
        Self {
            lines: vec![Vec::new()],
            cursor_row: 0,
            cursor_col: 0,
            style: CliStyle::default(),
        }
    }

    fn ensure_position(&mut self) {
        while self.lines.len() <= self.cursor_row {
            self.lines.push(Vec::new());
        }
        let row = &mut self.lines[self.cursor_row];
        while row.len() <= self.cursor_col {
            row.push(CliStyledChar {
                ch: ' ',
                style: CliStyle::default(),
            });
        }
    }

    fn put_char(&mut self, ch: char) {
        self.ensure_position();
        self.lines[self.cursor_row][self.cursor_col] = CliStyledChar {
            ch,
            style: self.style,
        };
        self.cursor_col += 1;
    }

    fn newline(&mut self) {
        self.cursor_row += 1;
        self.cursor_col = 0;
        while self.lines.len() <= self.cursor_row {
            self.lines.push(Vec::new());
        }
    }

    fn move_up(&mut self, count: usize) {
        self.cursor_row = self.cursor_row.saturating_sub(count.max(1));
    }

    fn move_down(&mut self, count: usize) {
        self.cursor_row += count.max(1);
        while self.lines.len() <= self.cursor_row {
            self.lines.push(Vec::new());
        }
    }

    fn move_right(&mut self, count: usize) {
        self.cursor_col += count.max(1);
    }

    fn move_left(&mut self, count: usize) {
        self.cursor_col = self.cursor_col.saturating_sub(count.max(1));
    }

    fn set_column(&mut self, column: usize) {
        self.cursor_col = column.saturating_sub(1);
    }

    fn set_position(&mut self, row: usize, col: usize) {
        self.cursor_row = row.saturating_sub(1);
        self.cursor_col = col.saturating_sub(1);
        while self.lines.len() <= self.cursor_row {
            self.lines.push(Vec::new());
        }
    }

    fn erase_line_from_cursor(&mut self) {
        if let Some(row) = self.lines.get_mut(self.cursor_row) {
            if self.cursor_col < row.len() {
                row.truncate(self.cursor_col);
            }
        }
    }

    fn finish(mut self) -> Vec<Vec<CliStyledChar>> {
        while matches!(self.lines.last(), Some(last) if last.is_empty()) {
            self.lines.pop();
        }
        if self.lines.is_empty() {
            vec![Vec::new()]
        } else {
            self.lines
        }
    }
}

fn render_cli_frame(
    line: &str,
    frame: usize,
    row: usize,
    total_rows: usize,
    total_frames: usize,
    effect: EffectKind,
) -> String {
    match effect {
        EffectKind::Plain => line.to_string(),
        EffectKind::Wipe => cli_wipe_frame(line, frame, row, total_rows, total_frames),
        EffectKind::Sweep => cli_sweep_frame(line, frame, total_frames),
        EffectKind::Fade => cli_fade_frame(line, frame, total_frames),
        EffectKind::Coalesce => cli_coalesce_frame(line, frame, row, total_frames),
        EffectKind::Glitch => cli_glitch_frame(line, frame, row, total_frames),
        EffectKind::Matrix => cli_matrix_frame(line, frame, row, total_rows, total_frames),
        EffectKind::Scanline => cli_scanline_frame(line, frame, row, total_rows, total_frames),
        EffectKind::Scatter => cli_scatter_frame(line, frame, row, total_frames),
    }
}

fn cli_wipe_frame(
    line: &str,
    frame: usize,
    row: usize,
    total_rows: usize,
    total_frames: usize,
) -> String {
    let progress = frame as f32 / total_frames.max(1) as f32;
    let chars: Vec<char> = line.chars().collect();
    let row_weight = if total_rows <= 1 {
        0.0
    } else {
        row as f32 / (total_rows - 1) as f32
    };
    let len = chars.len().max(1);

    chars
        .iter()
        .enumerate()
        .map(|(col, &ch)| {
            let col_weight = col as f32 / len as f32;
            let threshold = row_weight * 0.55 + col_weight * 0.45;
            if progress >= threshold {
                ch
            } else {
                ' '
            }
        })
        .collect()
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

fn cli_glitch_frame(line: &str, frame: usize, row: usize, total_frames: usize) -> String {
    let progress = frame as f32 / total_frames.max(1) as f32;
    let instability = (1.0 - progress).clamp(0.0, 1.0);

    line.chars()
        .enumerate()
        .map(|(col, ch)| {
            if ch.is_whitespace() {
                return ch;
            }
            let gate = pseudo_random_01(row as u64 + frame as u64, col as u64);
            if gate < instability * 0.75 {
                matrix_noise_symbol(row, col, frame)
            } else {
                ch
            }
        })
        .collect()
}

fn cli_matrix_frame(
    line: &str,
    frame: usize,
    row: usize,
    total_rows: usize,
    total_frames: usize,
) -> String {
    let progress = frame as f32 / total_frames.max(1) as f32;
    let chars: Vec<char> = line.chars().collect();

    chars
        .iter()
        .enumerate()
        .map(|(col, &ch)| {
            if ch.is_whitespace() {
                return ch;
            }

            let state = matrix_cell_state(row, col, total_rows.max(1), progress);
            if state >= 1.0 {
                ch
            } else {
                matrix_noise_symbol(row, col, frame)
            }
        })
        .collect()
}

fn cli_scanline_frame(
    line: &str,
    frame: usize,
    row: usize,
    total_rows: usize,
    total_frames: usize,
) -> String {
    let progress = frame as f32 / total_frames.max(1) as f32;
    let band = progress * (total_rows.max(1) as f32 + 1.5);
    let distance = band - row as f32;

    if distance > 1.5 {
        return line.to_string();
    }
    if distance < -0.5 {
        return " ".repeat(line.chars().count());
    }

    line.chars()
        .enumerate()
        .map(|(col, ch)| {
            if ch.is_whitespace() {
                ch
            } else {
                matrix_noise_symbol(row, col, frame)
            }
        })
        .collect()
}

fn cli_scatter_frame(line: &str, frame: usize, row: usize, total_frames: usize) -> String {
    let progress = frame as f32 / total_frames.max(1) as f32;
    let eased = 1.0 - (1.0 - progress).powi(2);

    line.chars()
        .enumerate()
        .map(|(col, ch)| {
            if ch.is_whitespace() {
                return ch;
            }

            let threshold = pseudo_random_01(row as u64 + 97, col as u64 + 193);
            if eased >= threshold {
                ch
            } else if eased + 0.12 >= threshold {
                CLI_SCRAMBLE[(row * 23 + col * 31 + frame * 7) % CLI_SCRAMBLE.len()]
            } else {
                ' '
            }
        })
        .collect()
}

fn matrix_noise_symbol(row: usize, col: usize, frame: usize) -> char {
    const SYMBOLS: [char; 8] = ['0', '1', '|', ':', '.', '+', '*', '#'];
    SYMBOLS[(row * 19 + col * 11 + frame * 5) % SYMBOLS.len()]
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

fn pseudo_random_01(a: u64, b: u64) -> f32 {
    let mut x = a.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ b.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    (x % 10_000) as f32 / 10_000.0
}

fn capture_command_text(command: &[OsString], stdout_is_tty: bool) -> Result<(String, i32)> {
    if stdout_is_tty {
        return capture_command_text_via_pty(command);
    }

    let output = std::process::Command::new(&command[0])
        .args(&command[1..])
        .output()?;
    let mut text = String::new();
    text.push_str(&String::from_utf8_lossy(&output.stdout));
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok((text, output.status.code().unwrap_or(1)))
}

fn capture_command_text_via_pty(command: &[OsString]) -> Result<(String, i32)> {
    let (cols, rows) = size().unwrap_or((80, 24));
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut cmd = CommandBuilder::new(&command[0]);
    cmd.args(&command[1..]);
    if let Ok(cwd) = std::env::current_dir() {
        cmd.cwd(cwd);
    }
    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let mut buf = [0u8; 8192];
    let mut out = Vec::new();
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
            Err(_) => break,
        }
    }

    let status = child.wait()?;
    Ok((
        String::from_utf8_lossy(&out).into_owned(),
        status.exit_code() as i32,
    ))
}

fn read_stdin_text() -> Result<(String, i32)> {
    let mut text = String::new();
    io::stdin().read_to_string(&mut text)?;
    Ok((text, 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        model::{Backend, Feature, Runtime, Theme},
        theme::builtin_theme,
    };
    use std::collections::{BTreeSet, HashMap};

    #[test]
    fn split_ansi_plain_lines_preserve_tail_does_not_add_extra_blank_line_for_trailing_newline() {
        let lines = split_ansi_plain_lines_preserve_tail("a\nb\n");

        assert_eq!(lines, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn cli_coalesce_frame_preserves_whitespace() {
        let rendered = cli_coalesce_frame("a b", 0, 0, 10);
        let chars: Vec<char> = rendered.chars().collect();

        assert_eq!(chars[1], ' ');
    }

    #[test]
    fn cli_matrix_frame_preserves_whitespace() {
        let rendered = cli_matrix_frame("a b", 0, 0, 3, 10);
        let chars: Vec<char> = rendered.chars().collect();

        assert_eq!(chars[1], ' ');
    }

    #[test]
    fn cli_glitch_frame_preserves_whitespace() {
        let rendered = cli_glitch_frame("a b", 0, 0, 10);
        let chars: Vec<char> = rendered.chars().collect();

        assert_eq!(chars[1], ' ');
    }

    #[test]
    fn cli_scanline_frame_preserves_whitespace() {
        let rendered = cli_scanline_frame("a b", 0, 0, 3, 10);
        let chars: Vec<char> = rendered.chars().collect();

        assert_eq!(chars[1], ' ');
    }

    #[test]
    fn cli_scatter_frame_preserves_whitespace() {
        let rendered = cli_scatter_frame("a b", 0, 0, 10);
        let chars: Vec<char> = rendered.chars().collect();

        assert_eq!(chars[1], ' ');
    }

    #[test]
    fn split_ansi_lines_preserve_tail_keeps_sgr_styles() {
        let lines = split_ansi_lines_preserve_tail("\x1b[31mred\x1b[0m\n");

        assert_eq!(lines.len(), 1);
        assert_eq!(plain_line_from_styled(&lines[0]), "red");
        assert_eq!(lines[0][0].style.fg, CliDisplayColor::Indexed(1));
    }

    #[test]
    fn split_ansi_lines_preserve_tail_applies_cursor_forward_padding() {
        let lines = split_ansi_lines_preserve_tail("left\x1b[4Cright");

        assert_eq!(plain_line_from_styled(&lines[0]), "left    right");
    }

    #[test]
    fn split_ansi_plain_lines_preserve_tail_applies_cursor_forward_padding() {
        let lines = split_ansi_plain_lines_preserve_tail("left\x1b[4Cright");

        assert_eq!(lines, vec!["left    right".to_string()]);
    }

    #[test]
    fn split_ansi_lines_preserve_tail_does_not_paint_cursor_forward_gap_with_current_style() {
        let lines = split_ansi_lines_preserve_tail("\x1b[30m\x1b[40m\x1b[4Cxxx");

        assert_eq!(plain_line_from_styled(&lines[0]), "    xxx");
        assert_eq!(lines[0][0].style, CliStyle::default());
        assert_eq!(lines[0][3].style, CliStyle::default());
        assert_eq!(lines[0][4].style.bg, CliDisplayColor::Indexed(0));
    }

    #[test]
    fn split_ansi_plain_lines_preserve_tail_treats_bare_carriage_return_as_newline() {
        let lines = split_ansi_plain_lines_preserve_tail("a\rb\r");

        assert_eq!(lines, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn split_ansi_lines_preserve_tail_handles_cursor_layout_with_cr_terminated_lines() {
        let lines = split_ansi_lines_preserve_tail("line1\rline2\r\x1b[2A\x1b[5Cinfo\r");

        assert_eq!(plain_line_from_styled(&lines[0]), "line1info");
        assert_eq!(plain_line_from_styled(&lines[1]), "line2");
    }

    #[test]
    fn cli_preserves_source_ansi_when_theme_is_default_and_no_cli_color_override() {
        let rt = runtime_for_cli_ansi(builtin_theme("default"));

        assert!(cli_preserves_source_ansi(&rt));
    }

    #[test]
    fn cli_does_not_preserve_source_ansi_when_theme_forces_recolor() {
        let rt = runtime_for_cli_ansi(builtin_theme("matrix-green"));

        assert!(!cli_preserves_source_ansi(&rt));
    }

    #[test]
    fn fade_rgb_for_progress_darkens_before_settling() {
        let mut rt = runtime_for_cli_ansi(builtin_theme("default"));
        rt.animation_color_fade = true;
        let faded = fade_rgb_for_progress(&rt, Rgb(200, 100, 50), 0.0);
        let settled = fade_rgb_for_progress(&rt, Rgb(200, 100, 50), 1.0);

        assert_eq!(faded, Rgb(50, 25, 13));
        assert_eq!(settled, Rgb(200, 100, 50));
    }

    #[test]
    fn fade_rgb_for_progress_is_disabled_when_flag_is_false() {
        let mut rt = runtime_for_cli_ansi(builtin_theme("default"));
        rt.animation_color_fade = false;

        assert_eq!(
            fade_rgb_for_progress(&rt, Rgb(200, 100, 50), 0.0),
            Rgb(200, 100, 50)
        );
    }

    fn runtime_for_cli_ansi(theme: Theme) -> Runtime {
        Runtime {
            backend: Backend::Cli,
            features: BTreeSet::from([Feature::InlineAnimation]),
            effect: EffectKind::Coalesce,
            command: vec![],
            theme,
            keymap: HashMap::new(),
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
            cli_animation_color: None,
            cli_settled_color: None,
            cli_gradient_start: None,
            cli_gradient_end: None,
            no_theme_after_reveal: false,
            highlight_rules: Vec::new(),
        }
    }
}
