//! FST file format types and constants

use std::fmt;

/// FST block types as defined in the format specification
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum BlockType {
    /// File header block
    Hdr = 0x00,
    /// Value change data block
    VcData = 0x01,
    /// Blackout (dump on/off) markers
    Blackout = 0x02,
    /// Geometry block (signal metadata)
    Geom = 0x03,
    /// Hierarchy block (scope/variable info)
    Hier = 0x04,
    /// VCD data with dynamic aliasing
    VcDataDynAlias = 0x05,
    /// LZ4 compressed hierarchy
    HierLz4 = 0x06,
    /// Double LZ4 compressed hierarchy
    HierLz4Duo = 0x07,
    /// Extended VCD data format
    VcDataDynAlias2 = 0x08,
    /// Compressed wrapper over whole file
    ZWrapper = 0xFE,
    /// Invalid block type marker
    Bad = 0xFF,
}

impl fmt::Display for BlockType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BlockType::Hdr => write!(f, "HDR"),
            BlockType::VcData => write!(f, "VCDATA"),
            BlockType::Blackout => write!(f, "BLACKOUT"),
            BlockType::Geom => write!(f, "GEOM"),
            BlockType::Hier => write!(f, "HIER"),
            BlockType::VcDataDynAlias => write!(f, "VCDATA_DYN_ALIAS"),
            BlockType::HierLz4 => write!(f, "HIER_LZ4"),
            BlockType::HierLz4Duo => write!(f, "HIER_LZ4DUO"),
            BlockType::VcDataDynAlias2 => write!(f, "VCDATA_DYN_ALIAS2"),
            BlockType::ZWrapper => write!(f, "ZWRAPPER"),
            BlockType::Bad => write!(f, "BAD"),
        }
    }
}

/// FST file header
#[derive(Debug, Clone)]
pub struct FstHeader {
    pub start_time: u64,
    pub end_time: u64,
    pub timescale_exp: i8,
    pub version: String,
    pub date: String,
}

impl Default for FstHeader {
    fn default() -> Self {
        Self {
            start_time: 0,
            end_time: 0,
            timescale_exp: -9, // 1ns default
            version: "walconv 0.1.0".to_string(),
            date: chrono_lite_date(),
        }
    }
}

fn chrono_lite_date() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let days = secs / 86400;
    let year = 1970 + days / 365;
    let yday = days % 365;
    let month = yday / 30 + 1;
    let day = yday % 30 + 1;
    format!("{:04}-{:02}-{:02}", year, month, day)
}

/// Variable type in FST
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarType {
    /// VCD wire
    VcdWire = 16,
    /// VCD reg
    VcdReg = 5,
    /// VCD port
    VcdPort = 18,
    /// Integer
    Integer = 29,
    /// Real number
    Real = 3,
    /// String
    GenString = 21,
    /// SystemVerilog bit
    SvBit = 22,
    /// SystemVerilog logic
    SvLogic = 23,
    /// SystemVerilog integer
    SvInt = 24,
}

impl VarType {
    #[allow(dead_code)]
    pub fn from_vcd_type(vcd_type: &str, width: u32) -> Self {
        match vcd_type {
            "wire" => VarType::VcdWire,
            "reg" => VarType::VcdReg,
            "port" => VarType::VcdPort,
            "integer" => VarType::Integer,
            "real" => VarType::Real,
            "string" => VarType::GenString,
            _ if width == 1 && vcd_type.contains("bit") => VarType::SvBit,
            _ if vcd_type.contains("logic") || vcd_type.contains("int") => VarType::SvLogic,
            _ => VarType::VcdWire,
        }
    }
}

/// Scope type in FST
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeType {
    /// VCD module
    VcdModule = 0,
    /// VCD task
    VcdTask = 1,
    /// VCD function
    VcdFunction = 2,
    /// VCD begin block
    VcdBegin = 3,
    /// VCD fork
    VcdFork = 4,
    /// VCD generate
    VcdGenerate = 5,
}

impl ScopeType {
    #[allow(dead_code)]
    pub fn from_vcd_kind(kind: &str) -> Self {
        match kind {
            "module" => ScopeType::VcdModule,
            "task" => ScopeType::VcdTask,
            "function" => ScopeType::VcdFunction,
            "begin" => ScopeType::VcdBegin,
            "fork" => ScopeType::VcdFork,
            "generate" => ScopeType::VcdGenerate,
            _ => ScopeType::VcdModule,
        }
    }
}

/// Signal declaration
#[derive(Debug, Clone)]
pub struct SignalDecl {
    pub handle: u32,
    pub name: String,
    pub width: u32,
    pub var_type: VarType,
}

/// Compression algorithm selection
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Compression {
    /// LZ4 - fastest, ~6GB/s
    Lz4,
    /// zlib - balanced, ~500MB/s
    Zlib,
    /// FastLZ - lightweight alternative
    FastLz,
}

impl Default for Compression {
    fn default() -> Self {
        Compression::Lz4
    }
}
