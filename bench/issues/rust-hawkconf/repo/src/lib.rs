//! hawkconf: a tiny parser for `.hawk` config files.
//!
//! Format: `key = value` entries, one per line. `#` starts a comment
//! that runs to the end of the line. Blank lines are ignored.

mod lexer;
mod parser;

pub use lexer::{lex, Token};
pub use parser::{parse, Entry, ParseError};
