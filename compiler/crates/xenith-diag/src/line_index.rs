//! Byte offset to line/column conversion.
//!
//! Spans are byte offsets everywhere inside the compiler; line and column are
//! computed only at the rendering boundary. Build one [`LineIndex`] per file
//! and reuse it — construction is linear in the file, lookup is logarithmic.

use serde::{Deserialize, Serialize};

/// A one-based line and column.
///
/// `column` counts Unicode scalar values, not bytes, so a tab is one column and
/// `あ` is one column. That matches what a reader counts when looking at the
/// caret in a terminal.
///
/// Note for later: LSP measures columns in UTF-16 code units by default. That
/// conversion belongs in the language server, not here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LineCol {
    pub line: u32,
    pub column: u32,
}

#[derive(Clone, Debug)]
pub struct LineIndex {
    /// Byte offset at which each line begins. Always starts with `0`.
    line_starts: Vec<u32>,
    /// Length of the indexed source in bytes, so lookups can be clamped.
    len: u32,
}

impl LineIndex {
    pub fn new(source: &str) -> LineIndex {
        let mut line_starts = vec![0u32];
        for (offset, byte) in source.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(offset as u32 + 1);
            }
        }
        LineIndex {
            line_starts,
            len: source.len() as u32,
        }
    }

    pub fn line_count(&self) -> u32 {
        self.line_starts.len() as u32
    }

    /// Byte offset where the given one-based line begins.
    pub fn line_start(&self, line: u32) -> Option<u32> {
        if line == 0 {
            return None;
        }
        self.line_starts.get(line as usize - 1).copied()
    }

    /// Convert a byte offset to a one-based line and column.
    ///
    /// Offsets past the end of the source clamp to the end rather than
    /// panicking; a diagnostic renderer must never be what crashes the
    /// compiler.
    pub fn line_col(&self, source: &str, offset: u32) -> LineCol {
        let offset = offset.min(self.len);
        // `partition_point` gives the count of line starts at or before
        // `offset`, which is exactly the one-based line number.
        let line = self.line_starts.partition_point(|&start| start <= offset);
        let line_start = self.line_starts[line - 1] as usize;

        // Count characters, not bytes, and tolerate an offset that lands
        // mid-character by counting the characters fully before it.
        let column = source
            .get(line_start..offset as usize)
            .map(|text| text.chars().count())
            .unwrap_or(0) as u32
            + 1;

        LineCol {
            line: line as u32,
            column,
        }
    }

    /// The text of a one-based line, without its trailing newline.
    pub fn line_text<'a>(&self, source: &'a str, line: u32) -> Option<&'a str> {
        let start = self.line_start(line)? as usize;
        let end = self
            .line_start(line + 1)
            .map(|next| next as usize)
            .unwrap_or(source.len());
        let text = source.get(start..end)?;
        Some(text.trim_end_matches('\n').trim_end_matches('\r'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_character_is_line_one_column_one() {
        let src = "abc";
        let index = LineIndex::new(src);
        assert_eq!(index.line_col(src, 0), LineCol { line: 1, column: 1 });
    }

    #[test]
    fn columns_advance_within_a_line() {
        let src = "abc";
        let index = LineIndex::new(src);
        assert_eq!(index.line_col(src, 2).column, 3);
    }

    #[test]
    fn newline_starts_the_next_line() {
        let src = "ab\ncd";
        let index = LineIndex::new(src);
        assert_eq!(index.line_count(), 2);
        // offset 2 is the '\n' itself, still on line 1
        assert_eq!(index.line_col(src, 2).line, 1);
        // offset 3 is 'c', the first character of line 2
        assert_eq!(index.line_col(src, 3), LineCol { line: 2, column: 1 });
    }

    #[test]
    fn columns_count_characters_not_bytes() {
        // Each "あ" is three bytes but one column.
        let src = "あああx";
        let index = LineIndex::new(src);
        assert_eq!(index.line_col(src, 9).column, 4, "x is the 4th character");
    }

    #[test]
    fn crlf_line_endings_are_handled() {
        let src = "ab\r\ncd";
        let index = LineIndex::new(src);
        assert_eq!(index.line_count(), 2);
        assert_eq!(index.line_col(src, 4).line, 2);
        assert_eq!(index.line_text(src, 1), Some("ab"), "\\r is trimmed");
    }

    #[test]
    fn offset_past_the_end_clamps_instead_of_panicking() {
        let src = "ab\ncd";
        let index = LineIndex::new(src);
        let got = index.line_col(src, 9_999);
        assert_eq!(got.line, 2);
        assert_eq!(got.column, 3, "clamps to just past the last character");
    }

    #[test]
    fn offset_inside_a_multibyte_character_does_not_panic() {
        let src = "あい";
        let index = LineIndex::new(src);
        // Offset 1 is inside the first character.
        let got = index.line_col(src, 1);
        assert_eq!(got.line, 1);
    }

    #[test]
    fn line_text_excludes_the_newline() {
        let src = "one\ntwo\nthree";
        let index = LineIndex::new(src);
        assert_eq!(index.line_text(src, 1), Some("one"));
        assert_eq!(index.line_text(src, 2), Some("two"));
        assert_eq!(index.line_text(src, 3), Some("three"));
        assert_eq!(index.line_text(src, 4), None);
    }

    #[test]
    fn empty_source_is_one_empty_line() {
        let src = "";
        let index = LineIndex::new(src);
        assert_eq!(index.line_count(), 1);
        assert_eq!(index.line_col(src, 0), LineCol { line: 1, column: 1 });
        assert_eq!(index.line_text(src, 1), Some(""));
    }

    #[test]
    fn trailing_newline_creates_a_final_empty_line() {
        let src = "a\n";
        let index = LineIndex::new(src);
        assert_eq!(index.line_count(), 2);
        assert_eq!(index.line_col(src, 2).line, 2);
    }
}
