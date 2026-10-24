use std::fmt;

/// A candidate match found by a replacer.
pub struct MatchCandidate {
    pub start: usize,
    pub end: usize,
    pub matched_text: String,
}

/// Error from the replace function.
#[derive(Debug)]
pub enum EditError {
    NotFound,
    Ambiguous { count: usize },
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EditError::NotFound => write!(
                f,
                "Could not find old_string in file. It must match exactly, including whitespace, indentation, and line endings."
            ),
            EditError::Ambiguous { count } => write!(
                f,
                "Found {} matches for old_string. Provide more surrounding context to make the match unique.",
                count
            ),
        }
    }
}

pub trait Replacer: Send + Sync {
    fn find(&self, content: &str, search: &str) -> Vec<MatchCandidate>;
}

// ---------------------------------------------------------------------------
// Levenshtein distance
// ---------------------------------------------------------------------------

fn levenshtein(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let a_len = a_chars.len();
    let b_len = b_chars.len();

    if a_len == 0 {
        return b_len;
    }
    if b_len == 0 {
        return a_len;
    }

    let mut prev_row: Vec<usize> = (0..=b_len).collect();
    let mut curr_row: Vec<usize> = vec![0; b_len + 1];

    for i in 1..=a_len {
        curr_row[0] = i;
        for j in 1..=b_len {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            curr_row[j] = (prev_row[j] + 1)
                .min(curr_row[j - 1] + 1)
                .min(prev_row[j - 1] + cost);
        }
        std::mem::swap(&mut prev_row, &mut curr_row);
    }

    prev_row[b_len]
}

fn levenshtein_similarity(a: &str, b: &str) -> f64 {
    let max_len = a.len().max(b.len());
    if max_len == 0 {
        return 1.0;
    }
    1.0 - (levenshtein(a, b) as f64 / max_len as f64)
}

// ---------------------------------------------------------------------------
// Line ending utilities
// ---------------------------------------------------------------------------

pub fn detect_line_ending(text: &str) -> &'static str {
    if text.contains("\r\n") { "\r\n" } else { "\n" }
}

pub fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n")
}

pub fn convert_line_endings(text: &str, ending: &str) -> String {
    // First normalize to \n, then convert to target.
    let normalized = normalize_line_endings(text);
    if ending == "\r\n" {
        normalized.replace('\n', "\r\n")
    } else {
        normalized
    }
}

// ---------------------------------------------------------------------------
// Helper: count how many times `needle` appears as an exact substring of `haystack`
// ---------------------------------------------------------------------------

fn count_occurrences(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    haystack.matches(needle).count()
}

// ---------------------------------------------------------------------------
// 1. SimpleReplacer — exact substring match
// ---------------------------------------------------------------------------

struct SimpleReplacer;

impl Replacer for SimpleReplacer {
    fn find(&self, content: &str, search: &str) -> Vec<MatchCandidate> {
        let mut candidates = Vec::new();
        let mut start = 0;
        while let Some(offset) = content[start..].find(search) {
            let abs_start = start + offset;
            let abs_end = abs_start + search.len();
            candidates.push(MatchCandidate {
                start: abs_start,
                end: abs_end,
                matched_text: content[abs_start..abs_end].to_string(),
            });
            start = abs_end;
        }
        candidates
    }
}

// ---------------------------------------------------------------------------
// 2. LineTrimmedReplacer — compare lines by trimmed content
// ---------------------------------------------------------------------------

struct LineTrimmedReplacer;

impl Replacer for LineTrimmedReplacer {
    fn find(&self, content: &str, search: &str) -> Vec<MatchCandidate> {
        let content_lines: Vec<&str> = content.lines().collect();
        let search_lines: Vec<&str> = search.lines().collect();

        if search_lines.is_empty() || search_lines.len() > content_lines.len() {
            return Vec::new();
        }

        let search_trimmed: Vec<&str> = search_lines.iter().map(|l| l.trim()).collect();
        let n = search_trimmed.len();
        let mut candidates = Vec::new();

        for window_start in 0..=(content_lines.len() - n) {
            let window = &content_lines[window_start..window_start + n];
            if window
                .iter()
                .zip(search_trimmed.iter())
                .all(|(cl, sl)| cl.trim() == *sl)
            {
                // Compute the byte span in the original content string.
                let (start, end) = line_span(
                    content,
                    window_start,
                    window_start + n - 1,
                    content_lines.as_slice(),
                );
                candidates.push(MatchCandidate {
                    start,
                    end,
                    matched_text: content[start..end].to_string(),
                });
            }
        }
        candidates
    }
}

