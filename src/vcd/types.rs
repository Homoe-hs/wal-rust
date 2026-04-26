//! VCD parser types
//!
//! Core types for representing VCD file data structures

use std::fmt;

/// VCD value representation
#[derive(Debug, Clone, PartialEq)]
pub enum VcdValue {
    /// Single bit: 0, 1, x, z, X, Z
    Bit(u8),
    /// Multi-bit vector
    Vector(Vec<u8>),
    /// Real number
    Real(f64),
}

impl VcdValue {
    /// Get the bit at position (0 = LSB)
    #[allow(dead_code)]
    pub fn bit_at(&self, pos: usize) -> u8 {
        match self {
            VcdValue::Bit(b) => *b,
            VcdValue::Vector(v) if pos < v.len() => v[v.len() - 1 - pos],
            _ => 0,
        }
    }

    /// Get width in bits
    pub fn width(&self) -> usize {
        match self {
            VcdValue::Bit(_) => 1,
            VcdValue::Vector(v) => v.len(),
            VcdValue::Real(_) => 64,
        }
    }

    /// Convert to bytes for FST emission
    #[allow(dead_code)]
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            VcdValue::Bit(b) => vec![*b],
            VcdValue::Vector(v) => v.clone(),
            VcdValue::Real(r) => {
                let bits = r.to_bits();
                bits.to_le_bytes().to_vec()
            }
        }
    }
}

impl VcdValue {
    /// Parse a scalar character to VcdValue
    pub fn parse_scalar(c: char) -> Option<Self> {
        match c {
            '0' | '1' | 'x' | 'z' | 'X' | 'Z' | 'u' | 'U' | 'w' | 'W' | 'l' | 'L' | '-' => {
                Some(VcdValue::Bit(c as u8))
            }
            _ => None,
        }
    }

    /// Parse a binary vector string (without 'b' prefix)
    pub fn parse_vector(s: &str) -> Option<Self> {
        if s.is_empty() {
            return None;
        }
        let bytes: Vec<u8> = s.bytes().collect();
        if bytes.iter().all(|&b| b == b'0' || b == b'1' || b == b'x' || b == b'z' || b == b'X' || b == b'Z') {
            Some(VcdValue::Vector(bytes))
        } else {
            None
        }
    }

    /// Parse a real number string (without 'r' prefix)
    pub fn parse_real(s: &str) -> Option<Self> {
        s.parse::<f64>().ok().map(VcdValue::Real)
    }
}

