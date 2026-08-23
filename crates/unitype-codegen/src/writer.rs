//! Building up a file of generated code.
//!
//! Nothing clever — an indented string builder. It exists so that indentation
//! is tracked in one place rather than threaded through every emitter, and so
//! that every generated file ends the same way: newline-terminated, no trailing
//! blank lines, no stray whitespace at the end of a line.
//!
//! That tidiness matters more than it sounds. Generated files get committed and
//! diffed, and a file that regenerates with invisible differences makes every
//! diff useless.

use std::fmt::Write as _;

/// An indented buffer of generated code.
#[derive(Debug, Default)]
pub struct CodeWriter {
    out: String,
    depth: usize,
    unit: &'static str,
}

impl CodeWriter {
    /// A writer indenting by four spaces.
    pub fn new() -> Self {
        Self::with_indent("    ")
    }

    /// A writer indenting by something else.
    pub fn with_indent(unit: &'static str) -> Self {
        Self {
            out: String::new(),
            depth: 0,
            unit,
        }
    }

    /// Writes one line at the current indentation.
    ///
    /// An empty line stays empty rather than becoming indentation, since
    /// trailing whitespace is exactly the kind of invisible difference that
    /// makes a regenerated file look changed when it is not.
    pub fn line(&mut self, text: impl AsRef<str>) {
        let text = text.as_ref();
        if text.is_empty() {
            self.out.push('\n');
            return;
        }
        for _ in 0..self.depth {
            self.out.push_str(self.unit);
        }
        self.out.push_str(text.trim_end());
        self.out.push('\n');
    }

    /// Writes a formatted line.
    pub fn write(&mut self, args: std::fmt::Arguments<'_>) {
        let mut text = String::new();
        let _ = text.write_fmt(args);
        self.line(text);
    }

    /// A blank line, unless the buffer is empty or already ends with one.
    pub fn blank(&mut self) {
        if self.out.is_empty() || self.out.ends_with("\n\n") {
            return;
        }
        self.out.push('\n');
    }

    /// Writes prose as a comment, wrapped, with each line prefixed.
    pub fn comment(&mut self, prefix: &str, text: &str) {
        for line in wrap(text, 76_usize.saturating_sub(self.depth * self.unit.len())) {
            if line.is_empty() {
                self.line(prefix.trim_end());
            } else {
                self.line(format!("{prefix}{line}"));
            }
        }
    }

    /// Runs `body` one level further in.
    pub fn indented(&mut self, body: impl FnOnce(&mut Self)) {
        self.depth += 1;
        body(self);
        self.depth -= 1;
    }

    /// Opens a block, writes its contents indented, and closes it.
    pub fn block(&mut self, open: impl AsRef<str>, close: &str, body: impl FnOnce(&mut Self)) {
        self.line(open);
        self.indented(body);
        self.line(close);
    }

    /// Whether nothing has been written.
    pub fn is_empty(&self) -> bool {
        self.out.is_empty()
    }

    /// The finished file: newline-terminated, with no trailing blank lines.
    pub fn finish(self) -> String {
        let trimmed = self.out.trim_end();
        if trimmed.is_empty() {
            String::new()
        } else {
            format!("{trimmed}\n")
        }
    }
}

/// Breaks prose into lines no longer than `width`, keeping paragraph breaks.
///
/// A word longer than the width goes on its own line rather than being cut:
/// a generated type name is not improved by hyphenation.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            if line.is_empty() {
                line.push_str(word);
            } else if line.len() + 1 + word.len() <= width {
                line.push(' ');
                line.push_str(word);
            } else {
                out.push(std::mem::take(&mut line));
                line.push_str(word);
            }
        }
        if !line.is_empty() {
            out.push(line);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_indented_by_how_deep_they_are() {
        let mut w = CodeWriter::new();
        w.line("outer");
        w.indented(|w| {
            w.line("inner");
            w.indented(|w| w.line("deeper"));
        });
        assert_eq!(w.finish(), "outer\n    inner\n        deeper\n");
    }

    #[test]
    fn a_block_opens_indents_and_closes() {
        let mut w = CodeWriter::new();
        w.block("interface Shape {", "}", |w| w.line("id: string;"));
        assert_eq!(w.finish(), "interface Shape {\n    id: string;\n}\n");
    }

    #[test]
    fn a_blank_line_is_blank_rather_than_indentation() {
        let mut w = CodeWriter::new();
        w.indented(|w| {
            w.line("a");
            w.line("");
            w.line("b");
        });
        assert_eq!(w.finish(), "    a\n\n    b\n");
    }

    #[test]
    fn nothing_ends_with_whitespace_that_a_diff_would_notice() {
        let mut w = CodeWriter::new();
        w.line("code   ");
        w.blank();
        w.blank();
        let out = w.finish();
        assert_eq!(out, "code\n");
    }

    #[test]
    fn blank_lines_do_not_pile_up() {
        let mut w = CodeWriter::new();
        w.line("a");
        w.blank();
        w.blank();
        w.blank();
        w.line("b");
        assert_eq!(w.finish(), "a\n\nb\n");
    }

    #[test]
    fn prose_is_wrapped_and_prefixed() {
        let mut w = CodeWriter::new();
        w.comment(
            "// ",
            "A shape that is round. This sentence is long enough to need breaking \
             across more than a single line of output.",
        );
        let out = w.finish();
        assert!(out.lines().all(|l| l.starts_with("// ")), "{out}");
        assert!(out.lines().all(|l| l.len() <= 80), "{out}");
        assert!(out.lines().count() > 1, "it should have wrapped: {out}");
    }

    #[test]
    fn a_word_too_long_to_wrap_is_left_alone() {
        let mut w = CodeWriter::new();
        w.comment("# ", &"x".repeat(120));
        let out = w.finish();
        assert_eq!(out.lines().count(), 1, "hyphenating a name would be worse");
    }

    #[test]
    fn an_empty_file_stays_empty() {
        assert_eq!(CodeWriter::new().finish(), "");
        assert!(CodeWriter::new().is_empty());
    }
}