// ---------------------------------------------------------------------------
// 3. BlockAnchorReplacer — anchor first/last lines, fuzzy middle
// ---------------------------------------------------------------------------

struct BlockAnchorReplacer;

impl Replacer for BlockAnchorReplacer {
    fn find(&self, content: &str, search: &str) -> Vec<MatchCandidate> {
        let content_lines: Vec<&str> = content.lines().collect();
        let search_lines: Vec<&str> = search.lines().collect();

        if search_lines.len() < 3 || search_lines.len() > content_lines.len() {
            return Vec::new();
        }

        let n = search_lines.len();
        let first_trimmed = search_lines[0].trim();
        let last_trimmed = search_lines[n - 1].trim();
        let middle_search: Vec<&str> = search_lines[1..n - 1].iter().map(|l| l.trim()).collect();

        let mut anchor_hits: Vec<(usize, f64, usize, usize)> = Vec::new(); // (window_start, score, start_byte, end_byte)

        for window_start in 0..=(content_lines.len() - n) {
            let window_end = window_start + n - 1;
            if content_lines[window_start].trim() != first_trimmed
                || content_lines[window_end].trim() != last_trimmed
            {
                continue;
            }

            let (start_byte, end_byte) =
                line_span(content, window_start, window_end, &content_lines);
            let middle_content: Vec<&str> = content_lines[window_start + 1..window_end]
                .iter()
                .map(|l| l.trim())
                .collect();

            let mid_text = middle_content.join("\n");
            let search_mid_text = middle_search.join("\n");
            let sim = levenshtein_similarity(&mid_text, &search_mid_text);

            anchor_hits.push((window_start, sim, start_byte, end_byte));
        }

        if anchor_hits.is_empty() {
            return Vec::new();
        }

        if anchor_hits.len() == 1 {
            // Single candidate: always accept (threshold 0.0)
            let (_, _, s, e) = anchor_hits[0];
            return vec![MatchCandidate {
                start: s,
                end: e,
                matched_text: content[s..e].to_string(),
            }];
        }

        // Multiple candidates: pick best with threshold 0.3
        let threshold = 0.3;
        let mut best: Option<(usize, f64, usize, usize)> = None;
        for hit in &anchor_hits {
            if hit.1 >= threshold && (best.is_none() || hit.1 > best.unwrap().1) {
                best = Some(*hit);
            }
        }

        match best {
            Some((_, _, s, e)) => vec![MatchCandidate {
                start: s,
                end: e,
                matched_text: content[s..e].to_string(),
            }],
            None => Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// 4. WhitespaceNormalizedReplacer — collapse whitespace runs to single space
// ---------------------------------------------------------------------------

struct WhitespaceNormalizedReplacer;

fn normalize_whitespace(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut in_ws = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !in_ws {
                result.push(' ');
                in_ws = true;
            }
        } else {
            result.push(ch);
            in_ws = false;
        }
    }
    result
}

impl Replacer for WhitespaceNormalizedReplacer {
    fn find(&self, content: &str, search: &str) -> Vec<MatchCandidate> {
        let norm_content = normalize_whitespace(content);
        let norm_search = normalize_whitespace(search);

        if norm_search.is_empty() {
            return Vec::new();
        }

        let mut candidates = Vec::new();
        let mut start = 0;
        while let Some(offset) = norm_content[start..].find(&norm_search) {
            let abs_start = start + offset;
            let abs_end = abs_start + norm_search.len();

            // Map normalized positions back to original positions.
            let orig_start = normalized_to_original_pos(content, abs_start);
            let orig_end = normalized_to_original_pos(content, abs_end);

            candidates.push(MatchCandidate {
                start: orig_start,
                end: orig_end,
                matched_text: content[orig_start..orig_end].to_string(),
            });
            start = abs_end;
        }
        candidates
    }
}

/// Maps a position in the whitespace-normalized version of `original` back to
/// the corresponding byte position in `original`.
fn normalized_to_original_pos(original: &str, norm_pos: usize) -> usize {
    let mut norm_idx = 0;
    let mut orig_idx = 0;
    let mut in_ws = false;
    let bytes = original.as_bytes();
    let len = bytes.len();

    while orig_idx < len && norm_idx < norm_pos {
        let ch = bytes[orig_idx];
        if ch.is_ascii_whitespace() {
            if !in_ws {
                norm_idx += 1; // the single space we emitted
                in_ws = true;
            }
            // Skip consecutive whitespace bytes (only handle ASCII whitespace for simplicity)
            orig_idx += 1;
        } else {
            norm_idx += 1;
            orig_idx += 1;
            in_ws = false;
        }
    }

    // Advance past any remaining whitespace that was collapsed into the last space
    while orig_idx < len && bytes[orig_idx].is_ascii_whitespace() && in_ws {
        orig_idx += 1;
    }

    orig_idx
}

// ---------------------------------------------------------------------------
// 5. IndentationFlexibleReplacer — strip minimum common leading whitespace
// ---------------------------------------------------------------------------

struct IndentationFlexibleReplacer;

/// Returns the minimum leading whitespace (spaces/tabs) across all non-empty
/// lines in `lines`.
fn min_indent(lines: &[&str]) -> usize {
    let mut min = usize::MAX;
    for line in lines {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        let indent = line.len() - trimmed.len();
        if indent < min {
            min = indent;
        }
    }
    if min == usize::MAX { 0 } else { min }
}

fn strip_indent(lines: &[&str], n: usize) -> Vec<String> {
    lines
        .iter()
        .map(|l| {
            if l.len() >= n && l[..n].chars().all(|c| c.is_whitespace()) {
                l[n..].to_string()
            } else {
                l.trim_start().to_string()
            }
        })
        .collect()
}

impl Replacer for IndentationFlexibleReplacer {
    fn find(&self, content: &str, search: &str) -> Vec<MatchCandidate> {
        let content_lines: Vec<&str> = content.lines().collect();
        let search_lines: Vec<&str> = search.lines().collect();

        if search_lines.is_empty() || search_lines.len() > content_lines.len() {
            return Vec::new();
        }

        let n = search_lines.len();
        let search_min = min_indent(&search_lines);
        let search_stripped = strip_indent(&search_lines, search_min);

        let mut candidates = Vec::new();

        for window_start in 0..=(content_lines.len() - n) {
            let window = &content_lines[window_start..window_start + n];
            let window_min = min_indent(window);
            let window_stripped = strip_indent(window, window_min);

            if window_stripped == search_stripped {
                let (s, e) = line_span(content, window_start, window_start + n - 1, &content_lines);
                candidates.push(MatchCandidate {
                    start: s,
                    end: e,
                    matched_text: content[s..e].to_string(),
                });
            }
        }
        candidates
    }
}

// ---------------------------------------------------------------------------
// 6. EscapeNormalizedReplacer — unescape common escape sequences in search
// ---------------------------------------------------------------------------

struct EscapeNormalizedReplacer;

fn unescape_search(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.peek() {
                Some('n') => {
                    chars.next();
                    result.push('\n');
                }
                Some('t') => {
                    chars.next();
                    result.push('\t');
                }
                Some('r') => {
                    chars.next();
                    result.push('\r');
                }
                Some('"') => {
                    chars.next();
                    result.push('"');
                }
                Some('\'') => {
                    chars.next();
                    result.push('\'');
                }
                Some('`') => {
                    chars.next();
                    result.push('`');
                }
                Some('\\') => {
                    chars.next();
                    result.push('\\');
                }
                _ => result.push(ch),
            }
        } else {
            result.push(ch);
        }
    }
    result
}

