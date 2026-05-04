use crate::model::KeymapFile;
use anyhow::{anyhow, Result};
use std::{collections::HashMap, fs, path::Path};

pub(crate) fn read_keymap(path: &Path) -> Result<KeymapFile> {
    let s = fs::read_to_string(path)?;
    Ok(serde_yaml::from_str(&s)?)
}

pub(crate) struct KeyMapper {
    rules: Vec<(Vec<u8>, Vec<u8>)>,
}

impl KeyMapper {
    pub(crate) fn new(map: HashMap<Vec<u8>, Vec<u8>>) -> Self {
        let mut rules: Vec<_> = map.into_iter().collect();
        rules.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));
        Self { rules }
    }

    pub(crate) fn map_bytes(&self, input: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(input.len());
        let mut i = 0;
        while i < input.len() {
            let mut matched = false;
            for (from, to) in &self.rules {
                if input[i..].starts_with(from) {
                    out.extend_from_slice(to);
                    i += from.len();
                    matched = true;
                    break;
                }
            }
            if !matched {
                out.push(input[i]);
                i += 1;
            }
        }
        out
    }
}

pub(crate) fn compile_keymap(map: &HashMap<String, String>) -> Result<HashMap<Vec<u8>, Vec<u8>>> {
    let mut out = HashMap::new();
    for (k, v) in map {
        out.insert(key_to_bytes(k)?, key_to_bytes(v)?);
    }
    Ok(out)
}

fn key_to_bytes(s: &str) -> Result<Vec<u8>> {
    let lower = s.to_ascii_lowercase();
    let bytes = match lower.as_str() {
        "up" => b"\x1b[A".to_vec(),
        "down" => b"\x1b[B".to_vec(),
        "right" => b"\x1b[C".to_vec(),
        "left" => b"\x1b[D".to_vec(),
        "home" => b"\x1b[H".to_vec(),
        "end" => b"\x1b[F".to_vec(),
        "page-up" => b"\x1b[5~".to_vec(),
        "page-down" => b"\x1b[6~".to_vec(),
        "enter" => b"\r".to_vec(),
        "esc" => b"\x1b".to_vec(),
        "tab" => b"\t".to_vec(),
        "backspace" => vec![0x7f],
        _ if lower.starts_with("ctrl-") && lower.len() == 6 => {
            let c = lower.as_bytes()[5];
            if c.is_ascii_lowercase() {
                vec![c - b'a' + 1]
            } else {
                return Err(anyhow!("unsupported control key: {s}"));
            }
        }
        _ if lower.starts_with('f') => {
            let n: u8 = lower[1..]
                .parse()
                .map_err(|_| anyhow!("invalid function key: {s}"))?;
            match n {
                1 => b"\x1bOP".to_vec(),
                2 => b"\x1bOQ".to_vec(),
                3 => b"\x1bOR".to_vec(),
                4 => b"\x1bOS".to_vec(),
                5 => b"\x1b[15~".to_vec(),
                6 => b"\x1b[17~".to_vec(),
                7 => b"\x1b[18~".to_vec(),
                8 => b"\x1b[19~".to_vec(),
                9 => b"\x1b[20~".to_vec(),
                10 => b"\x1b[21~".to_vec(),
                _ => return Err(anyhow!("unsupported function key: {s}")),
            }
        }
        _ if s.starts_with("\\x") => parse_hex_bytes(s)?,
        _ if s.chars().count() == 1 => s.as_bytes().to_vec(),
        _ => return Err(anyhow!("unsupported key syntax: {s}")),
    };
    Ok(bytes)
}

fn parse_hex_bytes(s: &str) -> Result<Vec<u8>> {
    let hex = s.trim_start_matches("\\x").replace("\\x", "");
    if !hex.len().is_multiple_of(2) {
        return Err(anyhow!("invalid hex byte string: {s}"));
    }
    let mut out = Vec::new();
    for i in (0..hex.len()).step_by(2) {
        let byte = u8::from_str_radix(&hex[i..i + 2], 16)
            .map_err(|_| anyhow!("invalid hex byte string: {s}"))?;
        out.push(byte);
    }
    Ok(out)
}
