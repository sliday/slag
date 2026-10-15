//! Lexer: turns source text into tokens tagged with 1-based line numbers.

/// A lexical token with the 1-based line it started on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    Word { line: usize, text: String },
    Equals { line: usize },
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | '/' | ':' | '*')
}

/// Tokenize `src`. Unknown characters are skipped.
pub fn lex(src: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut line = 1usize;
    let mut chars = src.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            '\n' => {
                line += 1;
                chars.next();
            }
            ' ' | '\t' | '\r' => {
                chars.next();
            }
            '#' => {
                // Skip the comment to end of line.
                while let Some(&c2) = chars.peek() {
                    chars.next();
                    if c2 == '\n' {
                        break;
                    }
                }
            }
            '=' => {
                tokens.push(Token::Equals { line });
                chars.next();
            }
            _ if is_word_char(c) => {
                let mut text = String::new();
                while let Some(&c2) = chars.peek() {
                    if is_word_char(c2) {
                        text.push(c2);
                        chars.next();
                    } else {
                        break;
                    }
                }
                tokens.push(Token::Word { line, text });
            }
            _ => {
                chars.next();
            }
        }
    }
    tokens
}
