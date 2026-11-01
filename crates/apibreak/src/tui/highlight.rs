//! Syntax coloring for pretty-printed JSON, where no token spans two lines.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use super::style;

pub fn json(text: &str) -> Vec<Line<'static>> {
    text.lines().map(json_line).collect()
}

fn json_line(line: &str) -> Line<'static> {
    let mut spans = Vec::new();
    let mut rest = line;
    while let Some(first) = rest.chars().next() {
        let (length, token_style) = match first {
            '"' => {
                let length = string_length(rest);
                let is_key = rest[length..].trim_start().starts_with(':');
                let color = if is_key {
                    style::accent()
                } else {
                    Style::new().fg(Color::Green)
                };
                (length, color)
            }
            '{' | '}' | '[' | ']' | ':' | ',' => (1, style::muted()),
            ' ' => (rest.len() - rest.trim_start().len(), Style::new()),
            _ => {
                let end = rest.find([',', ' ', ']', '}']).unwrap_or(rest.len());
                (end.max(first.len_utf8()), Style::new().fg(Color::Yellow))
            }
        };
        let (token, tail) = rest.split_at(length);
        spans.push(Span::styled(token.to_owned(), token_style));
        rest = tail;
    }
    Line::from(spans)
}

/// Byte length of the string starting at `text`, quotes included.
fn string_length(text: &str) -> usize {
    let mut escaped = false;
    for (index, character) in text.char_indices().skip(1) {
        match character {
            '"' if !escaped => return index + 1,
            '\\' if !escaped => escaped = true,
            _ => escaped = false,
        }
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(line: &str) -> Vec<String> {
        json_line(line)
            .spans
            .iter()
            .map(|span| span.content.to_string())
            .collect()
    }

    #[test]
    fn splits_keys_values_and_punctuation() {
        assert_eq!(tokens(r#"  "a": 1,"#), ["  ", "\"a\"", ":", " ", "1", ","]);
    }

    #[test]
    fn keeps_escapes_and_unicode_inside_strings() {
        assert_eq!(tokens(r#""a\"б""#), [r#""a\"б""#]);
    }

    #[test]
    fn keys_and_string_values_differ() {
        let line = json_line(r#""key": "value""#);
        assert_ne!(line.spans[0].style, line.spans[3].style);
    }

    #[test]
    fn every_line_round_trips() {
        let text = "{\n  \"a\": [\n    true,\n    null\n  ]\n}";
        let rebuilt: Vec<String> = json(text).iter().map(ToString::to_string).collect();
        assert_eq!(rebuilt.join("\n"), text);
    }
}
