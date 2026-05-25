//! Derive a URL-safe slug from an agent's display name.

/// Lowercase, non-alphanumeric runs collapse to a single `-`, leading/trailing
/// `-` trimmed. Empty input (or all-punctuation) yields `"agent"`.
pub fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "agent".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_slugs() {
        assert_eq!(slugify("Strict Code Reviewer"), "strict-code-reviewer");
        assert_eq!(slugify("  Hello!! World  "), "hello-world");
        assert_eq!(slugify("Café 2.0"), "caf-2-0");
        assert_eq!(slugify("!!!"), "agent");
        assert_eq!(slugify(""), "agent");
    }
}
