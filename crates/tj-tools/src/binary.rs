/// Binary file extensions — always reject these without reading content.
const BINARY_EXTENSIONS: &[&str] = &[
    ".zip", ".tar", ".gz", ".exe", ".dll", ".so", ".class", ".jar", ".war", ".7z", ".doc", ".docx",
    ".xls", ".xlsx", ".ppt", ".pptx", ".odt", ".ods", ".odp", ".bin", ".dat", ".obj", ".o", ".a",
    ".lib", ".wasm", ".pyc", ".pyo",
];

/// Image/media extensions — also binary.
const MEDIA_EXTENSIONS: &[&str] = &[
    ".png", ".jpg", ".jpeg", ".gif", ".webp", ".pdf", ".ico", ".mp3", ".mp4", ".avi", ".mov",
    ".flac", ".ogg", ".wav",
];

/// Detect whether a file is binary based on extension and/or content sample.
///
/// - `ext`: file extension including dot (e.g., ".zip", ".rs")
/// - `sample`: first N bytes of the file (recommend 4096)
///
/// Returns true if the file is binary and should not be read as text.
pub fn is_binary(ext: &str, sample: &[u8]) -> bool {
    let ext_lower = ext.to_ascii_lowercase();

    if BINARY_EXTENSIONS.contains(&ext_lower.as_str())
        || MEDIA_EXTENSIONS.contains(&ext_lower.as_str())
    {
        return true;
    }

    if sample.is_empty() {
        return false;
    }

    if sample.contains(&0x00) {
        return true;
    }

    let non_printable_count = sample
        .iter()
        .filter(|&&b| matches!(b, 0x00..=0x08 | 0x0E..=0x1F))
        .count();

    let ratio = non_printable_count as f64 / sample.len() as f64;
    ratio > 0.3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_binary_extension() {
        assert!(is_binary(".zip", b"anything"));
        assert!(is_binary(".exe", b"anything"));
        assert!(is_binary(".dll", b"anything"));
    }

    #[test]
    fn test_media_extension() {
        assert!(is_binary(".png", b"anything"));
        assert!(is_binary(".jpg", b"anything"));
        assert!(is_binary(".pdf", b"anything"));
    }

    #[test]
    fn test_null_byte_detection() {
        let sample = b"hello\x00world";
        assert!(is_binary(".txt", sample));
    }

    #[test]
    fn test_nonprintable_ratio() {
        // >30% non-printable bytes, no nulls, no known extension
        let mut sample = vec![0u8; 1024];
        for (i, b) in sample.iter_mut().enumerate() {
            *b = if i % 3 == 0 { 0x12 } else { b'A' };
        }
        // ~33% non-printable (0x12 is in 0x0E..=0x1F range)
        assert!(is_binary(".dat_unknown", &sample));
    }

    #[test]
    fn test_empty_sample() {
        assert!(!is_binary(".txt", &[]));
    }

    #[test]
    fn test_utf8_text() {
        let text = "Hello, world! This is a normal text file with some content.";
        assert!(!is_binary(".txt", text.as_bytes()));
    }

    #[test]
    fn test_mixed_content() {
        // Mostly non-printable bytes — should be detected as binary
        let sample: Vec<u8> = (0..100).map(|i| if i < 50 { 0x15 } else { b'x' }).collect();
        assert!(is_binary(".unknown", &sample));
    }

    #[test]
    fn test_rust_extension() {
        let code = b"fn main() { println!(\"hello\"); }";
        assert!(!is_binary(".rs", code));
    }

    #[test]
    fn test_case_insensitive() {
        assert!(is_binary(".ZIP", b"anything"));
        assert!(is_binary(".Png", b"anything"));
    }
}
