//! Token types for WAL lexer

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub struct Position {
    pub line: usize,
    pub column: usize,
    pub offset: usize,
}

impl Position {
    pub fn new(line: usize, column: usize, offset: usize) -> Self {
        Self { line, column, offset }
    }
}

impl fmt::Display for Position {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.column)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub value: String,
    pub pos: Position,
}

impl Token {
    pub fn new(kind: TokenKind, value: String, pos: Position) -> Self {
        Self { kind, value, pos }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    // Literals
    Symbol,
    Int,
    Float,
    String,
    Bool,

    // Operators
    Plus,
    Minus,
    Star,
    Slash,
    DoubleStar,
    Percent,

    // Comparison
    Eq,
    Neq,
    Lt,
   Gt,
    Le,
    Ge,

    // Logical
    And,
    Or,
    Not,

    // Delimiters
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,

    // Special
    Quote,
    Quasiquote,
    Unquote,
    UnquoteSplice,
    At,              // @ for timed expressions
    Tilde,           // ~ for scoped symbols
    Hash,            // # for grouped symbols

    // Other
    Comment,
    Whitespace,
    Comma,
    Eof,
    Error,
}

impl fmt::Display for TokenKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TokenKind::Symbol => write!(f, "symbol"),
            TokenKind::Int => write!(f, "integer"),
            TokenKind::Float => write!(f, "float"),
            TokenKind::String => write!(f, "string"),
            TokenKind::Bool => write!(f, "bool"),
            TokenKind::Plus => write!(f, "+"),
            TokenKind::Minus => write!(f, "-"),
            TokenKind::Star => write!(f, "*"),
            TokenKind::Slash => write!(f, "/"),
            TokenKind::DoubleStar => write!(f, "**"),
            TokenKind::Percent => write!(f, "%"),
            TokenKind::Eq => write!(f, "="),
            TokenKind::Neq => write!(f, "!="),
            TokenKind::Lt => write!(f, "<"),
            TokenKind::Gt => write!(f, ">"),
            TokenKind::Le => write!(f, "<="),
            TokenKind::Ge => write!(f, ">="),
            TokenKind::And => write!(f, "&&"),
            TokenKind::Or => write!(f, "||"),
            TokenKind::Not => write!(f, "!"),
            TokenKind::LParen => write!(f, "("),
            TokenKind::RParen => write!(f, ")"),
            TokenKind::LBracket => write!(f, "["),
            TokenKind::RBracket => write!(f, "]"),
            TokenKind::LBrace => write!(f, "{{"),
            TokenKind::RBrace => write!(f, "}}"),
            TokenKind::Quote => write!(f, "'"),
            TokenKind::Quasiquote => write!(f, "`"),
            TokenKind::Unquote => write!(f, ","),
            TokenKind::UnquoteSplice => write!(f, ",@"),
            TokenKind::At => write!(f, "@"),
            TokenKind::Tilde => write!(f, "~"),
            TokenKind::Hash => write!(f, "#"),
            TokenKind::Comment => write!(f, "comment"),
            TokenKind::Whitespace => write!(f, "whitespace"),
            TokenKind::Comma => write!(f, ","),
            TokenKind::Eof => write!(f, "EOF"),
            TokenKind::Error => write!(f, "error"),
        }
    }
}