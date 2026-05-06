mod effects;
mod screen;

pub(super) use self::effects::{
    animate_styled_reveal, animate_styled_reveal_in_place, coalesce_text, draw_live_screen,
    flash_highlight_markers, live_render_effect_ratios, live_render_per_frame_ms, LiveRenderFrame,
    LiveRenderTuning, RedrawMasks, RevealTuning,
};
#[cfg(test)]
pub(super) use self::effects::{
    animated_faded_color, live_render_text_for_cell, reveal_text_for_cell, CellAnimCtx,
};
pub(super) use self::screen::{
    apply_scroll_hint, apply_transforms_to_cells, collect_screen, collect_terminal_state,
    detect_scroll_hint, diff_screen, full_screen_changed, no_screen_changed, screen_lines,
    screen_to_svg, LiveRenderScene, StyledCell, TerminalState,
};
#[cfg(test)]
pub(super) use self::screen::{
    display_vt_color, row_has_changes, theme_preserves_terminal_colors, DisplayColor, ScrollHint,
};