/// VCD event types emitted by the parser
#[derive(Debug, Clone)]
pub enum VcdEvent {
    /// Timescale directive
    Timescale(#[allow(dead_code)] i8),
    /// Date directive
    Date(#[allow(dead_code)] String),
    /// Scope start
    ScopeStart { #[allow(dead_code)] name: String, #[allow(dead_code)] kind: String },
    /// Scope end
    ScopeEnd,
    /// Variable declaration
    VarDecl {
        id: String,
        name: String,
        #[allow(dead_code)] width: u32,
        #[allow(dead_code)] var_type: String,
    },
    /// Timestamp marker
    Timestamp(u64),
    /// Value change for a signal (id is signal number for fast lookup)
    ValueChange { id: u32, value: VcdValue },
    /// Dump vars marker
    DumpVars,
    /// Dump off marker
    DumpOff,
    /// Dump on marker
    DumpOn,
    /// Comment (ignored)
    #[allow(dead_code)]
    Comment(String),
}

/// VCD parser error
#[derive(Debug, Clone)]
pub struct VcdError {
    pub line: usize,
    pub column: usize,
    pub kind: ErrorKind,
    pub help: Option<String>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum ErrorKind {
    /// Unexpected character found
    UnexpectedChar { found: char, expected: &'static str },
    /// Missing $end keyword
    MissingEndKeyword,
    /// Invalid timestamp format
    InvalidTimestamp { value: String },
    /// Unterminated comment
    UnterminatedComment,
    /// Duplicate signal declaration
    DuplicateSignal { first_line: usize },
    /// Invalid timescale value
    InvalidTimescale,
    /// IO error
    IoError(String),
}

impl fmt::Display for VcdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "error: {} at line {}:{}", self.kind, self.line, self.column)?;
        if let Some(help) = &self.help {
            writeln!(f, "help: {}", help)?;
        }
        Ok(())
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorKind::UnexpectedChar { found, expected } => {
                write!(f, "Unexpected character '{}', expected {}", found, expected)
            }
            ErrorKind::MissingEndKeyword => write!(f, "Missing $end keyword"),
            ErrorKind::InvalidTimestamp { value } => write!(f, "Invalid timestamp '{}'", value),
            ErrorKind::UnterminatedComment => write!(f, "Unterminated $comment"),
            ErrorKind::DuplicateSignal { first_line } => {
                write!(f, "Duplicate signal declaration (first at line {})", first_line)
            }
            ErrorKind::InvalidTimescale => write!(f, "Invalid $timescale value"),
            ErrorKind::IoError(msg) => write!(f, "IO error: {}", msg),
        }
    }
}

impl std::error::Error for VcdError {}

impl VcdError {
    /// Check if this error allows continuation
    #[allow(dead_code)]
    pub fn is_recoverable(&self) -> bool {
        match self.kind {
            ErrorKind::UnexpectedChar { .. } => true,
            ErrorKind::MissingEndKeyword => true,
            ErrorKind::InvalidTimestamp { .. } => true,
            ErrorKind::UnterminatedComment => true,
            ErrorKind::DuplicateSignal { .. } => false,
            ErrorKind::InvalidTimescale => false,
            ErrorKind::IoError(_) => false,
        }
    }
}

/// Parser state
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParserState {
    /// Parsing header section
    Header,
    /// Parsing dump section
    Dump,
    /// Skipping to $end keyword
    SkipToEnd,
}

impl Default for ParserState {
    fn default() -> Self {
        ParserState::Header
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_scalar() {
        assert_eq!(VcdValue::parse_scalar('0'), Some(VcdValue::Bit(b'0')));
        assert_eq!(VcdValue::parse_scalar('1'), Some(VcdValue::Bit(b'1')));
        assert_eq!(VcdValue::parse_scalar('x'), Some(VcdValue::Bit(b'x')));
        assert_eq!(VcdValue::parse_scalar('z'), Some(VcdValue::Bit(b'z')));
        assert_eq!(VcdValue::parse_scalar('X'), Some(VcdValue::Bit(b'X')));
        assert_eq!(VcdValue::parse_scalar('Z'), Some(VcdValue::Bit(b'Z')));
        assert_eq!(VcdValue::parse_scalar('a'), None);
    }

    #[test]
    fn test_parse_vector() {
        assert_eq!(
            VcdValue::parse_vector("1010"),
            Some(VcdValue::Vector(vec![b'1', b'0', b'1', b'0']))
        );
        assert_eq!(
            VcdValue::parse_vector("xxxx"),
            Some(VcdValue::Vector(vec![b'x', b'x', b'x', b'x']))
        );
        assert_eq!(VcdValue::parse_vector(""), None);
        assert_eq!(VcdValue::parse_vector("abc"), None);
    }

    #[test]
    fn test_parse_real() {
        assert_eq!(
            VcdValue::parse_real("3.14159"),
            Some(VcdValue::Real(3.14159))
        );
        assert_eq!(VcdValue::parse_real("invalid"), None);
    }

    #[test]
    fn test_vcd_value_width() {
        assert_eq!(VcdValue::Bit(b'0').width(), 1);
        assert_eq!(VcdValue::Vector(vec![b'1'; 8]).width(), 8);
        assert_eq!(VcdValue::Real(0.0).width(), 64);
    }
}
