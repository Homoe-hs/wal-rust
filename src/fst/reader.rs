//! FST file reader
//!
//! Provides reading capabilities for FST (Fast Signal Trace) files.
//! Supports reading compressed FST files with automatic decompression.

use super::types::{FstHeader, ScopeType, SignalDecl, VarType};
use super::varint::decode_varint;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct FstFile {
    pub header: FstHeader,
    pub signals: Vec<SignalDecl>,
    pub scopes: Vec<ScopeInfo>,
}

#[derive(Debug, Clone)]
pub struct ScopeInfo {
    pub name: String,
    #[allow(dead_code)]
    pub scope_type: ScopeType,
    #[allow(dead_code)]
    pub parent_idx: Option<usize>,
}

pub struct FstReader<R: Read + Seek> {
    reader: BufReader<R>,
    pub file: FstFile,
}

impl FstReader<File> {
    pub fn from_path(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        Self::from_reader(file)
    }
}

impl<R: Read + Seek> FstReader<R> {
    pub fn from_reader(reader: R) -> io::Result<Self> {
        let mut r = Self {
            reader: BufReader::new(reader),
            file: FstFile {
                header: FstHeader::default(),
                signals: Vec::new(),
                scopes: Vec::new(),
            },
        };
        r.read_file()?;
        Ok(r)
    }

    fn read_file(&mut self) -> io::Result<()> {
        loop {
            let block_type = match self.read_u8() {
                Ok(b) => b,
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
                Err(e) => return Err(e),
            };

            let block_len = self.read_u64()?;

            match block_type {
                0x00 => self.read_header_block(block_len)?,
                0x01 => self.read_vcdata_block(block_len)?,
                0x02 => self.skip_block(block_len)?,
                0x03 => self.read_geom_block(block_len)?,
                0x04 => self.read_hier_block(block_len)?,
                0x06 => self.read_hier_lz4_block(block_len)?,
                0x07 => self.read_hier_lz4duo_block(block_len)?,
                0xFE => self.read_zwrapper_block(block_len)?,
                _ => self.skip_block(block_len)?,
            }
        }
    }

    fn read_u8(&mut self) -> io::Result<u8> {
        let mut buf = [0u8; 1];
        self.reader.read_exact(&mut buf)?;
        Ok(buf[0])
    }

    fn read_u64(&mut self) -> io::Result<u64> {
        let mut buf = [0u8; 8];
        self.reader.read_exact(&mut buf)?;
        Ok(u64::from_le_bytes(buf))
    }

    fn read_u32(&mut self) -> io::Result<u32> {
        let mut buf = [0u8; 4];
        self.reader.read_exact(&mut buf)?;
        Ok(u32::from_le_bytes(buf))
    }

    fn read_bytes(&mut self, len: usize) -> io::Result<Vec<u8>> {
        let mut buf = vec![0u8; len];
        self.reader.read_exact(&mut buf)?;
        Ok(buf)
    }

    fn skip_block(&mut self, len: u64) -> io::Result<()> {
        let pos = self.reader.stream_position()?;
        self.reader.seek(SeekFrom::Start(pos + len))?;
        Ok(())
    }

