use crate::model::{ColorStop, Rgb, Theme};
use anyhow::{anyhow, Result};
use std::{collections::HashMap, fs, path::Path};

pub(crate) fn read_theme(path: &Path) -> Result<Theme> {
    let s = fs::read_to_string(path)?;
    Ok(serde_yaml::from_str(&s)?)
}

pub(crate) fn builtin_theme(name: &str) -> Theme {
    match name {
        "matrix" | "matrix-green" => Theme {
            _name: Some("matrix-green".to_string()),
            default_fg: Some("#b6ffd0".to_string()),
            default_bg: Some("#001008".to_string()),
            force_default: true,
            palette_map: HashMap::new(),
            background_palette_map: HashMap::new(),
            foreground: vec![
                stop(0.00, "#004d26"),
                stop(0.35, "#00aa55"),
                stop(0.70, "#33ff99"),
                stop(1.00, "#eafff2"),
            ],
            background: vec![
                stop(0.00, "#001008"),
                stop(0.50, "#003018"),
                stop(1.00, "#006633"),
            ],
        },
        _ => Theme {
            _name: Some("jirai-pink".to_string()),
            default_fg: Some("#ffcdeb".to_string()),
            default_bg: Some("#120018".to_string()),
            force_default: true,
            palette_map: HashMap::new(),
            background_palette_map: HashMap::new(),
            foreground: vec![
                stop(0.00, "#84205c"),
                stop(0.30, "#ff45ac"),
                stop(0.62, "#ff8fd6"),
                stop(0.84, "#ffcdeb"),
                stop(1.00, "#fff2fa"),
            ],
            background: vec![
                stop(0.00, "#120018"),
                stop(0.28, "#26002a"),
                stop(0.55, "#570a41"),
                stop(0.78, "#962369"),
                stop(1.00, "#ff8fcf"),
            ],
        },
    }
}

fn stop(at: f32, color: &str) -> ColorStop {
    ColorStop {
        at,
        color: color.to_string(),
    }
}

impl Theme {
    pub(crate) fn default_fg_rgb(&self) -> Rgb {
        parse_hex(self.default_fg.as_deref().unwrap_or("#ffffff")).unwrap_or(Rgb(255, 255, 255))
    }

    pub(crate) fn default_bg_rgb(&self) -> Rgb {
        parse_hex(self.default_bg.as_deref().unwrap_or("#000000")).unwrap_or(Rgb(0, 0, 0))
    }

    pub(crate) fn map_vt_color(&self, color: vt100::Color, foreground: bool) -> Rgb {
        match color {
            vt100::Color::Default => {
                if foreground {
                    self.default_fg_rgb()
                } else {
                    self.default_bg_rgb()
                }
            }
            vt100::Color::Idx(idx) => self.map_indexed_color(idx, foreground),
            vt100::Color::Rgb(r, g, b) => self.map_rgb(Rgb(r, g, b), foreground),
        }
    }

    pub(crate) fn map_indexed_color(&self, idx: u8, foreground: bool) -> Rgb {
        let mapped = if foreground {
            self.palette_map.get(&idx)
        } else {
            self.background_palette_map
                .get(&idx)
                .or_else(|| self.palette_map.get(&idx))
        };
        if let Some(rgb) = mapped.and_then(|raw| parse_hex(raw)) {
            rgb
        } else {
            self.map_rgb(indexed_color(idx), foreground)
        }
    }

    pub(crate) fn map_rgb(&self, rgb: Rgb, foreground: bool) -> Rgb {
        let intensity = ((rgb.0 as f32 * 0.299 + rgb.1 as f32 * 0.587 + rgb.2 as f32 * 0.114)
            / 255.0)
            .clamp(0.0, 1.0);
        let stops = if foreground {
            &self.foreground
        } else {
            &self.background
        };
        gradient(stops, intensity).unwrap_or(rgb)
    }
}

