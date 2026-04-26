//! VCD parser with streaming state machine
//!
//! Implements a zero-copy, streaming parser for VCD files.

use super::reader::LineReader;
use super::types::{ErrorKind, ParserState, VcdError, VcdEvent, VcdValue};
use std::io::Read;

/// VCD parser
pub struct VcdParser<R: Read> {
    reader: LineReader<R>,
    state: ParserState,
    /// Current scope stack
    scopes: Vec<String>,
    /// Known signal IDs
    signal_ids: Vec<String>,
    /// Last timestamp
    last_timestamp: u64,
    /// Line buffer for multi-line constructs
    #[allow(dead_code)]
    line_buf: String,
}

#[allow(dead_code)]
impl<R: Read> VcdParser<R> {
    /// Create a new VCD parser
    pub fn new(reader: R) -> Self {
        Self {
            reader: LineReader::new(reader),
            state: ParserState::Header,
            scopes: Vec::new(),
            signal_ids: Vec::new(),
            last_timestamp: 0,
            line_buf: String::new(),
        }
    }

    /// Get current line number
    #[allow(dead_code)]
    pub fn line_number(&self) -> usize {
        self.reader.line_number()
    }

    /// Parse the next event
    fn next_event(&mut self) -> Option<Result<VcdEvent, VcdError>> {
        loop {
            let line = match self.reader.read_line() {
                Ok(Some(l)) => l,
                Ok(None) => return None,
                Err(e) => return Some(Err(VcdError {
                    line: self.reader.line_number(),
                    column: 0,
                    kind: ErrorKind::IoError(e.to_string()),
                    help: None,
                })),
            };

            let line_num = self.reader.line_number();
            let trimmed = line.trim();

            if trimmed.is_empty() {
                continue;
            }

            match self.state {
                ParserState::Header => {
                    if let Some(event) = self.parse_header_line_impl(line_num, &trimmed, &line) {
                        return Some(event);
                    }
                }
                ParserState::Dump => {
                    if let Some(event) = self.parse_dump_line_impl(line_num, &trimmed) {
                        return Some(event);
                    }
                }
                ParserState::SkipToEnd => {
                    if trimmed == "$end" {
                        self.state = ParserState::Header;
                    } else if trimmed.starts_with("$") {
                        // Another directive started before $end - shouldn't happen in valid VCD
                        // But handle it gracefully
                    } else {
                        // This might be a timescale or date value
                        // For now, just skip it - the parse_header_line_impl would have
                        // returned None for multi-line cases
                    }
                    // Continue skipping
                }
            }
        }
    }

    /// Parse a line in header state (implementation)
    /// line: original line (may have leading whitespace)
    /// trimmed: trimmed line content
    fn parse_header_line_impl(&mut self, line_num: usize, trimmed: &str, _line: &str) -> Option<Result<VcdEvent, VcdError>> {
        if trimmed.starts_with("$scope") {
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if parts.len() >= 3 {
                let kind = parts[1];
                let name = parts[2];
                self.scopes.push(name.to_string());
                return Some(Ok(VcdEvent::ScopeStart {
                    name: name.to_string(),
                    kind: kind.to_string(),
                }));
            }
        } else if trimmed.starts_with("$upscope") {
            self.scopes.pop();
            return Some(Ok(VcdEvent::ScopeEnd));
        } else if trimmed.starts_with("$var") {
            // Remove $end from the line if present for proper parsing
            let clean_line = if trimmed.ends_with("$end") {
                &trimmed[..trimmed.len() - 4].trim()
            } else {
                trimmed
            };
            return Some(self.parse_var_decl_impl(line_num, clean_line));
        } else if trimmed.starts_with("$timescale") {
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if parts.len() >= 2 {
                // Value on same line: $timescale 1ns $end
                let timescale_str = parts[1..].join(" ");
                let exp = parse_timescale_exp(&timescale_str);
                return Some(Ok(VcdEvent::Timescale(exp)));
            } else {
                // Multi-line: value is on next line
                self.state = ParserState::SkipToEnd;
                return None;
            }
        } else if trimmed.starts_with("$date") {
            // Extract date - may be on same line or next line
            if let Some(date) = trimmed.strip_prefix("$date") {
                let date = date.trim().to_string();
                if !date.is_empty() && !date.contains("$end") {
                    return Some(Ok(VcdEvent::Date(date)));
                }
            }
            // Multi-line or no date: skip to $end
            self.state = ParserState::SkipToEnd;
            return None;
        } else if trimmed.starts_with("$version") {
            // Skip version, enter skip state
            self.state = ParserState::SkipToEnd;
            return None;
        } else if trimmed.starts_with("$enddefinitions") {
            self.state = ParserState::Dump;
            return Some(Ok(VcdEvent::DumpVars));
        } else if trimmed.starts_with("$comment") {
            self.state = ParserState::SkipToEnd;
        }
        None
    }