    fn read_header_block(&mut self, len: u64) -> io::Result<()> {
        let start_pos = self.reader.stream_position()?;
        self.file.header.start_time = self.read_u64()?;
        self.file.header.end_time = self.read_u64()?;
        let _endian_check = self.read_u64()?;
        let _mem_used = self.read_u64()?;
        let _scope_count = self.read_u64()?;
        let _var_count = self.read_u64()?;
        let _max_handle = self.read_u64()?;
        let _vc_count = self.read_u64()?;
        self.file.header.timescale_exp = self.read_i8()?;

        let mut version = vec![0u8; 128];
        self.reader.read_exact(&mut version)?;
        if let Some(pos) = version.iter().position(|&b| b == 0) {
            version.truncate(pos);
        }
        self.file.header.version = String::from_utf8_lossy(&version).to_string();

        let mut date = vec![0u8; 128];
        self.reader.read_exact(&mut date)?;
        if let Some(pos) = date.iter().position(|&b| b == 0) {
            date.truncate(pos);
        }
        self.file.header.date = String::from_utf8_lossy(&date).to_string();

        // FST header block has a fixed structure: 8 u64 + 1 i8 + 128 version + 128 date = 321 bytes
        // Some FST generators (e.g. Icarus Verilog) write incorrect block_len values (e.g. 1 instead of 321).
        // Always advance past the actual header body (max(321, len) bytes) to handle both cases.
        const HEADER_BODY_SIZE: u64 = 321; // bytes of fixed header fields
        let header_end = if len < HEADER_BODY_SIZE {
            // Some implementations (Icarus) report block_len < actual header size
            start_pos + 8 + HEADER_BODY_SIZE // 8 = block_len field itself
        } else {
            start_pos + len
        };
        let current_pos = self.reader.stream_position()?;
        if current_pos < header_end {
            self.reader.seek(SeekFrom::Start(header_end))?;
        } else if current_pos > header_end {
            // Hard seek forward past any trailing data in the header
            self.reader.seek(SeekFrom::Start(header_end))?;
        }
        Ok(())
    }

    fn read_i8(&mut self) -> io::Result<i8> {
        let mut buf = [0u8; 1];
        self.reader.read_exact(&mut buf)?;
        Ok(buf[0] as i8)
    }

    fn read_geom_block(&mut self, len: u64) -> io::Result<()> {
        let start_pos = self.reader.stream_position()?;
        let _section_length = self.read_u64()?;
        let _uncompressed_length = self.read_u64()?;
        let _max_handle = self.read_u64()?;

        let end_pos = start_pos + len as u64;
        while self.reader.stream_position()? < end_pos {
            let handle = self.read_u32()?;
            let (name_len, _) = decode_varint(&self.read_varint_bytes()?)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Invalid varint"))?;
            let name_bytes = self.read_bytes(name_len as usize)?;
            let name = String::from_utf8_lossy(&name_bytes).to_string();
            let var_type = self.read_u8()?;
            let _direction = self.read_u8()?;
            let width = self.read_u32()?;

            self.file.signals.push(SignalDecl {
                handle,
                name,
                width,
                var_type: VarType::from_u8(var_type),
            });
        }

        if self.reader.stream_position()? != end_pos {
            self.reader.seek(SeekFrom::Start(end_pos))?;
        }

        Ok(())
    }

    fn read_hier_block(&mut self, len: u64) -> io::Result<()> {
        let data = self.read_bytes(len as usize)?;
        self.parse_hier_data(&data)
    }