pub(crate) fn gradient(stops: &[ColorStop], t: f32) -> Option<Rgb> {
    if stops.is_empty() {
        return None;
    }
    let mut parsed: Vec<(f32, Rgb)> = stops
        .iter()
        .filter_map(|s| parse_hex(&s.color).map(|c| (s.at, c)))
        .collect();
    if parsed.is_empty() {
        return None;
    }
    parsed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    if t <= parsed[0].0 {
        return Some(parsed[0].1);
    }
    for pair in parsed.windows(2) {
        let (a_t, a) = pair[0];
        let (b_t, b) = pair[1];
        if t <= b_t {
            let k = ((t - a_t) / (b_t - a_t).max(0.0001)).clamp(0.0, 1.0);
            return Some(Rgb(lerp(a.0, b.0, k), lerp(a.1, b.1, k), lerp(a.2, b.2, k)));
        }
    }
    Some(parsed.last().unwrap().1)
}

pub(crate) fn lerp(a: u8, b: u8, k: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * k)
        .round()
        .clamp(0.0, 255.0) as u8
}

pub(crate) fn parse_hex(s: &str) -> Option<Rgb> {
    let hex = s.strip_prefix('#').unwrap_or(s);
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Rgb(r, g, b))
}

pub(crate) fn parse_optional_rgb(value: Option<&str>) -> Result<Option<Rgb>> {
    match value {
        Some(raw) => parse_hex(raw)
            .map(Some)
            .ok_or_else(|| anyhow!("expected #RRGGBB, got: {raw}")),
        None => Ok(None),
    }
}

pub(crate) fn indexed_color(idx: u8) -> Rgb {
    const BASIC: [Rgb; 16] = [
        Rgb(0, 0, 0),
        Rgb(205, 49, 49),
        Rgb(13, 188, 121),
        Rgb(229, 229, 16),
        Rgb(36, 114, 200),
        Rgb(188, 63, 188),
        Rgb(17, 168, 205),
        Rgb(229, 229, 229),
        Rgb(102, 102, 102),
        Rgb(241, 76, 76),
        Rgb(35, 209, 139),
        Rgb(245, 245, 67),
        Rgb(59, 142, 234),
        Rgb(214, 112, 214),
        Rgb(41, 184, 219),
        Rgb(255, 255, 255),
    ];
    if idx < 16 {
        return BASIC[idx as usize];
    }
    if idx >= 232 {
        let value = 8 + (idx - 232) * 10;
        return Rgb(value, value, value);
    }
    let n = idx - 16;
    let r = n / 36;
    let g = (n % 36) / 6;
    let b = n % 6;
    let convert = |x: u8| if x == 0 { 0 } else { 55 + x * 40 };
    Rgb(convert(r), convert(g), convert(b))
}

pub(crate) struct SgrRewriter {
    theme: Theme,
    state: EscState,
    buf: Vec<u8>,
}

#[derive(Clone, Copy)]
enum EscState {
    Ground,
    Esc,
    Csi,
}

impl SgrRewriter {
    pub(crate) fn new(theme: Theme) -> Self {
        Self {
            theme,
            state: EscState::Ground,
            buf: Vec::new(),
        }
    }

    pub(crate) fn feed(&mut self, input: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(input.len() + 64);
        for &byte in input {
            match self.state {
                EscState::Ground => {
                    if byte == 0x1b {
                        self.buf.clear();
                        self.buf.push(byte);
                        self.state = EscState::Esc;
                    } else {
                        out.push(byte);
                    }
                }
                EscState::Esc => {
                    self.buf.push(byte);
                    if byte == b'[' {
                        self.state = EscState::Csi;
                    } else {
                        out.extend_from_slice(&self.buf);
                        self.state = EscState::Ground;
                    }
                }
                EscState::Csi => {
                    self.buf.push(byte);
                    if (0x40..=0x7e).contains(&byte) {
                        if byte == b'm' {
                            out.extend(self.rewrite_sgr());
                        } else {
                            out.extend_from_slice(&self.buf);
                        }
                        self.state = EscState::Ground;
                        self.buf.clear();
                    }
                }
            }
        }
        out
    }