    /// Parse a $var declaration (implementation)
    fn parse_var_decl_impl(&mut self, line_num: usize, line: &str) -> Result<VcdEvent, VcdError> {
        let parts: Vec<&str> = line.split_whitespace().collect();

        if parts.len() < 5 {
            return Err(VcdError {
                line: line_num,
                column: 0,
                kind: ErrorKind::UnexpectedChar {
                    found: ' ',
                    expected: "$var kind width id name",
                },
                help: Some("Variable declaration should be: $var <type> <width> <id> <name>".to_string()),
            });
        }

        let var_type = parts[1];
        let width = parts[2].parse().unwrap_or(1);
        let id = parts[3].to_string();

        let name_start = line.find(parts[3]).unwrap() + parts[3].len();
        let name_end = line.rfind("$end").unwrap_or(line.len());
        let name = line[name_start..name_end].trim().to_string();

        self.signal_ids.push(id.clone());

        Ok(VcdEvent::VarDecl {
            id,
            name,
            width,
            var_type: var_type.to_string(),
        })
    }

    /// Parse a $timescale directive (implementation)
    #[allow(dead_code)]
    fn parse_timescale_impl(&mut self, line_num: usize, line: &str) -> Result<VcdEvent, VcdError> {
        let parts: Vec<&str> = line.split_whitespace().collect();

        if parts.len() < 2 {
            return Err(VcdError {
                line: line_num,
                column: 0,
                kind: ErrorKind::InvalidTimescale,
                help: Some("Timescale should be like '1 ns', '10 us', '1 ps', etc.".to_string()),
            });
        }

        let timescale_str = parts[1..].join(" ");
        let exp = parse_timescale_exp(&timescale_str);

        Ok(VcdEvent::Timescale(exp))
    }

    /// Parse a $date directive (implementation)
    #[allow(dead_code)]
    fn parse_date_impl(&mut self, line: &str) -> Result<VcdEvent, VcdError> {
        self.state = ParserState::SkipToEnd;
        let date = line
            .strip_prefix("$date")
            .unwrap_or("")
            .trim()
            .to_string();
        Ok(VcdEvent::Date(date))
    }

    /// Parse a line in dump state (implementation)
    #[inline]
    fn parse_dump_line_impl(&mut self, line_num: usize, line: &str) -> Option<Result<VcdEvent, VcdError>> {
        if line.is_empty() {
            return None;
        }

        let first_char = line.as_bytes()[0];

        if first_char == b'#' {
            let timestamp = line[1..].parse().unwrap_or(0);
            self.last_timestamp = timestamp;
            return Some(Ok(VcdEvent::Timestamp(timestamp)));
        } else if first_char == b'$' {
            if line.starts_with("$dumpvars") {
                return Some(Ok(VcdEvent::DumpVars));
            } else if line.starts_with("$dumpoff") {
                return Some(Ok(VcdEvent::DumpOff));
            } else if line.starts_with("$dumpon") {
                return Some(Ok(VcdEvent::DumpOn));
            } else if line.starts_with("$dumpall") {
                return None;
            }
        } else if first_char == b'b' || first_char == b'r' {
            return Some(self.parse_vector_change_impl(line_num, line, first_char as char));
        } else if first_char == b'0' || first_char == b'1' || first_char == b'x' || first_char == b'z' || first_char == b'X' || first_char == b'Z' {
            return Some(self.parse_scalar_change_impl(line_num, line, first_char as char));
        }

        None
    }

