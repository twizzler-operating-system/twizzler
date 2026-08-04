pub const MAX_TOOL_OUTPUT: usize = 4000;
pub const MAX_HISTORY: usize = 12000;

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

pub fn clip(text: impl ToString, limit: usize) -> String {
    let text = text.to_string();
    let len = text.chars().count();
    if len <= limit {
        return text.to_string();
    }
    let clipped: String = text.chars().take(limit).collect();
    format!("{clipped}\n...[truncated {} chars]", len - limit)
}

/// Removes the middle of a long piece of text and adds "..." to represent
/// concatenation
pub fn middle(text: impl ToString, limit: usize) -> String {
    let text = text.to_string().replace('\n', " ");
    let len = text.chars().count();

    if len <= limit {
        return text;
    }

    if limit <= 3 {
        return text.chars().take(limit).collect();
    }

    let left = (limit - 3) / 2;
    let right = limit - 3 - left;
    let start: String = text.chars().take(left).collect();
    let end: String = text
        .chars()
        .rev()
        .take(right)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    format!("{start}...{end}")
}

#[cfg(test)]
mod tests {
    use super::middle;

    #[test]
    fn returns_short_text_unchanged() {
        assert_eq!(middle("hello", 5), "hello");
    }

    #[test]
    fn replaces_newlines_with_spaces() {
        assert_eq!(middle("hello\nworld", 20), "hello world");
    }

    #[test]
    fn truncates_to_limit_when_limit_is_three_or_less() {
        assert_eq!(middle("abcdef", 3), "abc");
        assert_eq!(middle("abcdef", 2), "ab");
    }

    #[test]
    fn preserves_start_and_end_with_ellipsis_in_middle() {
        assert_eq!(middle("abcdefghijklmnopqrstuvwxyz", 10), "abc...wxyz");
    }
}
