use std::fmt;
use std::iter::Peekable;
use thiserror::Error;

use crate::provenance::Provenance;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub value: TokenValue,
    pub start: Provenance,
    pub end: Provenance,
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at {}", self.value, self.start)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenValue {
    Word(String),
    Int(u64),
    Plus,
    Minus,
    Asterisk,
    ForwardSlash,
    Equals,
    Colon,
    Comma,
    Semicolon,
    Period,
    OpenParen,
    CloseParen,
    OpenBracket,
    CloseBracket,
    OpenSquare,
    CloseSquare,
    LessThan,
    GreaterThan,
    If,
    While,
    Let,
    True,
    False,
    Function,
    Import,
    Struct,
    Unique,
    Shared,
    Return,
}

impl fmt::Display for TokenValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use TokenValue::*;
        match self {
            Word(word) => write!(f, "word {}", word),
            Int(int) => write!(f, "int {}", int),
            Plus => write!(f, "+"),
            Minus => write!(f, "-"),
            Asterisk => write!(f, "*"),
            ForwardSlash => write!(f, "/"),
            Equals => write!(f, "="),
            Semicolon => write!(f, ";"),
            Comma => write!(f, ","),
            Colon => write!(f, ":"),
            Period => write!(f, "."),
            OpenParen => write!(f, "("),
            CloseParen => write!(f, ")"),
            OpenBracket => write!(f, "{{"),
            CloseBracket => write!(f, "}}"),
            OpenSquare => write!(f, "["),
            CloseSquare => write!(f, "]"),
            LessThan => write!(f, "<"),
            GreaterThan => write!(f, ">"),
            Let => write!(f, "keyword let"),
            If => write!(f, "keyword if"),
            While => write!(f, "keyword while"),
            True => write!(f, "keyword true"),
            False => write!(f, "keyword false"),
            Function => write!(f, "keyword fn"),
            Import => write!(f, "keyword import"),
            Struct => write!(f, "keyword struct"),
            Unique => write!(f, "keyword unique"),
            Shared => write!(f, "keyword shared"),
            Return => write!(f, "keyword return"),
        }
    }
}

#[derive(Clone, Debug, Error)]
pub enum LexError {
    #[error("unexpected character {0} at {1}")]
    UnexpectedStart(char, Provenance),
    #[error("illegal null byte in source code at {0}")]
    IllegalNullByte(Provenance),
}

pub fn lex<'a>(
    source_name: &'static str,
    source_text: String,
) -> impl 'a + Iterator<Item = Result<Token, LexError>> {
    let source_text = Box::leak(source_text.into_boxed_str());
    let source = source_text.chars().peekable();

    TokenIterator {
        source,
        source_name,
        source_text,
        line: 1,
        offset: 0,
    }
}

struct TokenIterator<T: Iterator<Item = char>> {
    source: Peekable<T>,
    source_name: &'static str,
    source_text: &'static str,
    line: u32,
    offset: u32,
}

impl<T: Iterator<Item = char>> TokenIterator<T> {
    fn next_char(&mut self) -> Option<(char, Provenance)> {
        match self.source.next() {
            None => None,
            Some('\n') => {
                self.line += 1;
                self.offset = 0;
                self.next_char()
            }
            Some(chr) => {
                self.offset += 1;
                Some((
                    chr,
                    Provenance::new(self.source_name, self.source_text, self.line, self.offset),
                ))
            }
        }
    }
}

impl<T: Iterator<Item = char>> Iterator for TokenIterator<T> {
    type Item = Result<Token, LexError>;

    fn next(&mut self) -> Option<Result<Token, LexError>> {
        if let Some((chr, start)) = self.next_char() {
            let mut end = None;
            let value = match chr {
                letter @ ('a'..='z' | 'A'..='Z' | '_') => {
                    let mut word = String::new();
                    word.push(letter);
                    while let Some(candidate) = self.source.peek() {
                        match candidate {
                            letter @ ('a'..='z' | 'A'..='Z' | '_' | '0'..='9') => {
                                word.push(*letter);
                            }
                            _ => break,
                        }
                        let (_, p) = self.next_char().unwrap();
                        end = Some(p);
                    }

                    match word.as_str() {
                        "if" => TokenValue::If,
                        "while" => TokenValue::While,
                        "true" => TokenValue::True,
                        "false" => TokenValue::False,
                        "let" => TokenValue::Let,
                        "fn" => TokenValue::Function,
                        "import" => TokenValue::Import,
                        "struct" => TokenValue::Struct,
                        "unique" => TokenValue::Unique,
                        "shared" => TokenValue::Shared,
                        "return" => TokenValue::Return,
                        _ => TokenValue::Word(word),
                    }
                }
                digit @ '0'..='9' => {
                    let mut number: u64 = (digit as u32 - '0' as u32) as u64;

                    // TODO: handle overflow
                    while let Some(candidate) = self.source.peek() {
                        match candidate {
                            digit @ '0'..='9' => {
                                number = number * 10 + (*digit as u32 - '0' as u32) as u64;
                            }
                            '_' => {}
                            _ => break,
                        }
                        let (_, p) = self.next_char().unwrap();
                        end = Some(p);
                    }

                    TokenValue::Int(number)
                }
                '+' => TokenValue::Plus,
                '-' => TokenValue::Minus,
                '=' => TokenValue::Equals,
                ',' => TokenValue::Comma,
                ';' => TokenValue::Semicolon,
                ':' => TokenValue::Colon,
                '.' => TokenValue::Period,
                '(' => TokenValue::OpenParen,
                ')' => TokenValue::CloseParen,
                '{' => TokenValue::OpenBracket,
                '}' => TokenValue::CloseBracket,
                '<' => TokenValue::LessThan,
                '>' => TokenValue::GreaterThan,
                '[' => TokenValue::OpenSquare,
                ']' => TokenValue::CloseSquare,
                '*' => TokenValue::Asterisk,
                '/' => TokenValue::ForwardSlash,
                '\0' => return Some(Err(LexError::IllegalNullByte(start))),
                ch if ch.is_whitespace() => return self.next(),
                ch => return Some(Err(LexError::UnexpectedStart(ch, start))),
            };

            Some(Ok(Token {
                value,
                start,
                end: end.unwrap_or(start),
            }))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use TokenValue::*;

    #[test]
    fn reserved_words() {
        let result = lex("test", "if let true false fn word".to_string())
            .map(|token| token.map(|token| token.value))
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(
            result,
            vec![If, Let, True, False, Function, Word("word".to_string())]
        );
    }
}