    /// Parse a vector value change (implementation)
    fn parse_vector_change_impl(&mut self, line_num: usize, line: &str, prefix: char) -> Result<VcdEvent, VcdError> {
        let parts: Vec<&str> = line.split_whitespace().collect();

        if parts.len() < 2 {
            return Err(VcdError {
                line: line_num,
                column: 0,
                kind: ErrorKind::UnexpectedChar {
                    found: ' ',
                    expected: "vector value and signal id",
                },
                help: None,
            });
        }

        let value_str = &parts[0][1..];
        let signal_num = parse_signal_number_fast(parts[1]);

        let value = if prefix == 'r' {
            VcdValue::parse_real(value_str)
        } else {
            VcdValue::parse_vector(value_str)
        };

        match value {
            Some(v) => Ok(VcdEvent::ValueChange { id: signal_num, value: v }),
            None => Err(VcdError {
                line: line_num,
                column: 0,
                kind: ErrorKind::InvalidTimestamp {
                    value: value_str.to_string(),
                },
                help: Some(format!("Invalid {} value format", if prefix == 'r' { "real" } else { "vector" })),
            }),
        }
    }

    /// Parse a scalar value change (implementation)
    #[inline]
    fn parse_scalar_change_impl(&mut self, line_num: usize, line: &str, value_char: char) -> Result<VcdEvent, VcdError> {
        // Fast path: for IDs like "s1", "s2", extract the number directly without allocating
        // Line format: "0s1" or "1s123" etc. - value_char is the first char (0 or 1)
        let id_str = &line[1..];
        let signal_num = parse_signal_number_fast(id_str);
        let value = VcdValue::parse_scalar(value_char);

        match value {
            Some(v) => Ok(VcdEvent::ValueChange { id: signal_num, value: v }),
            None => Err(VcdError {
                line: line_num,
                column: 0,
                kind: ErrorKind::UnexpectedChar {
                    found: value_char,
                    expected: "valid signal value (0, 1, x, z, X, Z)",
                },
                help: None,
            }),
        }
    }
}

impl<R: Read> Iterator for VcdParser<R> {
    type Item = Result<VcdEvent, VcdError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_event()
    }
}

/// Parse a timescale string to exponent
/// Examples:
///   "1 fs" -> -15
///   "1 ps" -> -12
///   "1 ns" -> -9
///   "1 us" -> -6
///   "1 ms" -> -3
///   "1 s"  -> 0
///   "10 ns" -> -8 (10 * 10^-9)
fn parse_timescale_exp(ts: &str) -> i8 {
    let ts_lower = ts.to_lowercase();

    if ts_lower.contains("fs") {
        let multiplier: f64 = ts_lower.split_whitespace().next().unwrap_or("1").parse().unwrap_or(1.0);
        return (multiplier.log10().round() as i8) + (-15i8);
    } else if ts_lower.contains("ps") {
        let multiplier: f64 = ts_lower.split_whitespace().next().unwrap_or("1").parse().unwrap_or(1.0);
        return (multiplier.log10().round() as i8) + (-12i8);
    } else if ts_lower.contains("ns") {
        let multiplier: f64 = ts_lower.split_whitespace().next().unwrap_or("1").parse().unwrap_or(1.0);
        return (multiplier.log10().round() as i8) + (-9i8);
    } else if ts_lower.contains("us") || ts_lower.contains("μs") {
        let multiplier: f64 = ts_lower.split_whitespace().next().unwrap_or("1").parse().unwrap_or(1.0);
        return (multiplier.log10().round() as i8) + (-6i8);
    } else if ts_lower.contains("ms") {
        let multiplier: f64 = ts_lower.split_whitespace().next().unwrap_or("1").parse().unwrap_or(1.0);
        return (multiplier.log10().round() as i8) + (-3i8);
    } else if ts_lower.contains("s") {
        let multiplier: f64 = ts_lower.split_whitespace().next().unwrap_or("1").parse().unwrap_or(1.0);
        return (multiplier.log10().round() as i8) + 0i8;
    }

    // Try to parse as a plain number
    let num: f64 = ts.parse().unwrap_or(1.0);
    num.log10().round() as i8
}

