use crate::model::KeymapFile;
use anyhow::{anyhow, Result};
use std::{collections::HashMap, fs, path::Path};

pub(crate) fn read_keymap(path: &Path) -> Result<KeymapFile> {
    let s = fs::read_to_string(path)?;
    Ok(serde_yaml::from_str(&s)?)
}

pub(crate) struct KeyMapper {
    rules: Vec<(Vec<u8>, Vec<u8>)>,
    pending: Vec<u8>,
    has_escape_rules: bool,
}

impl KeyMapper {
    pub(crate) fn new(map: HashMap<Vec<u8>, Vec<u8>>) -> Self {
        let mut rules: Vec<_> = map.into_iter().collect();
        rules.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));
        let has_escape_rules = rules.iter().any(|(from, _)| from.starts_with(b"\x1b"));
        Self {
            rules,
            pending: Vec::new(),
            has_escape_rules,
        }
    }

    pub(crate) fn push_bytes(&mut self, input: &[u8]) -> Vec<u8> {
        if !self.has_escape_rules {
            if self.pending == b"\x1b" {
                self.pending.extend_from_slice(input);
                return self.drain_ready(true);
            }
            if input == b"\x1b" {
                let mut out = self.drain_ready(true);
                self.pending.extend_from_slice(input);
                out.shrink_to_fit();
                return out;
            }
            if input.starts_with(b"\x1b") {
                let mut out = self.drain_ready(true);
                out.extend_from_slice(input);
                return out;
            }
        }
        self.pending.extend_from_slice(input);
        self.drain_ready(false)
    }

    pub(crate) fn flush_pending(&mut self) -> Vec<u8> {
        self.drain_ready(true)
    }

    fn drain_ready(&mut self, flush_ambiguous: bool) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pending.len());
        while !self.pending.is_empty() {
            let exact = self
                .rules
                .iter()
                .find(|(from, _)| self.pending.starts_with(from))
                .cloned();

            let has_longer_prefix = self.rules.iter().any(|(from, _)| {
                from.len() > self.pending.len() && from.starts_with(&self.pending)
            });
            if let Some((from, to)) = exact {
                if has_longer_prefix && !flush_ambiguous {
                    break;
                }
                out.extend_from_slice(&to);
                self.pending.drain(..from.len());
                continue;
            }

            let has_partial_prefix = self.rules.iter().any(|(from, _)| {
                from.len() > self.pending.len() && from.starts_with(&self.pending)
            });
            if has_partial_prefix && !flush_ambiguous {
                break;
            }

            out.push(self.pending[0]);
            self.pending.drain(..1);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_keymap_supports_control_and_function_keys() {
        let map = HashMap::from([
            ("ctrl-j".to_string(), "down".to_string()),
            ("f5".to_string(), "r".to_string()),
        ]);

        let compiled = compile_keymap(&map).expect("keymap should compile");

        assert_eq!(compiled.get(&vec![10]).cloned(), Some(b"\x1b[B".to_vec()));
        assert_eq!(
            compiled.get(b"\x1b[15~".as_slice()).cloned(),
            Some(b"r".to_vec())
        );
    }

    #[test]
    fn key_mapper_prefers_longest_match() {
        let mut mapper = KeyMapper::new(HashMap::from([
            (b"\x1b".to_vec(), b"E".to_vec()),
            (b"\x1b[A".to_vec(), b"UP".to_vec()),
        ]));

        let mapped = mapper.push_bytes(b"\x1b[A");

        assert_eq!(mapped, b"UP".to_vec());
    }

    #[test]
    fn key_mapper_waits_for_split_escape_sequence() {
        let mut mapper = KeyMapper::new(HashMap::from([(b"\x1b[A".to_vec(), b"UP".to_vec())]));

        let first = mapper.push_bytes(b"\x1b");
        let second = mapper.push_bytes(b"[A");

        assert!(first.is_empty());
        assert_eq!(second, b"UP".to_vec());
    }

    #[test]
    fn key_mapper_flushes_ambiguous_escape_as_shorter_match() {
        let mut mapper = KeyMapper::new(HashMap::from([
            (b"\x1b".to_vec(), b"ESC".to_vec()),
            (b"\x1b[A".to_vec(), b"UP".to_vec()),
        ]));

        let partial = mapper.push_bytes(b"\x1b");
        let flushed = mapper.flush_pending();

        assert!(partial.is_empty());
        assert_eq!(flushed, b"ESC".to_vec());
    }

    #[test]
    fn key_mapper_passthroughs_escape_sequences_without_escape_rules() {
        let mut mapper = KeyMapper::new(HashMap::from([(b"j".to_vec(), b"\x1b[B".to_vec())]));

        let mapped = mapper.push_bytes(b"\x1b[A");

        assert_eq!(mapped, b"\x1b[A".to_vec());
    }

    #[test]
    fn key_mapper_flushes_pending_before_passthrough_escape_sequence() {
        let mut mapper = KeyMapper::new(HashMap::from([(b"jj".to_vec(), b"X".to_vec())]));

        let first = mapper.push_bytes(b"j");
        let second = mapper.push_bytes(b"\x1b[A");

        assert!(first.is_empty());
        assert_eq!(second, b"j\x1b[A".to_vec());
    }

    #[test]
    fn key_mapper_passthroughs_split_escape_sequences_without_escape_rules() {
        let mut mapper = KeyMapper::new(HashMap::from([(b"j".to_vec(), b"\x1b[B".to_vec())]));

        let first = mapper.push_bytes(b"\x1b");
        let second = mapper.push_bytes(b"OA");

        assert!(first.is_empty());
        assert_eq!(second, b"\x1bOA".to_vec());
    }

    #[test]
    fn compile_keymap_rejects_unsupported_key() {
        let map = HashMap::from([("meta-x".to_string(), "down".to_string())]);

        let result = compile_keymap(&map);

        assert!(result.is_err());
    }
}
