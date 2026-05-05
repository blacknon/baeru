mod pty;
mod render;

use self::{pty::*, render::*};
use crate::{
    model::{EffectKind, Feature, Runtime, Theme},
    support::{exit_with_status, sleep_frame, spawn_direct},
};
use anyhow::Result;
use crossterm::{
    cursor::{Hide, Show},
    execute,
    terminal::{size, Clear, ClearType, EnterAlternateScreen},
};
use std::{
    collections::HashMap,
    io::{self, Read, Write},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

pub(crate) fn run_tui_backend(rt: Runtime) -> Result<()> {
    if rt.features.contains(&Feature::LiveRender) {
        return run_live_render(rt);
    }
    if rt.features.contains(&Feature::Splash) {
        return run_splash(rt);
    }
    if rt.features.contains(&Feature::Reveal) {
        return run_reveal(rt);
    }
    let theme = rt
        .features
        .contains(&Feature::LiveColor)
        .then_some(rt.theme.clone());
    let keymap = if rt.features.contains(&Feature::Keymap) {
        rt.keymap.clone()
    } else {
        HashMap::new()
    };
    run_pty_passthrough(rt, theme, keymap)
}

fn run_splash(rt: Runtime) -> Result<()> {
    let guard = TerminalGuard::enter(true)?;
    let mut out = io::stdout();
    execute!(out, Hide, Clear(ClearType::All))?;
    let logo = "baeru";
    for frame in 0..rt.frames {
        let ratio = frame as f32 / rt.frames.saturating_sub(1).max(1) as f32;
        execute!(out, crossterm::cursor::MoveTo(0, 0), Clear(ClearType::All))?;
        write!(out, "{}\r\n", coalesce_text(logo, ratio, rt.effect))?;
        write!(out, "making terminal apps glow up...\r\n")?;
        out.flush()?;
        sleep_frame(rt.duration_ms, rt.frames);
    }
    execute!(out, Show, Clear(ClearType::All))?;
    drop(guard);
    spawn_direct(&rt.command)
}

fn run_live_render(rt: Runtime) -> Result<()> {
    let (mut cols, mut rows) = size().unwrap_or((80, 24));
    let mut session = PtySession::spawn(&rt.command, rows, cols)?;
    let mut parser = vt100::Parser::new(rows, cols, 0);
    let mut buf = [0u8; 8192];
    let deadline = Instant::now() + Duration::from_millis(rt.capture_ms);
    let mut captured = Vec::new();
    while Instant::now() < deadline {
        match session.reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                parser.process(&buf[..n]);
                captured.extend_from_slice(&buf[..n]);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }

    let emulate_alt_screen = contains_alt_screen_enter_sequence(&captured);
    let mut initial_screen = collect_screen(parser.screen(), rows, cols, Some(&rt.theme));
    apply_transforms_to_cells(&mut initial_screen, &rt.output_transforms);
    let initial_state = collect_terminal_state(parser.screen(), rows, cols);
    let initial_lines = screen_lines(&initial_screen);
    let initial_highlights = crate::highlight::evaluate_lines(&initial_lines, &rt.highlight_rules);
    let mut trigger_state = crate::highlight::TriggerState::default();
    let new_triggers = crate::highlight::filter_new_triggers(
        &mut trigger_state,
        initial_highlights.triggers.clone(),
    );
    crate::highlight::dispatch_tui_triggers(
        &new_triggers,
        Some(&screen_to_svg(
            &initial_screen,
            Some(&initial_highlights.colors),
        )),
    )?;

    let _guard = TerminalGuard::enter(true)?;
    let mut stdout = io::stdout();
    if emulate_alt_screen {
        execute!(stdout, EnterAlternateScreen)?;
    }
    let _signal_cleanup = SignalCleanupWatcher::spawn(SignalCleanupConfig {
        leave_alt_screen: true,
        reset_live_sequences: true,
    });
    animate_styled_reveal_in_place(
        &initial_screen,
        Some(&initial_highlights.colors),
        rows,
        cols,
        RevealTuning {
            frames: rt.frames,
            duration_ms: rt.duration_ms,
            effect: rt.effect,
            color_fade: rt.animation_color_fade,
            darken_factor: rt.animation_color_darken_factor,
        },
    )?;
    let mut passthrough = LivePassthrough::default();
    passthrough.feed(&mut stdout, &captured)?;
    draw_live_screen(
        &mut stdout,
        LiveRenderScene {
            cells: &initial_screen,
            state: &initial_state,
            highlight_colors: Some(&initial_highlights.colors),
            theme: &rt.theme,
        },
        LiveRenderTuning {
            duration_ms: rt.live_render_duration_ms,
            color_fade: rt.animation_color_fade,
            darken_factor: rt.animation_color_darken_factor,
        },
        LiveRenderFrame {
            scroll_hint: None,
            effect: EffectKind::Plain,
            frame: 0,
            ratio: 1.0,
        },
        RedrawMasks {
            redraw: &full_screen_changed(&initial_screen),
            highlight: &no_screen_changed(&initial_screen),
        },
    )?;

    let mouse_quiet_window = Duration::from_millis(rt.live_render_mouse_quiet_ms);
    let writer = Arc::new(Mutex::new(session.writer));
    let mouse_activity = Arc::new(Mutex::new(None));
    let _input_handle = spawn_input_forwarder(
        writer.clone(),
        rt.keymap.clone(),
        Some(mouse_activity.clone()),
    );

    let mut prev: Option<Vec<Vec<StyledCell>>> = Some(initial_screen);
    let mut frame = 0usize;

    loop {
        match session.reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if let Ok((new_cols, new_rows)) = size() {
                    if new_cols != cols || new_rows != rows {
                        cols = new_cols;
                        rows = new_rows;
                        parser = vt100::Parser::new(rows, cols, 0);
                        prev = None;
                        execute!(
                            stdout,
                            Clear(ClearType::All),
                            crossterm::cursor::MoveTo(0, 0)
                        )?;
                    }
                }
                passthrough.feed(&mut stdout, &buf[..n])?;
                parser.process(&buf[..n]);
                let mut current = collect_screen(parser.screen(), rows, cols, Some(&rt.theme));
                apply_transforms_to_cells(&mut current, &rt.output_transforms);
                let current_lines = screen_lines(&current);
                let current_highlights =
                    crate::highlight::evaluate_lines(&current_lines, &rt.highlight_rules);
                let new_triggers = crate::highlight::filter_new_triggers(
                    &mut trigger_state,
                    current_highlights.triggers.clone(),
                );
                crate::highlight::dispatch_tui_triggers(
                    &new_triggers,
                    Some(&screen_to_svg(&current, Some(&current_highlights.colors))),
                )?;
                let state = collect_terminal_state(parser.screen(), rows, cols);
                let scroll_hint =
                    detect_scroll_hint(prev.as_ref(), &current, rows as usize, cols as usize);
                let mut changed =
                    diff_screen(prev.as_ref(), &current, rows as usize, cols as usize);
                apply_scroll_hint(&mut changed, scroll_hint, rows as usize, cols as usize);
                animate_live_render_update(
                    &mut stdout,
                    LiveRenderScene {
                        cells: &current,
                        state: &state,
                        highlight_colors: Some(&current_highlights.colors),
                        theme: &rt.theme,
                    },
                    LiveRenderFrame {
                        scroll_hint,
                        effect: if passthrough.mouse_reporting_active
                            && mouse_activity_recent(&mouse_activity, mouse_quiet_window)
                        {
                            EffectKind::Plain
                        } else {
                            rt.effect
                        },
                        frame,
                        ratio: 1.0,
                    },
                    LiveRenderTuning {
                        duration_ms: rt.live_render_duration_ms,
                        color_fade: rt.animation_color_fade,
                        darken_factor: rt.animation_color_darken_factor,
                    },
                    RedrawMasks {
                        redraw: &changed,
                        highlight: &changed,
                    },
                )?;
                prev = Some(current);
                frame = frame.wrapping_add(1);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }

    let status = session.child.wait()?;
    reset_live_passthrough_sequences(&mut stdout)?;
    exit_with_status(status.exit_code() as i32);
}

fn animate_live_render_update(
    out: &mut io::Stdout,
    scene: LiveRenderScene<'_>,
    base_frame: LiveRenderFrame,
    tuning: LiveRenderTuning,
    masks: RedrawMasks<'_>,
) -> io::Result<()> {
    let ratios = live_render_effect_ratios(base_frame.effect);
    let per_frame_ms = live_render_per_frame_ms(tuning.duration_ms, base_frame.effect);
    let full_redraw = full_screen_changed(scene.cells);
    let no_highlight = no_screen_changed(scene.cells);

    for (idx, ratio) in ratios.iter().copied().enumerate() {
        let redraw_mask = if idx + 1 == ratios.len() {
            &full_redraw
        } else {
            masks.redraw
        };
        let highlight_mask = if idx + 1 == ratios.len() {
            &no_highlight
        } else {
            masks.highlight
        };
        draw_live_screen(
            out,
            LiveRenderScene {
                cells: scene.cells,
                state: scene.state,
                highlight_colors: scene.highlight_colors,
                theme: scene.theme,
            },
            tuning,
            LiveRenderFrame {
                scroll_hint: if idx == 0 {
                    base_frame.scroll_hint
                } else {
                    None
                },
                effect: base_frame.effect,
                frame: base_frame.frame + idx,
                ratio,
            },
            RedrawMasks {
                redraw: redraw_mask,
                highlight: highlight_mask,
            },
        )?;
        if idx + 1 < ratios.len() {
            thread::sleep(Duration::from_millis(per_frame_ms));
        }
    }

    Ok(())
}

fn run_reveal(rt: Runtime) -> Result<()> {
    let (cols, rows) = size().unwrap_or((80, 24));
    let mut session = PtySession::spawn(&rt.command, rows, cols)?;
    let mut parser = vt100::Parser::new(rows, cols, 0);
    let deadline = Instant::now() + Duration::from_millis(rt.capture_ms);
    let mut buf = [0u8; 8192];
    let mut captured = Vec::new();
    while Instant::now() < deadline {
        match session.reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                parser.process(&buf[..n]);
                captured.extend_from_slice(&buf[..n]);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }

    let emulate_alt_screen = contains_alt_screen_enter_sequence(&captured);
    if emulate_alt_screen {
        execute!(io::stdout(), EnterAlternateScreen)?;
    }

    let theme = if rt.no_theme_after_reveal || !rt.features.contains(&Feature::LiveColor) {
        None
    } else {
        Some(rt.theme.clone())
    };
    let mut screen = collect_screen(parser.screen(), rows, cols, theme.as_ref());
    apply_transforms_to_cells(&mut screen, &rt.output_transforms);
    let highlight_eval =
        crate::highlight::evaluate_lines(&screen_lines(&screen), &rt.highlight_rules);
    crate::highlight::dispatch_tui_triggers(
        &highlight_eval.triggers,
        Some(&screen_to_svg(&screen, Some(&highlight_eval.colors))),
    )?;
    animate_styled_reveal(
        &screen,
        Some(&highlight_eval.colors),
        rows,
        cols,
        RevealTuning {
            frames: rt.frames,
            duration_ms: rt.duration_ms,
            effect: rt.effect,
            color_fade: rt.animation_color_fade,
            darken_factor: rt.animation_color_darken_factor,
        },
        TerminalGuard::enter,
    )?;

    let keymap = if rt.features.contains(&Feature::Keymap) {
        rt.keymap.clone()
    } else {
        HashMap::new()
    };
    let initial_output = if emulate_alt_screen {
        strip_alt_screen_enter_sequences(&captured)
    } else {
        captured
    };
    continue_passthrough(
        session,
        theme,
        keymap,
        rt.highlight_rules.clone(),
        Some(initial_output),
        emulate_alt_screen,
        maybe_dispatch_passthrough_highlights,
    )
}

fn run_pty_passthrough(
    rt: Runtime,
    theme: Option<Theme>,
    keymap: HashMap<Vec<u8>, Vec<u8>>,
) -> Result<()> {
    let (cols, rows) = size().unwrap_or((80, 24));
    let session = PtySession::spawn(&rt.command, rows, cols)?;
    continue_passthrough(
        session,
        theme,
        keymap,
        rt.highlight_rules.clone(),
        None,
        false,
        maybe_dispatch_passthrough_highlights,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Rgb;
    use crate::model::{
        LIVE_RENDER_INPUT_MODE_DISABLE_SEQUENCES, LIVE_RENDER_INPUT_MODE_ENABLE_SEQUENCES,
        LIVE_RENDER_MOUSE_DISABLE_SEQUENCES, LIVE_RENDER_MOUSE_ENABLE_SEQUENCES,
        LIVE_RENDER_PASTE_MODE_DISABLE_SEQUENCES, LIVE_RENDER_PASTE_MODE_ENABLE_SEQUENCES,
    };

    #[test]
    fn detects_alternate_screen_sequences() {
        assert!(contains_alt_screen_enter_sequence(b"\x1b[?1049hhello"));
        assert!(contains_alt_screen_enter_sequence(b"\x1b[?47hhello"));
        assert!(!contains_alt_screen_enter_sequence(b"\x1b[31mhello"));
    }

    #[test]
    fn strips_alternate_screen_sequences_from_capture() {
        let stripped = strip_alt_screen_enter_sequences(b"\x1b[?1049habc\x1b[?47hdef");

        assert_eq!(stripped, b"abcdef".to_vec());
    }

    #[test]
    fn live_render_passthrough_detects_mouse_enable_sequences() {
        let bytes = b"\x1b[?1002hhello\x1b[?1006h";

        let mut found = Vec::new();
        for pattern in LIVE_RENDER_MOUSE_ENABLE_SEQUENCES {
            for _ in bytes
                .windows(pattern.len())
                .filter(|window| *window == *pattern)
            {
                found.push(*pattern);
            }
        }

        assert!(found.contains(&b"\x1b[?1002h".as_slice()));
        assert!(found.contains(&b"\x1b[?1006h".as_slice()));
    }

    #[test]
    fn live_render_passthrough_detects_mouse_disable_sequences() {
        let bytes = b"\x1b[?1002lhello\x1b[?1006l";

        let mut found = Vec::new();
        for pattern in LIVE_RENDER_MOUSE_DISABLE_SEQUENCES {
            for _ in bytes
                .windows(pattern.len())
                .filter(|window| *window == *pattern)
            {
                found.push(*pattern);
            }
        }

        assert!(found.contains(&b"\x1b[?1002l".as_slice()));
        assert!(found.contains(&b"\x1b[?1006l".as_slice()));
    }

    #[test]
    fn live_render_passthrough_detects_input_mode_sequences() {
        let bytes = b"\x1b[?1hhello\x1b=\x1b[?1l\x1b>";

        let mut found_enable = Vec::new();
        for pattern in LIVE_RENDER_INPUT_MODE_ENABLE_SEQUENCES {
            for _ in bytes
                .windows(pattern.len())
                .filter(|window| *window == *pattern)
            {
                found_enable.push(*pattern);
            }
        }

        let mut found_disable = Vec::new();
        for pattern in LIVE_RENDER_INPUT_MODE_DISABLE_SEQUENCES {
            for _ in bytes
                .windows(pattern.len())
                .filter(|window| *window == *pattern)
            {
                found_disable.push(*pattern);
            }
        }

        assert!(found_enable.contains(&b"\x1b[?1h".as_slice()));
        assert!(found_enable.contains(&b"\x1b=".as_slice()));
        assert!(found_disable.contains(&b"\x1b[?1l".as_slice()));
        assert!(found_disable.contains(&b"\x1b>".as_slice()));
    }

    #[test]
    fn live_render_passthrough_detects_bracketed_paste_mode_sequences() {
        let bytes = b"\x1b[?2004hhello\x1b[?2004l";

        let mut found_enable = Vec::new();
        for pattern in LIVE_RENDER_PASTE_MODE_ENABLE_SEQUENCES {
            for _ in bytes
                .windows(pattern.len())
                .filter(|window| *window == *pattern)
            {
                found_enable.push(*pattern);
            }
        }

        let mut found_disable = Vec::new();
        for pattern in LIVE_RENDER_PASTE_MODE_DISABLE_SEQUENCES {
            for _ in bytes
                .windows(pattern.len())
                .filter(|window| *window == *pattern)
            {
                found_disable.push(*pattern);
            }
        }

        assert!(found_enable.contains(&b"\x1b[?2004h".as_slice()));
        assert!(found_disable.contains(&b"\x1b[?2004l".as_slice()));
    }

    #[test]
    fn live_passthrough_tracks_split_mouse_enable_sequence() {
        let mut passthrough = LivePassthrough::default();
        let mut out = Vec::new();

        passthrough
            .feed(&mut out, b"\x1b[?10")
            .expect("feed should work");
        assert!(!passthrough.mouse_reporting_active);
        passthrough
            .feed(&mut out, b"00h")
            .expect("second feed should work");

        assert!(passthrough.mouse_reporting_active);
    }

    #[test]
    fn live_passthrough_keeps_partial_suffix_for_next_chunk() {
        assert_eq!(longest_live_passthrough_suffix(b"\x1b[?10"), 5);
        assert_eq!(longest_live_passthrough_suffix(b"hello"), 0);
    }

    #[test]
    fn noise_suppression_input_matches_up_down_and_wheel_or_drag() {
        assert!(contains_noise_suppression_input(b"\x1b[A"));
        assert!(contains_noise_suppression_input(b"\x1b[B"));
        assert!(contains_noise_suppression_input(b"\x1b[<64;10;5M"));
        assert!(contains_noise_suppression_input(b"\x1b[<32;10;5M"));
        assert!(contains_noise_suppression_input(b"\x1b[M`!!"));
        assert!(contains_noise_suppression_input(b"\x1b[M@!!"));
        assert!(!contains_noise_suppression_input(b"\x1b[C"));
        assert!(!contains_noise_suppression_input(b"\x1b[<0;10;5M"));
    }

    #[test]
    fn strip_byte_sequence_removes_all_occurrences() {
        let stripped = strip_byte_sequence(b"xxSTARTmiddlexxSTARTtail", b"START");

        assert_eq!(stripped, b"xxmiddlexxtail".to_vec());
    }

    #[test]
    fn row_has_changes_detects_dirty_rows() {
        let changed = vec![vec![false, true, false], vec![false, false, false]];

        assert!(row_has_changes(&changed, 0));
        assert!(!row_has_changes(&changed, 1));
        assert!(!row_has_changes(&changed, 2));
    }

    #[test]
    fn live_render_effect_ratios_match_effect_style() {
        assert_eq!(live_render_effect_ratios(EffectKind::Plain), &[1.0]);
        assert_eq!(live_render_effect_ratios(EffectKind::Fade), &[0.35, 1.0]);
        assert_eq!(
            live_render_effect_ratios(EffectKind::Wipe),
            &[0.2, 0.55, 1.0]
        );
        assert_eq!(
            live_render_effect_ratios(EffectKind::Sweep),
            &[0.25, 0.65, 1.0]
        );
        assert_eq!(
            live_render_effect_ratios(EffectKind::Coalesce),
            &[0.12, 0.38, 0.72, 1.0]
        );
        assert_eq!(
            live_render_effect_ratios(EffectKind::Glitch),
            &[0.08, 0.16, 0.32, 0.55, 1.0]
        );
        assert_eq!(
            live_render_effect_ratios(EffectKind::Matrix),
            &[0.08, 0.24, 0.45, 0.72, 1.0]
        );
        assert_eq!(
            live_render_effect_ratios(EffectKind::Scanline),
            &[0.12, 0.35, 0.68, 1.0]
        );
        assert_eq!(
            live_render_effect_ratios(EffectKind::Scatter),
            &[0.1, 0.28, 0.58, 1.0]
        );
    }

    #[test]
    fn live_render_per_frame_ms_uses_configured_duration_without_tight_clamp() {
        assert_eq!(live_render_per_frame_ms(8000, EffectKind::Coalesce), 2000);
        assert_eq!(live_render_per_frame_ms(90, EffectKind::Fade), 45);
    }

    #[test]
    fn live_render_coalesce_uses_noise_before_settling() {
        let cell = cell_with("X", &StyledCell::blank(None));

        let early = live_render_text_for_cell(
            &cell,
            CellAnimCtx {
                row: 0,
                col: 0,
                total_rows: 1,
                frame: 0,
                ratio: 0.12,
                color_fade: false,
                effect: EffectKind::Coalesce,
            },
        );
        let late = live_render_text_for_cell(
            &cell,
            CellAnimCtx {
                row: 0,
                col: 0,
                total_rows: 1,
                frame: 3,
                ratio: 1.0,
                color_fade: false,
                effect: EffectKind::Coalesce,
            },
        );

        assert_ne!(early, " ");
        assert_eq!(late, "X");
    }

    #[test]
    fn live_render_matrix_uses_noise_before_settling() {
        let cell = cell_with("X", &StyledCell::blank(None));

        let early = live_render_text_for_cell(
            &cell,
            CellAnimCtx {
                row: 0,
                col: 0,
                total_rows: 1,
                frame: 0,
                ratio: 0.08,
                color_fade: false,
                effect: EffectKind::Matrix,
            },
        );
        let late = live_render_text_for_cell(
            &cell,
            CellAnimCtx {
                row: 0,
                col: 0,
                total_rows: 1,
                frame: 4,
                ratio: 1.0,
                color_fade: false,
                effect: EffectKind::Matrix,
            },
        );

        assert_ne!(early, " ");
        assert_eq!(late, "X");
    }

    #[test]
    fn live_render_coalesce_with_color_fade_shows_original_char_earlier() {
        let cell = cell_with("X", &StyledCell::blank(None));

        let mid = live_render_text_for_cell(
            &cell,
            CellAnimCtx {
                row: 0,
                col: 0,
                total_rows: 1,
                frame: 1,
                ratio: 0.5,
                color_fade: true,
                effect: EffectKind::Coalesce,
            },
        );

        assert_eq!(mid, "X");
    }

    #[test]
    fn reveal_fade_keeps_original_text_visible() {
        let cell = cell_with("X", &StyledCell::blank(None));

        let early = reveal_text_for_cell(&cell, 0, 0, 1, 0, 0.1, EffectKind::Fade);
        let late = reveal_text_for_cell(&cell, 0, 0, 1, 3, 1.0, EffectKind::Fade);

        assert_eq!(early, "X");
        assert_eq!(late, "X");
    }

    #[test]
    fn reveal_scatter_settles_from_random_position_order() {
        let cell = cell_with("X", &StyledCell::blank(None));

        let early = reveal_text_for_cell(&cell, 0, 0, 2, 0, 0.0, EffectKind::Scatter);
        let late = reveal_text_for_cell(&cell, 0, 0, 2, 3, 1.0, EffectKind::Scatter);

        assert_ne!(early, "X");
        assert_eq!(late, "X");
    }

    #[test]
    fn full_screen_changed_marks_every_cell_dirty() {
        let blank = StyledCell::blank(None);
        let cells = vec![
            vec![cell_with("a", &blank), cell_with("b", &blank)],
            vec![cell_with("c", &blank)],
        ];

        let changed = full_screen_changed(&cells);

        assert_eq!(changed, vec![vec![true, true], vec![true]]);
    }

    #[test]
    fn no_screen_changed_marks_every_cell_clean() {
        let blank = StyledCell::blank(None);
        let cells = vec![
            vec![cell_with("a", &blank), cell_with("b", &blank)],
            vec![cell_with("c", &blank)],
        ];

        let changed = no_screen_changed(&cells);

        assert_eq!(changed, vec![vec![false, false], vec![false]]);
    }

    #[test]
    fn animated_faded_color_recovers_target_color() {
        let target = DisplayColor::Rgb(Rgb(200, 100, 50));

        let faded = animated_faded_color(target, 0.2, EffectKind::Matrix, 0.25);
        let settled = animated_faded_color(target, 1.0, EffectKind::Matrix, 0.25);

        assert_ne!(faded, target);
        assert_eq!(settled, target);
    }

    #[test]
    fn default_theme_preserves_terminal_default_and_indexed_colors() {
        let theme = crate::theme::builtin_theme("default");

        assert_eq!(
            display_vt_color(Some(&theme), vt100::Color::Default, true),
            DisplayColor::Default
        );
        assert_eq!(
            display_vt_color(Some(&theme), vt100::Color::Idx(6), true),
            DisplayColor::Indexed(6)
        );
    }

    #[test]
    fn themed_palette_maps_to_rgb_for_live_render() {
        let theme = crate::theme::builtin_theme("matrix-green");

        assert_eq!(
            display_vt_color(Some(&theme), vt100::Color::Default, false),
            DisplayColor::Rgb(theme.default_bg_rgb())
        );
        assert!(matches!(
            display_vt_color(Some(&theme), vt100::Color::Idx(2), true),
            DisplayColor::Rgb(_)
        ));
    }

    #[test]
    fn default_theme_live_render_highlight_keeps_original_cell_color() {
        let theme = crate::theme::builtin_theme("default");
        let mut cell = StyledCell::blank(Some(&theme));
        cell.text = "X".to_string();
        cell.fg = DisplayColor::Indexed(6);

        let mut highlighted = cell.clone();
        if !theme_preserves_terminal_colors(Some(&theme)) {
            highlighted.bold = true;
            highlighted.fg = DisplayColor::Rgb(theme.default_fg_rgb());
        }

        assert_eq!(highlighted.fg, DisplayColor::Indexed(6));
        assert!(!highlighted.bold);
    }

    #[test]
    fn detect_scroll_hint_finds_single_line_up_scroll() {
        let blank = StyledCell::blank(None);
        let prev = vec![
            vec![cell_with("a", &blank), cell_with("a", &blank)],
            vec![cell_with("b", &blank), cell_with("b", &blank)],
            vec![cell_with("c", &blank), cell_with("c", &blank)],
        ];
        let current = vec![
            vec![cell_with("b", &blank), cell_with("b", &blank)],
            vec![cell_with("c", &blank), cell_with("c", &blank)],
            vec![cell_with("d", &blank), cell_with("d", &blank)],
        ];

        assert_eq!(
            detect_scroll_hint(Some(&prev), &current, 3, 2),
            Some(ScrollHint::Up(1))
        );
    }

    #[test]
    fn apply_scroll_hint_keeps_only_new_edge_row_dirty() {
        let mut changed = vec![vec![true; 3], vec![true; 3], vec![true; 3]];

        apply_scroll_hint(&mut changed, Some(ScrollHint::Up(1)), 3, 3);

        assert_eq!(changed[0], vec![false; 3]);
        assert_eq!(changed[1], vec![false; 3]);
        assert_eq!(changed[2], vec![true; 3]);
    }

    #[test]
    fn detect_scroll_hint_finds_multi_line_up_scroll() {
        let blank = StyledCell::blank(None);
        let prev = vec![
            vec![cell_with("a", &blank)],
            vec![cell_with("b", &blank)],
            vec![cell_with("c", &blank)],
            vec![cell_with("d", &blank)],
        ];
        let current = vec![
            vec![cell_with("c", &blank)],
            vec![cell_with("d", &blank)],
            vec![cell_with("x", &blank)],
            vec![cell_with("y", &blank)],
        ];

        assert_eq!(
            detect_scroll_hint(Some(&prev), &current, 4, 1),
            Some(ScrollHint::Up(2))
        );
    }

    #[test]
    fn collect_terminal_state_clamps_cursor_to_screen_bounds() {
        let mut parser = vt100::Parser::new(2, 3, 0);
        parser.process(b"\x1b[99;99H");
        parser.process(b"\x1b[?25l");

        let state = collect_terminal_state(parser.screen(), 2, 3);

        assert_eq!(state.cursor_row, 1);
        assert_eq!(state.cursor_col, 2);
        assert!(!state.cursor_visible);
    }

    fn cell_with(text: &str, blank: &StyledCell) -> StyledCell {
        let mut cell = blank.clone();
        cell.text = text.to_string();
        cell
    }
}