impl Replacer for EscapeNormalizedReplacer {
    fn find(&self, content: &str, search: &str) -> Vec<MatchCandidate> {
        let unescaped = unescape_search(search);
        if unescaped == search {
            // No escape sequences found — nothing extra to try.
            return Vec::new();
        }
        SimpleReplacer.find(content, &unescaped)
    }
}

// ---------------------------------------------------------------------------
// 7. TrimmedBoundaryReplacer — trim leading/trailing whitespace from search
// ---------------------------------------------------------------------------

struct TrimmedBoundaryReplacer;

impl Replacer for TrimmedBoundaryReplacer {
    fn find(&self, content: &str, search: &str) -> Vec<MatchCandidate> {
        let trimmed = search.trim();
        if trimmed == search || trimmed.is_empty() {
            return Vec::new();
        }
        SimpleReplacer.find(content, trimmed)
    }
}

// ---------------------------------------------------------------------------
// 8. ContextAwareReplacer — first/last anchor + 50% middle exact match
// ---------------------------------------------------------------------------

struct ContextAwareReplacer;

impl Replacer for ContextAwareReplacer {
    fn find(&self, content: &str, search: &str) -> Vec<MatchCandidate> {
        let content_lines: Vec<&str> = content.lines().collect();
        let search_lines: Vec<&str> = search.lines().collect();

        if search_lines.len() < 3 || search_lines.len() > content_lines.len() {
            return Vec::new();
        }

        let n = search_lines.len();
        let first_trimmed = search_lines[0].trim();
        let last_trimmed = search_lines[n - 1].trim();

        let mut candidates = Vec::new();

        for window_start in 0..=(content_lines.len() - n) {
            let window_end = window_start + n - 1;
            if content_lines[window_start].trim() != first_trimmed
                || content_lines[window_end].trim() != last_trimmed
            {
                continue;
            }

            // Check middle lines: at least 50% must match exactly (trimmed).
            let middle_count = n - 2;
            let required = (middle_count as f64 * 0.5).ceil() as usize;
            let middle_match_count = search_lines[1..n - 1]
                .iter()
                .zip(content_lines[window_start + 1..window_end].iter())
                .filter(|(sl, cl)| sl.trim() == cl.trim())
                .count();

            if middle_match_count >= required {
                let (s, e) = line_span(content, window_start, window_end, &content_lines);
                candidates.push(MatchCandidate {
                    start: s,
                    end: e,
                    matched_text: content[s..e].to_string(),
                });
            }
        }
        candidates
    }
}