    fn rewrite_sgr(&self) -> Vec<u8> {
        let body = &self.buf[2..self.buf.len() - 1];
        let text = String::from_utf8_lossy(body);
        let params: Vec<i32> = if text.is_empty() {
            vec![0]
        } else {
            text.split(';')
                .map(|p| {
                    if p.is_empty() {
                        0
                    } else {
                        p.parse().unwrap_or(-1)
                    }
                })
                .collect()
        };
        let mut out = Vec::new();
        let mut i = 0;
        while i < params.len() {
            let p = params[i];
            match p {
                0 => {
                    out.extend_from_slice(b"\x1b[0m");
                    if self.theme.force_default {
                        let fg = self.theme.default_fg_rgb();
                        let bg = self.theme.default_bg_rgb();
                        out.extend_from_slice(
                            format!(
                                "\x1b[38;2;{};{};{}m\x1b[48;2;{};{};{}m",
                                fg.0, fg.1, fg.2, bg.0, bg.1, bg.2
                            )
                            .as_bytes(),
                        );
                    }
                }
                30..=37 | 90..=97 => {
                    let idx = if p >= 90 {
                        (p - 90 + 8) as u8
                    } else {
                        (p - 30) as u8
                    };
                    let rgb = self.theme.map_indexed_color(idx, true);
                    out.extend_from_slice(
                        format!("\x1b[38;2;{};{};{}m", rgb.0, rgb.1, rgb.2).as_bytes(),
                    );
                }
                40..=47 | 100..=107 => {
                    let idx = if p >= 100 {
                        (p - 100 + 8) as u8
                    } else {
                        (p - 40) as u8
                    };
                    let rgb = self.theme.map_indexed_color(idx, false);
                    out.extend_from_slice(
                        format!("\x1b[48;2;{};{};{}m", rgb.0, rgb.1, rgb.2).as_bytes(),
                    );
                }
                39 => {
                    let fg = self.theme.default_fg_rgb();
                    out.extend_from_slice(
                        format!("\x1b[38;2;{};{};{}m", fg.0, fg.1, fg.2).as_bytes(),
                    );
                }
                49 => {
                    let bg = self.theme.default_bg_rgb();
                    out.extend_from_slice(
                        format!("\x1b[48;2;{};{};{}m", bg.0, bg.1, bg.2).as_bytes(),
                    );
                }
                38 | 48 => {
                    let is_fg = p == 38;
                    if i + 2 < params.len() && params[i + 1] == 5 {
                        let idx = params[i + 2].clamp(0, 255) as u8;
                        let rgb = self.theme.map_indexed_color(idx, is_fg);
                        out.extend_from_slice(sgr_rgb(is_fg, rgb).as_bytes());
                        i += 2;
                    } else if i + 4 < params.len() && params[i + 1] == 2 {
                        let original = Rgb(
                            params[i + 2].clamp(0, 255) as u8,
                            params[i + 3].clamp(0, 255) as u8,
                            params[i + 4].clamp(0, 255) as u8,
                        );
                        let rgb = self.theme.map_rgb(original, is_fg);
                        out.extend_from_slice(sgr_rgb(is_fg, rgb).as_bytes());
                        i += 4;
                    } else {
                        out.extend_from_slice(format!("\x1b[{}m", p).as_bytes());
                    }
                }
                _ => out.extend_from_slice(format!("\x1b[{}m", p).as_bytes()),
            }
            i += 1;
        }
        out
    }
}