    fn read_hier_lz4_block(&mut self, len: u64) -> io::Result<()> {
        let compressed = self.read_bytes(len as usize)?;
        let decompressed = lz4_flex::block::decompress_size_prepended(&compressed)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("LZ4 decompression failed: {}", e)))?;
        self.parse_hier_data(&decompressed)
    }

    fn read_hier_lz4duo_block(&mut self, len: u64) -> io::Result<()> {
        self.read_hier_lz4_block(len)
    }

    fn parse_hier_data(&mut self, data: &[u8]) -> io::Result<()> {
        let mut pos = 0;
        let mut scope_stack: Vec<usize> = Vec::new();

        while pos < data.len() {
            let code = data[pos];
            pos += 1;

            match code {
                0x01 => {
                    let scope_type = data[pos] as u8;
                    pos += 1;
                    let (name, consumed) = self.read_cstring_from_slice(&data[pos..]);
                    pos += consumed;
                    let parent_idx = scope_stack.last().copied();
                    self.file.scopes.push(ScopeInfo {
                        name,
                        scope_type: ScopeType::from_u8(scope_type),
                        parent_idx,
                    });
                    scope_stack.push(self.file.scopes.len() - 1);
                }
                0x02 => {
                    let var_type = data[pos] as u8;
                    pos += 1;
                    let _direction = data[pos];
                    pos += 1;
                    let (name, consumed) = self.read_cstring_from_slice(&data[pos..]);
                    pos += consumed;
                    let handle = u32::from_le_bytes(
                        data[pos..pos + 4].try_into().unwrap()
                    );
                    pos += 4;
                    let width = u32::from_le_bytes(
                        data[pos..pos + 4].try_into().unwrap()
                    );
                    pos += 4;

                    if let Some(sig) = self.file.signals.iter_mut().find(|s| s.handle == handle) {
                        sig.var_type = VarType::from_u8(var_type);
                        sig.name = name;
                        sig.width = width;
                    }
                }
                0x03 => {
                    if !scope_stack.is_empty() {
                        scope_stack.pop();
                    }
                }
                _ => {
                    break;
                }
            }
        }

        Ok(())
    }

    fn read_cstring_from_slice<'a>(&self, data: &'a [u8]) -> (String, usize) {
        let mut pos = 0;
        while pos < data.len() && data[pos] != 0 {
            pos += 1;
        }
        let name = String::from_utf8_lossy(&data[..pos]).to_string();
        (name, pos + 1)
    }

    fn read_vcdata_block(&mut self, len: u64) -> io::Result<()> {
        self.skip_block(len)
    }

    fn read_zwrapper_block(&mut self, len: u64) -> io::Result<()> {
        let compressed = self.read_bytes(len as usize)?;

        let decompressed = {
            use flate2::read::ZlibDecoder;
            use std::io::Read;
            let mut decoder = ZlibDecoder::new(&compressed[..]);
            let mut output = Vec::new();
            decoder.read_to_end(&mut output)?;
            output
        };

        let mut cursor = std::io::Cursor::new(decompressed);
        let inner_reader = FstReader::from_reader(&mut cursor)?;

        self.file.header = inner_reader.file.header;
        self.file.signals.extend(inner_reader.file.signals);
        self.file.scopes.extend(inner_reader.file.scopes);

        Ok(())
    }

    fn read_varint_bytes(&mut self) -> io::Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(10);
        loop {
            let b = self.read_u8()?;
            buf.push(b);
            if b & 0x80 == 0 {
                break;
            }
        }
        Ok(buf)
    }

    #[allow(dead_code)]
    fn read_cstring(&mut self) -> io::Result<String> {
        let mut buf = Vec::new();
        loop {
            let b = self.read_u8()?;
            if b == 0 {
                break;
            }
            buf.push(b);
        }
        Ok(String::from_utf8_lossy(&buf).to_string())
    }
}

impl VarType {
    pub fn from_u8(v: u8) -> Self {
        match v {
            16 => VarType::VcdWire,
            5 => VarType::VcdReg,
            18 => VarType::VcdPort,
            29 => VarType::Integer,
            3 => VarType::Real,
            21 => VarType::GenString,
            22 => VarType::SvBit,
            23 => VarType::SvLogic,
            24 => VarType::SvInt,
            _ => VarType::VcdWire,
        }
    }
}

impl ScopeType {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => ScopeType::VcdModule,
            1 => ScopeType::VcdTask,
            2 => ScopeType::VcdFunction,
            3 => ScopeType::VcdBegin,
            4 => ScopeType::VcdFork,
            5 => ScopeType::VcdGenerate,
            _ => ScopeType::VcdModule,
        }
    }
}

impl FstFile {
    pub fn signal_names(&self) -> Vec<String> {
        self.signals.iter().map(|s| s.name.clone()).collect()
    }

    pub fn signal_by_name(&self, name: &str) -> Option<&SignalDecl> {
        self.signals.iter().find(|s| s.name == name)
    }

    #[allow(dead_code)]
    pub fn signal_by_handle(&self, handle: u32) -> Option<&SignalDecl> {
        self.signals.iter().find(|s| s.handle == handle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vartype_from_u8() {
        assert_eq!(VarType::from_u8(16), VarType::VcdWire);
        assert_eq!(VarType::from_u8(3), VarType::Real);
        assert_eq!(VarType::from_u8(99), VarType::VcdWire);
    }

    #[test]
    fn test_scopetype_from_u8() {
        assert_eq!(ScopeType::from_u8(0), ScopeType::VcdModule);
        assert_eq!(ScopeType::from_u8(5), ScopeType::VcdGenerate);
        assert_eq!(ScopeType::from_u8(99), ScopeType::VcdModule);
    }
}