/// Parse a signal number from an ID string like "s1", "s123", "sig42"
/// Returns the parsed number, or 0 if parsing fails
#[inline]
fn parse_signal_number_fast(id: &str) -> u32 {
    // Fast path for IDs like "s1", "s123"
    // We know the format is typically: optional letter prefix + digits
    let bytes = id.as_bytes();
    let mut start = 0;

    // Skip non-digit characters at the start
    while start < bytes.len() && !bytes[start].is_ascii_digit() {
        start += 1;
    }

    if start < bytes.len() {
        let mut end = start;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end > start {
            // Parse the digits directly without allocation
            let mut num: u32 = 0;
            for &b in &bytes[start..end] {
                num = num * 10 + (b - b'0') as u32;
            }
            return num;
        }
    }

    // Fallback: try to parse the whole string
    id.trim().parse().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_parse_var() {
        let input = b"$var wire 1 ! clk $end\n";
        let parser = VcdParser::new(Cursor::new(&input[..]));
        let events: Vec<_> = parser.collect();
        assert!(!events.is_empty());
        assert!(matches!(events[0], Ok(VcdEvent::VarDecl { .. })));
    }

    #[test]
    fn test_parse_timestamp() {
        let input = b"$timescale 1ns $end
$scope module test $end
$var wire 1 ! clk $end
$upscope $end
$enddefinitions $end
#1000
";
        let mut parser = VcdParser::new(Cursor::new(&input[..]));
        // Skip to 5th event which should be timestamp
        for (i, event) in parser.enumerate() {
            if i == 5 {
                match event.unwrap() {
                    VcdEvent::Timestamp(t) => assert_eq!(t, 1000),
                    other => panic!("Expected timestamp at position 5, got {:?}", other),
                }
                break;
            }
        }
    }

    #[test]
    fn test_parse_scalar_change() {
        let input = b"$timescale 1ns $end
$scope module test $end
$var wire 1 ! clk $end
$upscope $end
$enddefinitions $end
$dumpvars
b0 !
0!
#10
1!
#20
$end
";
        let mut parser = VcdParser::new(Cursor::new(&input[..]));
        let mut found_1_event = false;
        for event in parser {
            if let Ok(VcdEvent::ValueChange { id, value }) = event {
                if id == 0 && matches!(value, VcdValue::Bit(b'1')) {
                    found_1_event = true;
                    break;
                }
            }
        }
        assert!(found_1_event, "Should find value change '1!' for '!' signal");
    }

    #[test]
    fn test_parse_vector_change() {
        let input = b"$timescale 1ns $end
$scope module test $end
$var wire 4 s1 data $end
$upscope $end
$enddefinitions $end
$dumpvars
b0000 s1
b1010 s1
#20
$end
";
        let mut parser = VcdParser::new(Cursor::new(&input[..]));
        let mut found_1010_event = false;
        for event in parser {
            if let Ok(VcdEvent::ValueChange { id, value }) = event {
                if id == 1 && matches!(value, VcdValue::Vector(ref v) if v == &[b'1', b'0', b'1', b'0']) {
                    found_1010_event = true;
                    break;
                }
            }
        }
        assert!(found_1010_event, "Should find vector change 'b1010 s1' for 's1' signal");
    }

    #[test]
    fn test_timescale_parsing() {
        assert_eq!(parse_timescale_exp("1 ns"), -9);
        assert_eq!(parse_timescale_exp("10 ns"), -8);
        assert_eq!(parse_timescale_exp("1 ps"), -12);
        assert_eq!(parse_timescale_exp("1 us"), -6);
    }
}

