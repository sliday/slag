//! Parser: builds config entries from tokens.

use crate::lexer::{lex, Token};

/// One `key = value` entry with its 1-based source line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub key: String,
    pub value: String,
    pub line: usize,
}

/// A parse failure with the 1-based line where it was detected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

/// Parse `src` into entries. Returns the first error encountered.
pub fn parse(src: &str) -> Result<Vec<Entry>, ParseError> {
    let tokens = lex(src);
    let mut entries = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        match &tokens[i] {
            Token::Word { line, text } => {
                let key = text.clone();
                let key_line = *line;
                match tokens.get(i + 1) {
                    Some(Token::Equals { .. }) => {}
                    Some(Token::Word { line, .. }) => {
                        return Err(ParseError {
                            line: *line,
                            message: "expected '=', found a word".to_string(),
                        });
                    }
                    None => {
                        return Err(ParseError {
                            line: key_line,
                            message: "expected '=', found end of input".to_string(),
                        });
                    }
                }
                match tokens.get(i + 2) {
                    Some(Token::Word { text, .. }) => {
                        entries.push(Entry {
                            key,
                            value: text.clone(),
                            line: key_line,
                        });
                    }
                    Some(Token::Equals { line }) => {
                        return Err(ParseError {
                            line: *line,
                            message: "expected a value, found '='".to_string(),
                        });
                    }
                    None => {
                        return Err(ParseError {
                            line: key_line,
                            message: "expected a value, found end of input".to_string(),
                        });
                    }
                }
                i += 3;
            }
            Token::Equals { line } => {
                return Err(ParseError {
                    line: *line,
                    message: "unexpected '='".to_string(),
                });
            }
        }
    }
    Ok(entries)
}
