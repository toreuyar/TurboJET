/// Derive a URL-safe slug from a task title.
/// - Lowercase
/// - Replace non-alphanumeric characters with `-`
/// - Collapse consecutive `-` into single `-`
/// - Truncate to 40 characters
/// - Strip leading/trailing `-`
/// - If result is empty, return "task"
pub fn derive_slug(title: &str) -> String {
    let lower = title.to_lowercase();
    let mut slug = String::with_capacity(lower.len());
    let mut prev_hyphen = false;

    for ch in lower.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            prev_hyphen = false;
        } else if !prev_hyphen {
            slug.push('-');
            prev_hyphen = true;
        }
    }

    slug.truncate(40);
    let result = slug.trim_matches('-');
    if result.is_empty() {
        "task".to_string()
    } else {
        result.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_title() {
        assert_eq!(
            derive_slug("Fix authentication timeout"),
            "fix-authentication-timeout"
        );
    }

    #[test]
    fn long_title_truncated() {
        assert_eq!(
            derive_slug("Add integration tests for ADK runner with Docker support"),
            "add-integration-tests-for-adk-runner-wit"
        );
    }

    #[test]
    fn empty_string_fallback() {
        assert_eq!(derive_slug(""), "task");
    }

    #[test]
    fn extra_whitespace() {
        assert_eq!(derive_slug("  hello  world  "), "hello-world");
    }

    #[test]
    fn special_characters() {
        assert_eq!(derive_slug("Hello!!!World???"), "hello-world");
    }

    #[test]
    fn long_single_char_truncated() {
        let input = "a".repeat(50);
        let slug = derive_slug(&input);
        assert_eq!(slug.len(), 40);
        assert_eq!(slug, "a".repeat(40));
    }

    #[test]
    fn only_special_chars_fallback() {
        assert_eq!(derive_slug("---"), "task");
    }

    #[test]
    fn non_ascii_chars_become_hyphens() {
        assert_eq!(derive_slug("Fix café résumé"), "fix-caf-r-sum");
    }

    #[test]
    fn mixed_special_chars() {
        assert_eq!(
            derive_slug("task 123: fix bug #456"),
            "task-123-fix-bug-456"
        );
    }
}
