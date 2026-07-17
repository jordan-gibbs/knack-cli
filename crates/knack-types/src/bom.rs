//! UTF-8 BOM tolerance.
//!
//! Windows tooling loves to prepend EF BB BF: PowerShell 5.1's
//! `Set-Content -Encoding utf8` always writes one, and Notepad historically
//! did too. serde_yaml treats the BOM as document content, so a BOM'd
//! `meta.knack.yaml` fails strict deserialization with the baffling
//! "missing field `slug` at line 1 column 2". Every YAML (and frontmatter)
//! parse in the CLI must go through these helpers.

const BOM_BYTES: &[u8] = b"\xef\xbb\xbf";
const BOM_CHAR: char = '\u{feff}';

/// Strip a single leading UTF-8 BOM from a byte slice, if present.
pub fn strip_utf8_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(BOM_BYTES).unwrap_or(bytes)
}

/// Strip a single leading BOM char from a string slice, if present.
pub fn strip_utf8_bom_str(s: &str) -> &str {
    s.strip_prefix(BOM_CHAR).unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_bom_bytes() {
        assert_eq!(strip_utf8_bom(b"\xef\xbb\xbfslug: x"), b"slug: x");
        assert_eq!(strip_utf8_bom(b"slug: x"), b"slug: x");
        assert_eq!(strip_utf8_bom(b""), b"");
    }

    #[test]
    fn strips_bom_char() {
        assert_eq!(strip_utf8_bom_str("\u{feff}slug: x"), "slug: x");
        assert_eq!(strip_utf8_bom_str("slug: x"), "slug: x");
    }

    #[test]
    fn strips_only_one_leading_bom() {
        // A double BOM is malformed input; we only ever eat the first so
        // genuinely corrupt files still fail loudly downstream.
        assert_eq!(strip_utf8_bom(b"\xef\xbb\xbf\xef\xbb\xbfx"), b"\xef\xbb\xbfx");
    }
}
