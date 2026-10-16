use std::path::{Component, Path, PathBuf};

pub const DISPLAY_TEXT_MAX_CHARS: usize = 240;

pub fn clean_text(input: &str) -> String {
    terminal_safe(input)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn bounded_display_text(input: &str) -> String {
    let text = clean_text(input);
    match text.char_indices().nth(DISPLAY_TEXT_MAX_CHARS) {
        Some((cut, _)) => format!("{}…", text[..cut].trim_end()),
        None => text,
    }
}

pub fn terminal_safe(input: &str) -> String {
    let mut without_controls = String::with_capacity(input.len());
    let mut characters = input.chars().peekable();
    while let Some(character) = characters.next() {
        if matches!(character, '\u{1b}' | '\u{9b}') {
            if character == '\u{9b}' {
                for next in characters.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
                continue;
            }
            match characters.peek().copied() {
                Some('[') => {
                    characters.next();
                    for next in characters.by_ref() {
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    characters.next();
                    let mut previous_escape = false;
                    for next in characters.by_ref() {
                        if next == '\u{7}' || (previous_escape && next == '\\') {
                            break;
                        }
                        previous_escape = next == '\u{1b}';
                    }
                }
                Some(_) => {
                    characters.next();
                }
                None => {}
            }
        } else if character == '\u{9d}' {
            let mut previous_escape = false;
            for next in characters.by_ref() {
                if next == '\u{7}' || (previous_escape && next == '\\') {
                    break;
                }
                previous_escape = next == '\u{1b}';
            }
        } else if matches!(character, '\n' | '\r' | '\t') {
            without_controls.push(' ');
        } else if !character.is_control() && !is_format_control(character) {
            without_controls.push(character);
        }
    }
    without_controls
}

fn is_format_control(character: char) -> bool {
    matches!(
        character,
        '\u{061c}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206f}'
            | '\u{feff}'
    )
}

pub fn normalize_path(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

pub fn parse_timestamp(value: &serde_json::Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|stamp| i64::try_from(stamp).ok()))
        .map(|stamp| {
            if stamp > 100_000_000_000_000_000 {
                stamp / 1_000_000_000
            } else if stamp > 100_000_000_000_000 {
                stamp / 1_000_000
            } else if stamp > 10_000_000_000 {
                stamp / 1000
            } else {
                stamp
            }
        })
        .or_else(|| {
            value.as_str().and_then(|stamp| {
                chrono::DateTime::parse_from_rfc3339(stamp)
                    .ok()
                    .map(|date| date.timestamp())
            })
        })
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

pub fn query_tokens(query: &str) -> Vec<String> {
    query
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .take(32)
        .map(str::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_controls_and_collapses_whitespace() {
        assert_eq!(clean_text(" a\x1b[31mred\x1b[0m\n b\t"), "ared b");
        assert_eq!(clean_text("x\x1b]0;private\x07y"), "xy");
        assert_eq!(clean_text("a\u{202e}b\u{9b}31mc"), "abc");
    }

    #[test]
    fn display_text_cap_counts_unicode_scalars() {
        let input = format!("{}tail", "界".repeat(DISPLAY_TEXT_MAX_CHARS));
        let bounded = bounded_display_text(&input);
        assert_eq!(bounded, format!("{}…", "界".repeat(DISPLAY_TEXT_MAX_CHARS)));
        assert_eq!(bounded.chars().count(), DISPLAY_TEXT_MAX_CHARS + 1);
        assert_eq!(bounded.len(), DISPLAY_TEXT_MAX_CHARS * 3 + 3);
    }

    #[test]
    fn normalizes_lexical_paths() {
        assert!(normalize_path(Path::new("one/../two")).ends_with("two"));
    }
}