// ---------------------------------------------------------------------------
// 9. MultiOccurrenceReplacer — find ALL exact occurrences (used by replace_all)
// ---------------------------------------------------------------------------

struct MultiOccurrenceReplacer;

impl Replacer for MultiOccurrenceReplacer {
    fn find(&self, content: &str, search: &str) -> Vec<MatchCandidate> {
        SimpleReplacer.find(content, search)
    }
}

// ---------------------------------------------------------------------------
// Helper: compute byte span for lines [start_line..end_line] inclusive
// ---------------------------------------------------------------------------

fn line_span(content: &str, start_line: usize, end_line: usize, lines: &[&str]) -> (usize, usize) {
    let mut byte_pos = 0;
    let mut start_byte = 0;
    let mut end_byte = 0;

    for (i, line) in lines.iter().enumerate() {
        if i == start_line {
            start_byte = byte_pos;
        }
        byte_pos += line.len();
        if i == end_line {
            end_byte = byte_pos;
            break;
        }
        // Account for the line ending after this line.
        // We need to figure out if there's a \n or \r\n at this position.
        if byte_pos < content.len() {
            if content.as_bytes().get(byte_pos) == Some(&b'\r')
                && content.as_bytes().get(byte_pos + 1) == Some(&b'\n')
            {
                byte_pos += 2; // \r\n
            } else {
                byte_pos += 1; // \n (or the separator between lines)
            }
        }
    }

    (start_byte, end_byte)
}

// ---------------------------------------------------------------------------
// Main replace function
// ---------------------------------------------------------------------------