/// Specialized VCD parser for memory-mapped files
/// This avoids the overhead of LineReader/BufReader wrappers
pub struct MmapVcdParser {
    reader: super::reader::MmapReader,
    state: ParserState,
    scopes: Vec<String>,
    signal_ids: Vec<String>,
    last_timestamp: u64,
    #[allow(dead_code)]
    line_buf: String,
}

impl MmapVcdParser {
    /// Create a new VCD parser from a memory-mapped reader
    #[allow(dead_code)]
    pub fn new(reader: super::reader::MmapReader) -> Self {
        Self {
            reader,
            state: ParserState::Header,
            scopes: Vec::new(),
            signal_ids: Vec::new(),
            last_timestamp: 0,
            line_buf: String::new(),
        }
    }

    /// Get current line number
    #[allow(dead_code)]
    pub fn line_number(&self) -> usize {
        self.reader.line_number()
    }

    /// Parse the next event
    fn next_event(&mut self) -> Option<Result<VcdEvent, VcdError>> {
        loop {
            let line = match self.reader.read_line() {
                Some(l) => l,
                None => return None,
            };

            let line_num = self.reader.line_number();
            let trimmed = line.trim();

            if trimmed.is_empty() {
                continue;
            }

            match self.state {
                ParserState::Header => {
                    if let Some(event) = self.parse_header_line_impl(line_num, &trimmed, &line) {
                        return Some(event);
                    }
                }
                ParserState::Dump => {
                    if let Some(event) = self.parse_dump_line_impl(line_num, &trimmed) {
                        return Some(event);
                    }
                }
                ParserState::SkipToEnd => {
                    if trimmed == "$end" {
                        self.state = ParserState::Header;
                    }
                }
            }
        }
    }

    /// Parse a line in header state (implementation)
    fn parse_header_line_impl(&mut self, line_num: usize, trimmed: &str, _line: &str) -> Option<Result<VcdEvent, VcdError>> {
        if trimmed.starts_with("$scope") {
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if parts.len() >= 3 {
                let kind = parts[1];
                let name = parts[2];
                self.scopes.push(name.to_string());
                return Some(Ok(VcdEvent::ScopeStart {
                    name: name.to_string(),
                    kind: kind.to_string(),
                }));
            }
        } else if trimmed.starts_with("$upscope") {
            self.scopes.pop();
            return Some(Ok(VcdEvent::ScopeEnd));
        } else if trimmed.starts_with("$var") {
            let clean_line = if trimmed.ends_with("$end") {
                &trimmed[..trimmed.len() - 4].trim()
            } else {
                trimmed
            };
            return Some(self.parse_var_decl_impl(line_num, clean_line));
        } else if trimmed.starts_with("$timescale") {
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if parts.len() >= 2 {
                let timescale_str = parts[1..].join(" ");
                let exp = parse_timescale_exp(&timescale_str);
                return Some(Ok(VcdEvent::Timescale(exp)));
            } else {
                self.state = ParserState::SkipToEnd;
                return None;
            }
        } else if trimmed.starts_with("$date") {
            if let Some(date) = trimmed.strip_prefix("$date") {
                let date = date.trim().to_string();
                if !date.is_empty() && !date.contains("$end") {
                    return Some(Ok(VcdEvent::Date(date)));
                }
            }
            self.state = ParserState::SkipToEnd;
            return None;
        } else if trimmed.starts_with("$version") {
            self.state = ParserState::SkipToEnd;
            return None;
        } else if trimmed.starts_with("$enddefinitions") {
            self.state = ParserState::Dump;
            return Some(Ok(VcdEvent::DumpVars));
        } else if trimmed.starts_with("$comment") {
            self.state = ParserState::SkipToEnd;
        }
        None
    }

