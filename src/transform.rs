use crate::model::{OutputTransformKind, OutputTransformRule};

pub(crate) fn apply_output_transforms(
    lines: &[String],
    rules: &[OutputTransformRule],
) -> Vec<String> {
    if rules.is_empty() {
        return lines.to_vec();
    }
    lines
        .iter()
        .map(|line| transform_line(line, rules))
        .collect()
}

pub(crate) fn transform_line(line: &str, rules: &[OutputTransformRule]) -> String {
    let mut current = line.to_string();
    for rule in rules {
        let matches = rule
            .regex
            .find_iter(&current)
            .map(|m| (m.start(), m.end(), m.as_str().to_string()))
            .collect::<Vec<_>>();
        if matches.is_empty() {
            continue;
        }

        let mut chars = current.chars().collect::<Vec<_>>();
        for (start, end, matched) in matches {
            let start_idx = byte_to_char_idx(&current, start);
            let end_idx = byte_to_char_idx(&current, end);
            if start_idx >= end_idx || end_idx > chars.len() {
                continue;
            }
            let replacement = width_preserving_text(&matched, &rule.kind);
            for (offset, ch) in replacement.into_iter().enumerate() {
                chars[start_idx + offset] = ch;
            }
        }
        current = chars.into_iter().collect();
    }
    current
}

fn width_preserving_text(matched: &str, kind: &OutputTransformKind) -> Vec<char> {
    let width = matched.chars().count();
    match kind {
        OutputTransformKind::Replace(text) => fit_to_width(text, width),
        OutputTransformKind::Mask(mask_char) => vec![*mask_char; width],
    }
}

fn fit_to_width(text: &str, width: usize) -> Vec<char> {
    let mut chars = text.chars().take(width).collect::<Vec<_>>();
    while chars.len() < width {
        chars.push(' ');
    }
    chars
}

fn byte_to_char_idx(text: &str, byte_idx: usize) -> usize {
    text[..byte_idx.min(text.len())].chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;

    fn replace_rule(pattern: &str, replacement: &str) -> OutputTransformRule {
        OutputTransformRule {
            regex: Regex::new(pattern).expect("regex should compile"),
            kind: OutputTransformKind::Replace(replacement.to_string()),
        }
    }

    fn mask_rule(pattern: &str, mask_char: char) -> OutputTransformRule {
        OutputTransformRule {
            regex: Regex::new(pattern).expect("regex should compile"),
            kind: OutputTransformKind::Mask(mask_char),
        }
    }

    #[test]
    fn replace_rule_pads_to_match_width() {
        let out = transform_line("token=abcdef", &[replace_rule("abcdef", "ok")]);
        assert_eq!(out, "token=ok    ");
    }

    #[test]
    fn replace_rule_truncates_to_match_width() {
        let out = transform_line("token=abc", &[replace_rule("abc", "REDACTED")]);
        assert_eq!(out, "token=RED");
    }

    #[test]
    fn mask_rule_preserves_width() {
        let out = transform_line("secret-1234", &[mask_rule(r"1234", '*')]);
        assert_eq!(out, "secret-****");
    }

    #[test]
    fn multiple_rules_apply_in_order() {
        let out = transform_line(
            "user=alice token=abcdef",
            &[mask_rule("alice", 'x'), replace_rule("abcdef", "ok")],
        );
        assert_eq!(out, "user=xxxxx token=ok    ");
    }
}
