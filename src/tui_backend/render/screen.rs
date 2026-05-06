use crate::{
    model::{Rgb, Theme},
    transform::transform_line,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StyledCell {
    pub(crate) text: String,
    pub(crate) fg: DisplayColor,
    pub(crate) bg: DisplayColor,
    pub(crate) bold: bool,
    pub(crate) dim: bool,
    pub(crate) italic: bool,
    pub(crate) underline: bool,
    pub(crate) inverse: bool,
    pub(crate) wide_continuation: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DisplayColor {
    Default,
    Indexed(u8),
    Rgb(Rgb),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalState {
    pub(crate) cursor_row: u16,
    pub(crate) cursor_col: u16,
    pub(crate) cursor_visible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScrollHint {
    Up(u16),
    Down(u16),
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct LiveRenderScene<'a> {
    pub(crate) cells: &'a [Vec<StyledCell>],
    pub(crate) state: &'a TerminalState,
    pub(crate) highlight_colors: Option<&'a [Vec<Option<Rgb>>]>,
    pub(crate) theme: &'a Theme,
}

pub(crate) fn collect_terminal_state(
    screen: &vt100::Screen,
    rows: u16,
    cols: u16,
) -> TerminalState {
    let (cursor_row, cursor_col) = screen.cursor_position();
    TerminalState {
        cursor_row: cursor_row.min(rows.saturating_sub(1)),
        cursor_col: cursor_col.min(cols.saturating_sub(1)),
        cursor_visible: !screen.hide_cursor(),
    }
}

pub(crate) fn collect_screen(
    screen: &vt100::Screen,
    rows: u16,
    cols: u16,
    theme: Option<&Theme>,
) -> Vec<Vec<StyledCell>> {
    let mut result = Vec::new();
    for row in 0..rows {
        let mut out_row = Vec::new();
        for col in 0..cols {
            if let Some(cell) = screen.cell(row, col) {
                let fg = display_vt_color(theme, cell.fgcolor(), true);
                let bg = display_vt_color(theme, cell.bgcolor(), false);
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

pub(crate) fn screen_lines(cells: &[Vec<StyledCell>]) -> Vec<String> {
    cells
        .iter()
        .map(|row| {
            row.iter()
                .filter(|cell| !cell.wide_continuation)
                .map(|cell| cell.text.as_str())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

pub(crate) fn apply_transforms_to_cells(
    cells: &mut [Vec<StyledCell>],
    rules: &[crate::model::OutputTransformRule],
) {
    if rules.is_empty() {
        return;
    }

    for row in cells.iter_mut() {
        let visible_indexes = row
            .iter()
            .enumerate()
            .filter_map(|(idx, cell)| (!cell.wide_continuation).then_some(idx))
            .collect::<Vec<_>>();
        let plain = visible_indexes
            .iter()
            .map(|&idx| row[idx].text.as_str())
            .collect::<String>();
        let transformed = transform_line(&plain, rules);
        for (cell_idx, ch) in visible_indexes.into_iter().zip(transformed.chars()) {
            row[cell_idx].text = ch.to_string();
        }
    }
}

pub(crate) fn screen_to_svg(
    cells: &[Vec<StyledCell>],
    highlight_colors: Option<&[Vec<Option<Rgb>>]>,
) -> String {
    let cell_width = 9usize;
    let cell_height = 18usize;
    let width = cells.first().map(|row| row.len()).unwrap_or(0) * cell_width;
    let height = cells.len() * cell_height;
    let mut svg = String::new();
    svg.push_str(&format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">"#
    ));
    svg.push_str(r##"<rect width="100%" height="100%" fill="#101010"/>"##);
    svg.push_str(r#"<g font-family="monospace" font-size="14" dominant-baseline="hanging">"#);
    for (row_idx, row) in cells.iter().enumerate() {
        for (col_idx, cell) in row.iter().enumerate() {
            if cell.wide_continuation {
                continue;
            }
            let x = col_idx * cell_width;
            let y = row_idx * cell_height;
            let bg = highlight_colors
                .and_then(|rows| rows.get(row_idx))
                .and_then(|row_colors| row_colors.get(col_idx))
                .copied()
                .flatten()
                .map(rgb_hex)
                .unwrap_or_else(|| display_color_hex(cell.bg));
            if bg != "#00000000" && bg != "#000000" {
                svg.push_str(&format!(
                    r#"<rect x="{x}" y="{y}" width="{cell_width}" height="{cell_height}" fill="{bg}"/>"#
                ));
            }
            let fill = display_color_hex(cell.fg);
            let text = svg_escape(&cell.text);
            svg.push_str(&format!(
                r#"<text x="{x}" y="{y}" fill="{fill}">{text}</text>"#
            ));
        }
    }
    svg.push_str("</g></svg>");
    svg
}

impl StyledCell {
    pub(crate) fn blank(theme: Option<&Theme>) -> Self {
        let preserve_terminal = theme_preserves_terminal_colors(theme);
        Self {
            text: " ".to_string(),
            fg: if preserve_terminal {
                DisplayColor::Default
            } else {
                theme.map_or(DisplayColor::Rgb(Rgb(255, 255, 255)), |theme| {
                    DisplayColor::Rgb(theme.default_fg_rgb())
                })
            },
            bg: if preserve_terminal {
                DisplayColor::Default
            } else {
                theme.map_or(DisplayColor::Rgb(Rgb(0, 0, 0)), |theme| {
                    DisplayColor::Rgb(theme.default_bg_rgb())
                })
            },
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            inverse: false,
            wide_continuation: false,
        }
    }
}

pub(crate) fn theme_preserves_terminal_colors(theme: Option<&Theme>) -> bool {
    theme.is_none_or(|theme| {
        !theme.force_default
            && theme.palette_map.is_empty()
            && theme.background_palette_map.is_empty()
            && theme.foreground.is_empty()
            && theme.background.is_empty()
    })
}

pub(crate) fn display_vt_color(
    theme: Option<&Theme>,
    color: vt100::Color,
    foreground: bool,
) -> DisplayColor {
    if let Some(theme) = theme {
        if theme_preserves_terminal_colors(Some(theme)) {
            return match color {
                vt100::Color::Default => DisplayColor::Default,
                vt100::Color::Idx(idx) => DisplayColor::Indexed(idx),
                vt100::Color::Rgb(r, g, b) => DisplayColor::Rgb(Rgb(r, g, b)),
            };
        }

        return DisplayColor::Rgb(theme.map_vt_color(color, foreground));
    }

    match color {
        vt100::Color::Default => DisplayColor::Default,
        vt100::Color::Idx(idx) => DisplayColor::Indexed(idx),
        vt100::Color::Rgb(r, g, b) => DisplayColor::Rgb(Rgb(r, g, b)),
    }
}

pub(crate) fn diff_screen(
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

pub(crate) fn row_has_changes(changed: &[Vec<bool>], row: usize) -> bool {
    changed
        .get(row)
        .is_some_and(|cells| cells.iter().copied().any(|is_changed| is_changed))
}

pub(crate) fn detect_scroll_hint(
    prev: Option<&Vec<Vec<StyledCell>>>,
    current: &[Vec<StyledCell>],
    rows: usize,
    cols: usize,
) -> Option<ScrollHint> {
    let prev = prev?;
    if rows < 2 || cols == 0 {
        return None;
    }

    for offset in 1..rows {
        let scrolled_up = (0..rows.saturating_sub(offset)).all(|r| {
            prev.get(r + offset)
                .zip(current.get(r))
                .is_some_and(|(a, b)| rows_equal(a, b, cols))
        });
        if scrolled_up {
            return Some(ScrollHint::Up(offset as u16));
        }
    }

    for offset in 1..rows {
        let scrolled_down = (offset..rows).all(|r| {
            prev.get(r - offset)
                .zip(current.get(r))
                .is_some_and(|(a, b)| rows_equal(a, b, cols))
        });
        if scrolled_down {
            return Some(ScrollHint::Down(offset as u16));
        }
    }

    None
}

pub(crate) fn apply_scroll_hint(
    changed: &mut [Vec<bool>],
    scroll_hint: Option<ScrollHint>,
    rows: usize,
    cols: usize,
) {
    let Some(hint) = scroll_hint else {
        return;
    };

    match hint {
        ScrollHint::Up(lines) => {
            let preserved = rows.saturating_sub(lines as usize);
            for row in changed.iter_mut().take(preserved) {
                for cell in row.iter_mut().take(cols) {
                    *cell = false;
                }
            }
        }
        ScrollHint::Down(lines) => {
            let start = (lines as usize).min(rows);
            for row in changed
                .iter_mut()
                .skip(start)
                .take(rows.saturating_sub(start))
            {
                for cell in row.iter_mut().take(cols) {
                    *cell = false;
                }
            }
        }
    }
}

pub(crate) fn full_screen_changed(cells: &[Vec<StyledCell>]) -> Vec<Vec<bool>> {
    cells.iter().map(|row| vec![true; row.len()]).collect()
}

pub(crate) fn no_screen_changed(cells: &[Vec<StyledCell>]) -> Vec<Vec<bool>> {
    cells.iter().map(|row| vec![false; row.len()]).collect()
}

fn display_color_hex(color: DisplayColor) -> String {
    match color {
        DisplayColor::Default => "#d0d0d0".to_string(),
        DisplayColor::Indexed(idx) => rgb_hex(indexed_to_rgb(idx)),
        DisplayColor::Rgb(rgb) => rgb_hex(rgb),
    }
}

fn indexed_to_rgb(idx: u8) -> Rgb {
    match idx {
        0 => Rgb(0, 0, 0),
        1 => Rgb(205, 49, 49),
        2 => Rgb(13, 188, 121),
        3 => Rgb(229, 229, 16),
        4 => Rgb(36, 114, 200),
        5 => Rgb(188, 63, 188),
        6 => Rgb(17, 168, 205),
        7 => Rgb(229, 229, 229),
        8 => Rgb(102, 102, 102),
        9 => Rgb(241, 76, 76),
        10 => Rgb(35, 209, 139),
        11 => Rgb(245, 245, 67),
        12 => Rgb(59, 142, 234),
        13 => Rgb(214, 112, 214),
        14 => Rgb(41, 184, 219),
        _ => Rgb(255, 255, 255),
    }
}

fn rgb_hex(rgb: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb.0, rgb.1, rgb.2)
}

fn svg_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn rows_equal(a: &[StyledCell], b: &[StyledCell], cols: usize) -> bool {
    a.iter().take(cols).eq(b.iter().take(cols))
}