fn sgr_rgb(fg: bool, rgb: Rgb) -> String {
    if fg {
        format!("\x1b[38;2;{};{};{}m", rgb.0, rgb.1, rgb.2)
    } else {
        format!("\x1b[48;2;{};{};{}m", rgb.0, rgb.1, rgb.2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn test_theme() -> Theme {
        Theme {
            _name: Some("test".to_string()),
            default_fg: Some("#ffffff".to_string()),
            default_bg: Some("#000000".to_string()),
            force_default: true,
            palette_map: HashMap::from([(2u8, "#112233".to_string())]),
            background_palette_map: HashMap::from([(4u8, "#445566".to_string())]),
            foreground: vec![
                ColorStop {
                    at: 0.0,
                    color: "#000000".to_string(),
                },
                ColorStop {
                    at: 1.0,
                    color: "#ffffff".to_string(),
                },
            ],
            background: vec![
                ColorStop {
                    at: 0.0,
                    color: "#000000".to_string(),
                },
                ColorStop {
                    at: 1.0,
                    color: "#ffffff".to_string(),
                },
            ],
        }
    }

    #[test]
    fn map_indexed_color_prefers_palette_override() {
        let theme = test_theme();

        assert_eq!(theme.map_indexed_color(2, true), Rgb(0x11, 0x22, 0x33));
        assert_eq!(theme.map_indexed_color(4, false), Rgb(0x44, 0x55, 0x66));
    }

    #[test]
    fn parse_optional_rgb_rejects_invalid_hex() {
        let result = parse_optional_rgb(Some("oops"));

        assert!(result.is_err());
    }

    #[test]
    fn sgr_rewriter_rewrites_basic_indexed_colors() {
        let mut rewriter = SgrRewriter::new(test_theme());
        let bytes = rewriter.feed(b"\x1b[32mhello\x1b[44m!");
        let output = String::from_utf8(bytes).expect("valid utf8");

        assert!(output.contains("\x1b[38;2;17;34;51mhello"));
        assert!(output.contains("\x1b[48;2;68;85;102m!"));
    }

    #[test]
    fn sgr_rewriter_rewrites_256_color_sequences() {
        let mut rewriter = SgrRewriter::new(test_theme());
        let bytes = rewriter.feed(b"\x1b[38;5;2mgreen\x1b[48;5;4mblue");
        let output = String::from_utf8(bytes).expect("valid utf8");

        assert!(output.contains("\x1b[38;2;17;34;51mgreen"));
        assert!(output.contains("\x1b[48;2;68;85;102mblue"));
    }

    #[test]
    fn read_theme_loads_example_fixture_with_palette_map() {
        let theme = read_theme(Path::new("examples/themes/gundam-tricolor-htop.yml"))
            .expect("theme should load");

        assert_eq!(theme._name.as_deref(), Some("gundam-tricolor-htop"));
        assert_eq!(
            theme.palette_map.get(&4).map(String::as_str),
            Some("#3f7dff")
        );
        assert_eq!(
            theme.background_palette_map.get(&2).map(String::as_str),
            Some("#10254f")
        );
    }

    #[test]
    fn read_theme_parses_palette_only_theme() {
        let path = unique_temp_file("baeru-theme-test.yml");
        fs::write(
            &path,
            r##"
name: fixture
default_fg: "#eeeeee"
default_bg: "#111111"
force_default: true
palette_map:
  2: "#123456"
foreground:
  - { at: 0.0, color: "#000000" }
  - { at: 1.0, color: "#ffffff" }
background:
  - { at: 0.0, color: "#000000" }
  - { at: 1.0, color: "#ffffff" }
"##,
        )
        .expect("fixture theme should be written");

        let theme = read_theme(&path).expect("theme should parse");
        assert_eq!(
            theme.palette_map.get(&2).map(String::as_str),
            Some("#123456")
        );

        let _ = fs::remove_file(path);
    }

    fn unique_temp_file(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should move forward")
            .as_nanos();
        std::env::temp_dir().join(format!("{name}-{nanos}"))
    }
}
