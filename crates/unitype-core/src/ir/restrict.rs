//! Restrictions a bounded type puts on values beyond what a plain type says.
//!
//! Two kinds exist. A string may be restricted to the strings a regular
//! expression matches in full. A future may be restricted to a window of time
//! in which its value must be delivered. Both are checked where a value
//! crosses a boundary, the way a bound on an integer is, and neither is
//! reasoned about by the checker beyond what it can decide cheaply: a literal
//! string is tested against its pattern, and two windows are compared as
//! intervals. Whether two patterns accept the same strings is not decided, so
//! one pattern fits another only when they are the same text.

/// Whether `text` matches `pattern` in full, not just somewhere inside it.
///
/// The pattern is written in Rust's `regex` syntax. It is anchored at both
/// ends, so `[a-z]+` admits `abc` and refuses `abc1`.
pub fn full_match(pattern: &str, text: &str) -> Result<bool, regex::Error> {
    let anchored = regex::Regex::new(&format!("^(?:{pattern})$"))?;
    Ok(anchored.is_match(text))
}

#[cfg(test)]
mod tests {
    use super::full_match;

    #[test]
    fn a_pattern_must_cover_the_whole_string() {
        assert!(full_match("[a-z]+", "abc").unwrap());
        assert!(!full_match("[a-z]+", "abc1").unwrap());
        assert!(!full_match("[a-z]+", "1abc").unwrap());
    }

    #[test]
    fn alternation_is_anchored_as_a_whole() {
        assert!(full_match("cat|dog", "dog").unwrap());
        assert!(!full_match("cat|dog", "cats").unwrap());
    }

    #[test]
    fn a_pattern_that_does_not_compile_says_so() {
        assert!(full_match("(", "x").is_err());
    }
}