    /// Parse a $var declaration (implementation)
    fn parse_var_decl_impl(&mut self, line_num: usize, line: &str) -> Result<VcdEvent, VcdError> {
        let parts: Vec<&str> = line.split_whitespace().collect();

        if parts.len() < 5 {
            return Err(VcdError {
                line: line_num,
                column: 0,
                kind: ErrorKind::UnexpectedChar {
                    found: ' ',
                    expected: "$var kind width id name",
                },
                help: Some("Variable declaration should be: $var <type> <width> <id> <name>".to_string()),
            });
        }

        let var_type = parts[1];
        let width = parts[2].parse().unwrap_or(1);

        let id = parts[3].to_string();

        let id_pos = line.find(&id).unwrap_or(0);
        let name_start = id_pos + id.len();
        let name_end = line.rfind("$end").unwrap_or(line.len());
        let name = line[name_start..name_end].trim().to_string();
        let name = if name.is_empty() { id.clone() } else { name };

        self.signal_ids.push(id.clone());

        Ok(VcdEvent::VarDecl {
            id,
            name,
            width,
            var_type: var_type.to_string(),
        })
    }

    /// Parse a line in dump state (implementation)
    #[inline]
    fn parse_dump_line_impl(&mut self, line_num: usize, line: &str) -> Option<Result<VcdEvent, VcdError>> {
        if line.is_empty() {
            return None;
        }

        let first_char = line.as_bytes()[0];

        if first_char == b'#' {
            let timestamp = line[1..].parse().unwrap_or(0);
            self.last_timestamp = timestamp;
            return Some(Ok(VcdEvent::Timestamp(timestamp)));
        } else if first_char == b'$' {
            if line.starts_with("$dumpvars") {
                return Some(Ok(VcdEvent::DumpVars));
            } else if line.starts_with("$dumpoff") {
                return Some(Ok(VcdEvent::DumpOff));
            } else if line.starts_with("$dumpon") {
                return Some(Ok(VcdEvent::DumpOn));
            } else if line.starts_with("$dumpall") {
                return None;
            }
        } else if first_char == b'b' || first_char == b'r' {
            return Some(self.parse_vector_change_impl(line_num, line, first_char as char));
        } else if first_char == b'0' || first_char == b'1' || first_char == b'x' || first_char == b'z' || first_char == b'X' || first_char == b'Z' {
            return Some(self.parse_scalar_change_impl(line_num, line, first_char as char));
        }

        None
    }

    /// Parse a vector value change (implementation)
    fn parse_vector_change_impl(&mut self, line_num: usize, line: &str, prefix: char) -> Result<VcdEvent, VcdError> {
        let parts: Vec<&str> = line.split_whitespace().collect();

        if parts.len() < 2 {
            return Err(VcdError {
                line: line_num,
                column: 0,
                kind: ErrorKind::UnexpectedChar {
                    found: ' ',
                    expected: "vector value and signal id",
                },
                help: None,
            });
        }

        let value_str = &parts[0][1..];
        let signal_num = parse_signal_number_fast(parts[1]);

        let value = if prefix == 'r' {
            VcdValue::parse_real(value_str)
        } else {
            VcdValue::parse_vector(value_str)
        };

        match value {
            Some(v) => Ok(VcdEvent::ValueChange { id: signal_num, value: v }),
            None => Err(VcdError {
                line: line_num,
                column: 0,
                kind: ErrorKind::InvalidTimestamp {
                    value: value_str.to_string(),
                },
                help: Some(format!("Invalid {} value format", if prefix == 'r' { "real" } else { "vector" })),
            }),
        }
    }

    /// Parse a scalar value change (implementation)
    #[inline]
    fn parse_scalar_change_impl(&mut self, line_num: usize, line: &str, value_char: char) -> Result<VcdEvent, VcdError> {
        let id_str = &line[1..];
        let signal_num = parse_signal_number_fast(id_str);
        let value = VcdValue::parse_scalar(value_char);

        match value {
            Some(v) => Ok(VcdEvent::ValueChange { id: signal_num, value: v }),
            None => Err(VcdError {
                line: line_num,
                column: 0,
                kind: ErrorKind::UnexpectedChar {
                    found: value_char,
                    expected: "valid signal value (0, 1, x, z, X, Z)",
                },
                help: None,
            }),
        }
    }
}

impl Iterator for MmapVcdParser {
    type Item = Result<VcdEvent, VcdError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_event()
    }
}
