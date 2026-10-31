//! Error types — [`ParseError`] carries a miette span so `arca
//! check` can point at the malformed line; [`EmitError`] reports
//! models the canonical grammar can't express.

use miette::{Diagnostic, SourceSpan};
use thiserror::Error;

/// A `project/` file that doesn't fit the grammar.
#[derive(Debug, Clone, Error, Diagnostic)]
#[error("{file}:{line}:{column}: {message}")]
#[diagnostic(code(arca::store::parse))]
pub struct ParseError {
    file: String,
    line: usize,
    column: usize,
    message: String,
    /// Normalized source (CRLF → LF) the line/column refer to.
    #[source_code]
    src: String,
    /// Byte span of the offending node/token.
    #[label]
    span: SourceSpan,
}

impl ParseError {
    /// Error at a byte span (offsets into `src`).
    pub fn at(
        file: impl Into<String>,
        src: String,
        offset: usize,
        len: usize,
        message: impl Into<String>,
    ) -> Self {
        let (line, column) = line_col(&src, offset);
        ParseError {
            file: file.into(),
            line,
            column,
            message: message.into(),
            src,
            span: (offset, len.max(1)).into(),
        }
    }

    /// Error at an mdast [`markdown::unist::Position`].
    pub fn at_position(
        file: impl Into<String>,
        src: String,
        pos: &markdown::unist::Position,
        message: impl Into<String>,
    ) -> Self {
        let mut e = ParseError::at(
            file,
            src,
            pos.start.offset,
            pos.end.offset.saturating_sub(pos.start.offset),
            message,
        );
        // Trust mdast's line/col over recomputation.
        e.line = pos.start.line;
        e.column = pos.start.column;
        e
    }

    /// Error with no precise location — points at the file start.
    pub fn whole_file(file: impl Into<String>, src: String, message: impl Into<String>) -> Self {
        ParseError::at(file, src, 0, 1, message)
    }

    /// The file this error points at.
    pub fn file(&self) -> &str {
        &self.file
    }

    /// 1-based line.
    pub fn line(&self) -> usize {
        self.line
    }

    /// 1-based column.
    pub fn column(&self) -> usize {
        self.column
    }

    /// What went wrong.
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// Line/column (1-based) of a byte offset.
fn line_col(src: &str, offset: usize) -> (usize, usize) {
    let mut line = 1;
    let mut col = 1;
    for (i, ch) in src.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// A model the canonical grammar can't express — e.g. a `done`
/// item in `TODO.md`, a non-deferred item in `BACKLOG.md`, a title
/// containing `·` or a newline, an invalid `#label`.
#[derive(Debug, Error)]
#[error("cannot emit {file}: {message}")]
pub struct EmitError {
    file: &'static str,
    message: String,
}

impl EmitError {
    /// New emit failure for `file` (`TODO.md`, `meta.toml`, …).
    pub fn new(file: &'static str, message: impl Into<String>) -> Self {
        EmitError {
            file,
            message: message.into(),
        }
    }
}

impl From<ParseError> for arca_store::Error {
    fn from(e: ParseError) -> Self {
        arca_store::Error::Parse {
            path: e.file,
            line: e.line,
            column: e.column,
            message: e.message,
        }
    }
}

impl From<EmitError> for arca_store::Error {
    fn from(e: EmitError) -> Self {
        arca_store::Error::Invalid(e.to_string())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn line_col_tracks() {
        let src = "a\nbc\ndef".to_string();
        assert_eq!(line_col(&src, 0), (1, 1));
        assert_eq!(line_col(&src, 2), (2, 1));
        assert_eq!(line_col(&src, 5), (3, 1));
        assert_eq!(line_col(&src, 99), (3, 4));
    }

    #[test]
    fn error_spans() {
        let e = ParseError::at("TODO.md", "ab\ncd".into(), 3, 2, "bad");
        assert_eq!((e.line(), e.column()), (2, 1));
        assert_eq!(e.message(), "bad");
        assert_eq!(e.to_string(), "TODO.md:2:1: bad");
        let store: arca_store::Error = e.into();
        assert_eq!(store.to_string(), "TODO.md:2:1: bad");

        let w = ParseError::whole_file("x.md", "hi".into(), "nope");
        assert_eq!((w.line(), w.column()), (1, 1));

        // miette Diagnostic impl works
        let report = format!("{:?}", miette::Report::new(w));
        assert!(report.contains("nope"));
    }

    #[test]
    fn emit_error() {
        let e = EmitError::new("TODO.md", "done item");
        assert_eq!(e.to_string(), "cannot emit TODO.md: done item");
        let store: arca_store::Error = e.into();
        assert!(matches!(store, arca_store::Error::Invalid(_)));
    }

    #[test]
    fn at_position_uses_mdast_lines() {
        let pos = markdown::unist::Position {
            start: markdown::unist::Point {
                line: 7,
                column: 3,
                offset: 40,
            },
            end: markdown::unist::Point {
                line: 7,
                column: 9,
                offset: 46,
            },
        };
        let e = ParseError::at_position("f.md", "x".repeat(60), &pos, "bad");
        assert_eq!((e.line(), e.column()), (7, 3));
    }
}
