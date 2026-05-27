//! Shared slug-at-version parser used by `knack pull` and `knack run`.
//!
//! Accepted shapes:
//!
//!   * `slug`                     → `(slug, None)`
//!   * `slug@1.2.0`               → `(slug, Some("1.2.0"))`
//!   * `@author/slug`             → `("@author/slug", None)`
//!   * `@author/slug@1.2.0`       → `("@author/slug", Some("1.2.0"))`
//!
//! The leading `@` of a handle is preserved so `find_by_slug` can route to
//! the marketplace resolver. Only an `@` that appears *after* a leading-
//! handle prefix counts as the version separator.

pub fn parse_slug_at_version(s: &str) -> (&str, Option<&str>) {
    let lookup_start = usize::from(s.starts_with('@'));
    if let Some(rel) = s[lookup_start..].find('@') {
        let abs = lookup_start + rel;
        return (&s[..abs], Some(&s[abs + 1..]));
    }
    (s, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_version() {
        assert_eq!(parse_slug_at_version("foo@1.0.0"), ("foo", Some("1.0.0")));
    }

    #[test]
    fn without_version() {
        assert_eq!(parse_slug_at_version("foo"), ("foo", None));
    }

    #[test]
    fn v_prefix_left_intact() {
        // Server normalizes `v1.0` → `1.0.0`. CLI doesn't pre-strip.
        assert_eq!(parse_slug_at_version("foo@v1.0"), ("foo", Some("v1.0")));
    }

    #[test]
    fn handle_slug_no_version() {
        assert_eq!(
            parse_slug_at_version("@KnackOfficial/monthly-close"),
            ("@KnackOfficial/monthly-close", None)
        );
    }

    #[test]
    fn handle_slug_with_version() {
        assert_eq!(
            parse_slug_at_version("@KnackOfficial/monthly-close@1.2.0"),
            ("@KnackOfficial/monthly-close", Some("1.2.0"))
        );
    }
}
