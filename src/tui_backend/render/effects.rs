use super::screen::{theme_preserves_terminal_colors, DisplayColor, LiveRenderScene, StyledCell};
use crate::{
    model::{EffectKind, Rgb},
    support::sleep_frame,
    theme::{darken, lerp},
};
use anyhow::Result;
use crossterm::{
    cursor::{Hide, Show},
    execute,
    terminal::{Clear, ClearType, ScrollDown, ScrollUp},
};
use std::{
    io::{self, Write},
    thread,
    time::Duration,
};

#[derive(Clone, Copy)]
pub(crate) struct RevealTuning {
    pub(crate) frames: usize,
    pub(crate) duration_ms: u64,
    pub(crate) effect: EffectKind,
    pub(crate) color_fade: bool,
    pub(crate) darken_factor: f32,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct LiveRenderFrame {
    pub(crate) scroll_hint: Option<super::screen::ScrollHint>,
    pub(crate) effect: EffectKind,
    pub(crate) frame: usize,
    pub(crate) ratio: f32,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct LiveRenderTuning {
    pub(crate) duration_ms: u64,
    pub(crate) color_fade: bool,
    pub(crate) darken_factor: f32,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RedrawMasks<'a> {
    pub(crate) redraw: &'a [Vec<bool>],
    pub(crate) highlight: &'a [Vec<bool>],
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CellAnimCtx {
    pub(crate) row: usize,
    pub(crate) col: usize,
    pub(crate) total_rows: usize,
    pub(crate) frame: usize,
    pub(crate) ratio: f32,
    pub(crate) color_fade: bool,
    pub(crate) effect: EffectKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RenderStyle {
    fg: DisplayColor,
    bg: DisplayColor,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
}

pub(crate) fn animate_styled_reveal(
    cells: &[Vec<StyledCell>],
    highlight_colors: Option<&[Vec<Option<Rgb>>]>,
    rows: u16,
    cols: u16,
    tuning: RevealTuning,
    enter_guard: impl Fn(bool) -> Result<super::super::pty::TerminalGuard>,
) -> Result<()> {
    let _guard = enter_guard(true)?;
    animate_styled_reveal_in_place(cells, highlight_colors, rows, cols, tuning)
}

pub(crate) fn animate_styled_reveal_in_place(
    cells: &[Vec<StyledCell>],
    highlight_colors: Option<&[Vec<Option<Rgb>>]>,
    rows: u16,
    cols: u16,
    tuning: RevealTuning,
) -> Result<()> {
    let mut out = io::stdout();
    execute!(out, Clear(ClearType::All), crossterm::cursor::MoveTo(0, 0))?;
    let preserve_terminal = cells.iter().flatten().all(|cell| {
        matches!(cell.fg, DisplayColor::Default | DisplayColor::Indexed(_))
            && matches!(cell.bg, DisplayColor::Default | DisplayColor::Indexed(_))
    });
    for frame in 0..tuning.frames {
        let ratio = frame as f32 / tuning.frames.saturating_sub(1).max(1) as f32;
        execute!(out, crossterm::cursor::MoveTo(0, 0))?;
        for (r, row) in cells.iter().enumerate().take(rows as usize) {
            let mut style_state = None;
            for (c, cell) in row.iter().enumerate().take(cols as usize) {
                if cell.wide_continuation {
                    continue;
                }
                let text =
                    reveal_text_for_cell(cell, r, c, rows as usize, frame, ratio, tuning.effect);
                let mut animated = cell.clone();
                if tuning.color_fade && ratio < 1.0 {
                    if preserve_terminal {
                        animated.dim = ratio < 0.92;
                    } else {
                        animated.fg = animated_faded_color(
                            animated.fg,
                            ratio,
                            tuning.effect,
                            tuning.darken_factor,
                        );
                    }
                }
                if let Some(rgb) = highlight_colors
                    .and_then(|rows| rows.get(r))
                    .and_then(|row_colors| row_colors.get(c))
                    .copied()
                    .flatten()
                {
                    animated.bg = DisplayColor::Rgb(rgb);
                }
                write_cell(&mut out, &animated, text, &mut style_state)?;
            }
            write!(out, "\x1b[0m")?;
            if r + 1 < rows as usize {
                write!(out, "\r\n")?;
            }
        }
        out.flush()?;
        sleep_frame(tuning.duration_ms, tuning.frames);
    }
    execute!(out, crossterm::cursor::MoveTo(0, 0))?;
    for (r, row) in cells.iter().enumerate() {
        let mut style_state = None;
        for (c, cell) in row.iter().enumerate() {
            if !cell.wide_continuation {
                let mut final_cell = cell.clone();
                if let Some(rgb) = highlight_colors
                    .and_then(|rows| rows.get(r))
                    .and_then(|row_colors| row_colors.get(c))
                    .copied()
                    .flatten()
                {
                    final_cell.bg = DisplayColor::Rgb(rgb);
                }
                write_cell(&mut out, &final_cell, &final_cell.text, &mut style_state)?;
            }
        }
        write!(out, "\x1b[0m")?;
        if r + 1 < rows as usize {
            write!(out, "\r\n")?;
        }
    }
    out.flush()?;
    Ok(())
}

pub(crate) fn reveal_text_for_cell(
    cell: &StyledCell,
    row: usize,
    col: usize,
    total_rows: usize,
    frame: usize,
    ratio: f32,
    effect: EffectKind,
) -> &str {
    match effect {
        EffectKind::Plain => &cell.text,
        EffectKind::Wipe => {
            let row_weight = if total_rows <= 1 {
                0.0
            } else {
                row as f32 / (total_rows - 1) as f32
            };
            let threshold = row_weight * 0.55 + (col as f32 / (col + 8) as f32) * 0.45;
            if ratio >= threshold.clamp(0.0, 1.0) {
                &cell.text
            } else {
                " "
            }
        }
        EffectKind::Sweep => {
            let width_gate = (col as f32 + 1.0) / ((col + 8) as f32);
            if ratio >= width_gate.clamp(0.0, 1.0) {
                &cell.text
            } else {
                " "
            }
        }
        EffectKind::Fade => &cell.text,
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
        EffectKind::Glitch => {
            let instability = (1.0 - ratio).clamp(0.0, 1.0);
            if cell.text.trim().is_empty() {
                " "
            } else if pseudo_random_01(row as u64 + frame as u64 * 3, col as u64 + frame as u64 * 7)
                < instability * 0.75
            {
                reveal_noise_symbol(row, col, frame)
            } else {
                &cell.text
            }
        }
        EffectKind::Matrix => {
            let state = matrix_cell_state(row, col, total_rows.max(1), ratio);
            if state >= 1.0 {
                &cell.text
            } else if cell.text.trim().is_empty() || state < 0.0 {
                " "
            } else {
                reveal_matrix_symbol(row, col, frame)
            }
        }
        EffectKind::Scanline => {
            let band = ratio * (total_rows.max(1) as f32 + 1.5);
            let distance = band - row as f32;
            if distance > 1.5 {
                &cell.text
            } else if distance < -0.5 || cell.text.trim().is_empty() {
                " "
            } else {
                reveal_noise_symbol(row, col, frame)
            }
        }
        EffectKind::Scatter => {
            let threshold = pseudo_random_01(row as u64 + 97, col as u64 + 193);
            if ratio >= threshold {
                &cell.text
            } else if ratio + 0.12 >= threshold && !cell.text.trim().is_empty() {
                reveal_noise_symbol(row, col, frame)
            } else {
                " "
            }
        }
    }
}

pub(crate) fn reveal_noise_symbol(row: usize, col: usize, frame: usize) -> &'static str {
    const SYMBOLS: [&str; 8] = [".", ":", "+", "*", "#", "%", "@", "="];
    SYMBOLS[(row + col + frame) % SYMBOLS.len()]
}

pub(crate) fn draw_live_screen(
    out: &mut io::Stdout,
    scene: LiveRenderScene<'_>,
    tuning: LiveRenderTuning,
    animation: LiveRenderFrame,
    masks: RedrawMasks<'_>,
) -> io::Result<()> {
    let preserve_terminal = theme_preserves_terminal_colors(Some(scene.theme));
    match animation.scroll_hint {
        Some(super::screen::ScrollHint::Up(lines)) => execute!(out, ScrollUp(lines))?,
        Some(super::screen::ScrollHint::Down(lines)) => execute!(out, ScrollDown(lines))?,
        None => {}
    }
    let rows = scene.cells.len();
    for r in 0..rows {
        if !super::screen::row_has_changes(masks.redraw, r) {
            continue;
        }

        execute!(out, crossterm::cursor::MoveTo(0, r as u16))?;
        let mut style_state = None;
        for c in 0..scene.cells[r].len() {
            let cell = &scene.cells[r][c];
            if cell.wide_continuation {
                continue;
            }
            let highlight_bg = scene
                .highlight_colors
                .and_then(|rows| rows.get(r))
                .and_then(|row_colors| row_colors.get(c))
                .copied()
                .flatten();
            if masks.highlight[r][c] && !cell.text.trim().is_empty() {
                let mut highlighted = cell.clone();
                if !preserve_terminal {
                    highlighted.bold = true;
                }
                if let Some(rgb) = highlight_bg {
                    highlighted.bg = DisplayColor::Rgb(rgb);
                }
                if tuning.color_fade && !preserve_terminal && animation.ratio < 1.0 {
                    highlighted.fg = animated_faded_color(
                        highlighted.fg,
                        animation.ratio,
                        animation.effect,
                        tuning.darken_factor,
                    );
                }
                let text = live_render_text_for_cell(
                    cell,
                    CellAnimCtx {
                        row: r,
                        col: c,
                        total_rows: scene.cells.len(),
                        frame: animation.frame,
                        ratio: animation.ratio,
                        color_fade: tuning.color_fade,
                        effect: animation.effect,
                    },
                );
                write_cell(out, &highlighted, &text, &mut style_state)?;
            } else {
                let mut stable = cell.clone();
                if let Some(rgb) = highlight_bg {
                    stable.bg = DisplayColor::Rgb(rgb);
                }
                write_cell(out, &stable, &stable.text, &mut style_state)?;
            }
        }
        write!(out, "\x1b[0m")?;
    }
    execute!(
        out,
        crossterm::cursor::MoveTo(scene.state.cursor_col, scene.state.cursor_row)
    )?;
    if scene.state.cursor_visible {
        execute!(out, Show)?;
    } else {
        execute!(out, Hide)?;
    }
    out.flush()
}

pub(crate) fn flash_highlight_markers(
    out: &mut io::Stdout,
    scene: LiveRenderScene<'_>,
) -> io::Result<()> {
    let spans = highlight_spans(scene);
    if spans.is_empty() {
        return Ok(());
    }

    let phases = [
        (Rgb(255, 240, 120), Rgb(20, 12, 0)),
        (Rgb(255, 120, 220), Rgb(35, 0, 30)),
    ];

    for (bg, fg) in phases {
        for span in &spans {
            draw_highlight_span_flash(out, scene, span, bg, fg)?;
        }
        out.flush()?;
        thread::sleep(Duration::from_millis(80));
    }

    let mut restored_rows = spans.iter().map(|span| span.row).collect::<Vec<_>>();
    restored_rows.sort_unstable();
    restored_rows.dedup();
    for row in restored_rows {
        draw_live_row(out, scene, row)?;
    }
    execute!(
        out,
        crossterm::cursor::MoveTo(scene.state.cursor_col, scene.state.cursor_row)
    )?;
    if scene.state.cursor_visible {
        execute!(out, Show)?;
    } else {
        execute!(out, Hide)?;
    }
    out.flush()
}

pub(crate) fn animated_faded_color(
    color: DisplayColor,
    ratio: f32,
    effect: EffectKind,
    darken_factor: f32,
) -> DisplayColor {
    let target = match color {
        DisplayColor::Rgb(rgb) => rgb,
        _ => return color,
    };
    let effect_bias = match effect {
        EffectKind::Glitch => 0.15,
        EffectKind::Matrix => -0.08,
        EffectKind::Scanline => 0.05,
        _ => 0.0,
    };
    let start = darken(target, (darken_factor + effect_bias).clamp(0.0, 1.0));
    let k = ratio.clamp(0.0, 1.0);
    DisplayColor::Rgb(Rgb(
        lerp(start.0, target.0, k),
        lerp(start.1, target.1, k),
        lerp(start.2, target.2, k),
    ))
}

pub(crate) fn live_render_text_for_cell(cell: &StyledCell, ctx: CellAnimCtx) -> String {
    if cell.text.trim().is_empty() {
        return cell.text.clone();
    }

    match ctx.effect {
        EffectKind::Plain => cell.text.clone(),
        EffectKind::Wipe => {
            let row_weight = if ctx.total_rows <= 1 {
                0.0
            } else {
                ctx.row as f32 / (ctx.total_rows - 1) as f32
            };
            let threshold = row_weight * 0.55 + (ctx.col as f32 / (ctx.col + 8) as f32) * 0.45;
            if ctx.ratio >= threshold.clamp(0.0, 1.0) {
                cell.text.clone()
            } else {
                " ".to_string()
            }
        }
        EffectKind::Sweep => {
            let width_gate = (ctx.col as f32 + 1.0) / ((ctx.col + 8) as f32);
            let gate = width_gate.clamp(0.0, 1.0);
            if ctx.ratio >= gate || (ctx.color_fade && ctx.ratio >= gate * 0.6) {
                cell.text.clone()
            } else {
                reveal_noise_symbol(ctx.row, ctx.col, ctx.frame).to_string()
            }
        }
        EffectKind::Fade => {
            if ctx.ratio >= 0.6 || (ctx.color_fade && ctx.ratio >= 0.25) {
                cell.text.clone()
            } else {
                reveal_noise_symbol(ctx.row, ctx.col, ctx.frame).to_string()
            }
        }
        EffectKind::Coalesce => {
            let appear_at = ((ctx.row * 17 + ctx.col * 7) % 100) as f32 / 100.0;
            let reveal = ((ctx.ratio - appear_at * 0.55) / 0.45).clamp(0.0, 1.0);
            if reveal >= 0.98 || (ctx.color_fade && reveal >= 0.42) {
                cell.text.clone()
            } else {
                reveal_noise_symbol(ctx.row, ctx.col, ctx.frame).to_string()
            }
        }
        EffectKind::Glitch => {
            let instability = (1.0 - ctx.ratio).clamp(0.0, 1.0);
            if pseudo_random_01(
                ctx.row as u64 + ctx.frame as u64 * 3,
                ctx.col as u64 + ctx.frame as u64 * 7,
            ) < instability * 0.75
            {
                reveal_noise_symbol(ctx.row, ctx.col, ctx.frame).to_string()
            } else {
                cell.text.clone()
            }
        }
        EffectKind::Matrix => {
            let state = matrix_cell_state(ctx.row, ctx.col, ctx.total_rows.max(1), ctx.ratio);
            if state >= 1.0 || (ctx.color_fade && state >= 0.0) {
                cell.text.clone()
            } else {
                reveal_matrix_symbol(ctx.row, ctx.col, ctx.frame).to_string()
            }
        }
        EffectKind::Scanline => {
            let band = ctx.ratio * (ctx.total_rows.max(1) as f32 + 1.5);
            let distance = band - ctx.row as f32;
            if distance > 1.5 || (ctx.color_fade && distance >= 0.0) {
                cell.text.clone()
            } else {
                reveal_noise_symbol(ctx.row, ctx.col, ctx.frame).to_string()
            }
        }
        EffectKind::Scatter => {
            let threshold = pseudo_random_01(ctx.row as u64 + 97, ctx.col as u64 + 193);
            if ctx.ratio >= threshold || (ctx.color_fade && ctx.ratio + 0.08 >= threshold) {
                cell.text.clone()
            } else if ctx.ratio + 0.16 >= threshold {
                reveal_noise_symbol(ctx.row, ctx.col, ctx.frame).to_string()
            } else {
                " ".to_string()
            }
        }
    }
}

pub(crate) fn coalesce_text(text: &str, ratio: f32, effect: EffectKind) -> String {
    match effect {
        EffectKind::Plain => text.to_string(),
        EffectKind::Wipe => {
            let count = text.chars().count().max(1);
            text.chars()
                .enumerate()
                .map(|(idx, ch)| {
                    let threshold = idx as f32 / count as f32;
                    if ratio >= threshold {
                        ch
                    } else {
                        ' '
                    }
                })
                .collect()
        }
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
            let symbols = ['.', ':', '+', '*', '#', '%', '@'];
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
        EffectKind::Glitch => text
            .chars()
            .enumerate()
            .map(|(idx, ch)| {
                if ch.is_whitespace() {
                    return ch;
                }
                if pseudo_random_01(idx as u64, (ratio * 1000.0) as u64) < (1.0 - ratio) * 0.75 {
                    ['.', ':', '+', '*', '#', '%', '@'][(idx + (ratio * 100.0) as usize) % 7]
                } else {
                    ch
                }
            })
            .collect(),
        EffectKind::Matrix => text
            .chars()
            .enumerate()
            .map(|(idx, ch)| {
                if ch.is_whitespace() {
                    return ch;
                }
                let state = matrix_cell_state(0, idx, 1, ratio);
                if state >= 1.0 {
                    ch
                } else {
                    ['0', '1', '|', ':', '.', '+', '*', '#'][(idx + (ratio * 100.0) as usize) % 8]
                }
            })
            .collect(),
        EffectKind::Scanline => {
            if ratio >= 0.85 {
                text.to_string()
            } else {
                text.chars()
                    .enumerate()
                    .map(|(idx, ch)| {
                        if ch.is_whitespace() {
                            ch
                        } else {
                            ['.', ':', '+', '*', '#', '%', '@']
                                [(idx + (ratio * 100.0) as usize) % 7]
                        }
                    })
                    .collect()
            }
        }
        EffectKind::Scatter => text
            .chars()
            .enumerate()
            .map(|(idx, ch)| {
                if ch.is_whitespace() {
                    return ch;
                }
                let threshold = pseudo_random_01(idx as u64 + 97, text.len() as u64 + 193);
                if ratio >= threshold {
                    ch
                } else if ratio + 0.12 >= threshold {
                    ['.', ':', '+', '*', '#', '%', '@'][(idx + (ratio * 100.0) as usize) % 7]
                } else {
                    ' '
                }
            })
            .collect(),
    }
}

pub(crate) fn live_render_effect_ratios(effect: EffectKind) -> &'static [f32] {
    match effect {
        EffectKind::Plain => &[1.0],
        EffectKind::Fade => &[0.35, 1.0],
        EffectKind::Wipe => &[0.2, 0.55, 1.0],
        EffectKind::Sweep => &[0.25, 0.65, 1.0],
        EffectKind::Coalesce => &[0.12, 0.38, 0.72, 1.0],
        EffectKind::Glitch => &[0.08, 0.16, 0.32, 0.55, 1.0],
        EffectKind::Matrix => &[0.08, 0.24, 0.45, 0.72, 1.0],
        EffectKind::Scanline => &[0.12, 0.35, 0.68, 1.0],
        EffectKind::Scatter => &[0.1, 0.28, 0.58, 1.0],
    }
}

pub(crate) fn live_render_per_frame_ms(total_ms: u64, effect: EffectKind) -> u64 {
    let ratios = live_render_effect_ratios(effect);
    (total_ms.max(1) / ratios.len().max(1) as u64).max(1)
}

fn reveal_matrix_symbol(row: usize, col: usize, frame: usize) -> &'static str {
    const SYMBOLS: [&str; 8] = ["0", "1", "|", ":", ".", "+", "*", "#"];
    SYMBOLS[(row * 19 + col * 11 + frame * 5) % SYMBOLS.len()]
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
        write!(out, "\x1b[0m")?;
        write_display_color(out, true, next_style.fg)?;
        write_display_color(out, false, next_style.bg)?;
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

fn draw_live_row(out: &mut io::Stdout, scene: LiveRenderScene<'_>, row: usize) -> io::Result<()> {
    let Some(cells) = scene.cells.get(row) else {
        return Ok(());
    };

    execute!(out, crossterm::cursor::MoveTo(0, row as u16))?;
    let mut style_state = None;
    for (col, cell) in cells.iter().enumerate() {
        if cell.wide_continuation {
            continue;
        }
        let mut stable = cell.clone();
        if let Some(rgb) = scene
            .highlight_colors
            .and_then(|rows| rows.get(row))
            .and_then(|row_colors| row_colors.get(col))
            .copied()
            .flatten()
        {
            stable.bg = DisplayColor::Rgb(rgb);
        }
        write_cell(out, &stable, &stable.text, &mut style_state)?;
    }
    write!(out, "\x1b[0m")
}

#[derive(Clone, Copy)]
struct HighlightSpan {
    row: usize,
    start: usize,
    end: usize,
    left_marker: Option<usize>,
    right_marker: Option<usize>,
}

fn highlight_spans(scene: LiveRenderScene<'_>) -> Vec<HighlightSpan> {
    let mut spans = Vec::new();
    let Some(colors) = scene.highlight_colors else {
        return spans;
    };

    for (row_idx, row_colors) in colors.iter().enumerate() {
        let mut col = 0;
        while col < row_colors.len() {
            if row_colors[col].is_none() {
                col += 1;
                continue;
            }

            let start = col;
            while col < row_colors.len() && row_colors[col].is_some() {
                col += 1;
            }
            let end = col.saturating_sub(1);

            spans.push(HighlightSpan {
                row: row_idx,
                start,
                end,
                left_marker: marker_slot(scene.cells, row_idx, start, true),
                right_marker: marker_slot(scene.cells, row_idx, end, false),
            });
        }
    }

    spans
}

fn draw_highlight_span_flash(
    out: &mut io::Stdout,
    scene: LiveRenderScene<'_>,
    span: &HighlightSpan,
    bg: Rgb,
    fg: Rgb,
) -> io::Result<()> {
    let Some(row_cells) = scene.cells.get(span.row) else {
        return Ok(());
    };

    let left = span.left_marker.unwrap_or(span.start);
    let right = span.right_marker.unwrap_or(span.end);
    execute!(out, crossterm::cursor::MoveTo(left as u16, span.row as u16))?;

    let mut style_state = None;
    for col in left..=right {
        let Some(cell) = row_cells.get(col) else {
            continue;
        };
        if cell.wide_continuation {
            continue;
        }

        let text = if span.left_marker == Some(col) || span.right_marker == Some(col) {
            "✦"
        } else {
            cell.text.as_str()
        };
        let mut flashed = cell.clone();
        flashed.bold = true;
        flashed.fg = DisplayColor::Rgb(fg);
        flashed.bg = DisplayColor::Rgb(bg);
        write_cell(out, &flashed, text, &mut style_state)?;
    }
    write!(out, "\x1b[0m")
}

fn marker_slot(
    cells: &[Vec<StyledCell>],
    row: usize,
    edge_col: usize,
    left_side: bool,
) -> Option<usize> {
    let row_cells = cells.get(row)?;
    let candidate = if left_side {
        edge_col.checked_sub(1)?
    } else {
        edge_col.checked_add(1)?
    };
    let cell = row_cells.get(candidate)?;
    (!cell.wide_continuation).then_some(candidate)
}

fn write_display_color(out: &mut io::Stdout, fg: bool, color: DisplayColor) -> io::Result<()> {
    match color {
        DisplayColor::Default => {
            if fg {
                write!(out, "\x1b[39m")
            } else {
                write!(out, "\x1b[49m")
            }
        }
        DisplayColor::Indexed(idx) => {
            let code = if fg {
                if idx < 8 {
                    30 + idx
                } else if idx < 16 {
                    90 + (idx - 8)
                } else {
                    return write!(out, "\x1b[38;5;{}m", idx);
                }
            } else if idx < 8 {
                40 + idx
            } else if idx < 16 {
                100 + (idx - 8)
            } else {
                return write!(out, "\x1b[48;5;{}m", idx);
            };
            write!(out, "\x1b[{}m", code)
        }
        DisplayColor::Rgb(rgb) => {
            if fg {
                write!(out, "\x1b[38;2;{};{};{}m", rgb.0, rgb.1, rgb.2)
            } else {
                write!(out, "\x1b[48;2;{};{};{}m", rgb.0, rgb.1, rgb.2)
            }
        }
    }
}