/// Try to replace `old` with `new` in `content`, using the 9-replacer cascade.
/// If `replace_all` is true, replace all occurrences. Otherwise, require a unique match.
pub fn replace(
    content: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<String, EditError> {
    if old == new {
        return Ok(content.to_string());
    }

    if old.is_empty() {
        return Err(EditError::NotFound);
    }

    if replace_all {
        let multi = MultiOccurrenceReplacer;
        let candidates = multi.find(content, old);
        if candidates.is_empty() {
            return Err(EditError::NotFound);
        }
        // Replace from right to left to preserve earlier indices.
        let mut result = content.to_string();
        for c in candidates.iter().rev() {
            result = format!("{}{}{}", &result[..c.start], new, &result[c.end..]);
        }
        return Ok(result);
    }

    let replacers: Vec<Box<dyn Replacer>> = vec![
        Box::new(SimpleReplacer),
        Box::new(LineTrimmedReplacer),
        Box::new(BlockAnchorReplacer),
        Box::new(WhitespaceNormalizedReplacer),
        Box::new(IndentationFlexibleReplacer),
        Box::new(EscapeNormalizedReplacer),
        Box::new(TrimmedBoundaryReplacer),
        Box::new(ContextAwareReplacer),
    ];

    let mut any_found = false;
    let mut total_count = 0;

    for replacer in &replacers {
        let candidates = replacer.find(content, old);
        if candidates.is_empty() {
            continue;
        }

        // Filter to candidates whose matched_text appears exactly once in content.
        let unique: Vec<&MatchCandidate> = candidates
            .iter()
            .filter(|c| count_occurrences(content, &c.matched_text) == 1)
            .collect();

        if unique.len() == 1 {
            let c = unique[0];
            tracing::debug!(
                replacer = std::any::type_name_of_val(&**replacer),
                start = c.start,
                end = c.end,
                "Found unique fuzzy match"
            );
            let result = format!("{}{}{}", &content[..c.start], new, &content[c.end..]);
            return Ok(result);
        }

        any_found = true;
        total_count = total_count.max(candidates.len());
    }

    if any_found {
        return Err(EditError::Ambiguous { count: total_count });
    }

    Err(EditError::NotFound)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_match() {
        let content = "hello world\nfoo bar\n";
        let result = replace(content, "foo bar", "baz qux", false).unwrap();
        assert_eq!(result, "hello world\nbaz qux\n");
    }

    #[test]
    fn test_no_match() {
        let content = "hello world\nfoo bar\n";
        let err = replace(content, "nonexistent", "something", false).unwrap_err();
        assert!(matches!(err, EditError::NotFound));
    }

    #[test]
    fn test_ambiguous_match() {
        let content = "foo\nfoo\n";
        let err = replace(content, "foo", "bar", false).unwrap_err();
        assert!(matches!(err, EditError::Ambiguous { count: 2 }));
    }

    #[test]
    fn test_line_trimmed_match() {
        let content = "  hello world  \n  foo bar  \n";
        let result = replace(content, "hello world\nfoo bar", "replaced", false).unwrap();
        assert!(result.contains("replaced"));
    }

    #[test]
    fn test_block_anchor_single_candidate() {
        let content = "fn main() {\n    println!(\"hello\");\n}\nother line\n";
        let search = "fn main() {\n    println!(\"world\");\n}";
        let result = replace(content, search, "replaced", false).unwrap();
        assert!(result.contains("replaced"));
        assert!(!result.contains("println!(\"hello\")"));
    }

    #[test]
    fn test_block_anchor_multiple_candidates() {
        // Two similar blocks; the one with closer middle should win.
        let content = "fn a() {\n    x + 1\n}\nfn b() {\n    x + 2\n}\n";
        let search = "fn a() {\n    x + 1\n}";
        let result = replace(content, search, "REPLACED", false).unwrap();
        assert_eq!(result, "REPLACED\nfn b() {\n    x + 2\n}\n");
    }

    #[test]
    fn test_whitespace_normalized() {
        let content = "hello   world   foo";
        let result = replace(content, "hello world foo", "replaced", false).unwrap();
        assert_eq!(result, "replaced");
    }

    #[test]
    fn test_indentation_flexible() {
        let content = "    hello\n    world\n";
        let result = replace(content, "hello\nworld", "replaced", false).unwrap();
        assert_eq!(result, "replaced\n");
    }

    #[test]
    fn test_escape_normalized() {
        let content = "hello\nworld\n";
        let result = replace(content, "hello\\nworld", "replaced", false).unwrap();
        assert_eq!(result, "replaced\n");
    }

    #[test]
    fn test_trimmed_boundary() {
        let content = "hello world foo bar";
        let result = replace(content, "  hello world foo bar  ", "replaced", false).unwrap();
        assert_eq!(result, "replaced");
    }

    #[test]
    fn test_context_aware() {
        let content = "start\n  a\n  b\n  c\nend\nmiddle\nstart\n  x\n  y\n  z\nend\n";
        let search = "start\n  a\n  b\n  c\nend";
        let result = replace(content, search, "REPLACED", false).unwrap();
        assert!(result.starts_with("REPLACED\nmiddle"));
    }

    #[test]
    fn test_replace_all() {
        let content = "foo bar foo baz foo";
        let result = replace(content, "foo", "qux", true).unwrap();
        assert_eq!(result, "qux bar qux baz qux");
    }

    #[test]
    fn test_replace_all_no_match() {
        let content = "hello world";
        let err = replace(content, "nonexistent", "something", true).unwrap_err();
        assert!(matches!(err, EditError::NotFound));
    }

    #[test]
    fn test_line_ending_detection() {
        assert_eq!(detect_line_ending("hello\r\nworld"), "\r\n");
        assert_eq!(detect_line_ending("hello\nworld"), "\n");
    }

    #[test]
    fn test_line_ending_conversion() {
        assert_eq!(
            convert_line_endings("hello\nworld\n", "\r\n"),
            "hello\r\nworld\r\n"
        );
        assert_eq!(
            convert_line_endings("hello\r\nworld\r\n", "\n"),
            "hello\nworld\n"
        );
    }

    #[test]
    fn test_old_equals_new() {
        let content = "hello world";
        let result = replace(content, "hello", "hello", false).unwrap();
        assert_eq!(result, content);
    }

    #[test]
    fn test_levenshtein_distance() {
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("abc", "abc"), 0);
    }

    #[test]
    fn test_levenshtein_similarity_values() {
        assert!((levenshtein_similarity("abc", "abc") - 1.0).abs() < f64::EPSILON);
        assert!((levenshtein_similarity("", "") - 1.0).abs() < f64::EPSILON);
        assert!((levenshtein_similarity("abc", "") - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_normalize_whitespace() {
        assert_eq!(normalize_whitespace("  hello   world  "), " hello world ");
    }

    #[test]
    fn test_simple_replacer_multiple() {
        let candidates = SimpleReplacer.find("abcabc", "abc");
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].start, 0);
        assert_eq!(candidates[0].end, 3);
        assert_eq!(candidates[1].start, 3);
        assert_eq!(candidates[1].end, 6);
    }

    #[test]
    fn test_unescape_search() {
        assert_eq!(unescape_search("hello\\nworld"), "hello\nworld");
        assert_eq!(unescape_search("tab\\there"), "tab\there");
        assert_eq!(unescape_search("back\\\\slash"), "back\\slash");
        assert_eq!(unescape_search("quote\\\"here"), "quote\"here");
    }

    #[test]
    fn test_empty_search_returns_not_found() {
        let content = "hello";
        let err = replace(content, "", "something", false).unwrap_err();
        assert!(matches!(err, EditError::NotFound));
    }

    #[test]
    fn test_normalize_line_endings() {
        assert_eq!(normalize_line_endings("a\r\nb\r\n"), "a\nb\n");
        assert_eq!(normalize_line_endings("a\nb\n"), "a\nb\n");
    }

    #[test]
    fn test_levenshtein_multibyte_utf8() {
        // "café" (with é as multi-byte) vs "cafe" — should be distance 1
        let dist = levenshtein("café", "cafe");
        assert_eq!(dist, 1);

        // Turkish İ (multi-byte) vs I — should be distance 1
        let dist = levenshtein("İstanbul", "Istanbul");
        assert_eq!(dist, 1);
    }
}
