//! FST file writer
//!
//! High-level interface for creating FST files with:
//! - Block-based output (automatic flushing)
//! - Signal management
//! - Scope hierarchy
//! - Value change emission

use super::blocks::{encode_var_entry, BlockWriter};
use super::compress::{get_compressor, Compressor};
use super::types::{BlockType, Compression, FstHeader, ScopeType, SignalDecl, VarType};
use super::varint::encode_varint_buf;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Result, Write};
use std::path::Path;

/// FST writer options
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct FstOptions {
    pub compression: Compression,
    pub block_size: usize, // bytes
}

impl Default for FstOptions {
    fn default() -> Self {
        Self {
            compression: Compression::Lz4,
            block_size: 64 * 1024 * 1024, // 64MB
        }
    }
}

/// Statistics after closing FST file
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct FstStats {
    pub output_bytes: u64,
    pub blocks_written: usize,
    pub compression_ratio: f64,
    pub signals: usize,
    pub scopes: usize,
    pub timestamps: usize,
    pub value_changes: usize,
}

/// FST file writer
#[allow(dead_code)]
pub struct FstWriter<W: Write> {
    writer: BufWriter<W>,
    opts: FstOptions,
    compressor: Box<dyn Compressor>,

    // File state
    header: FstHeader,
    scopes: Vec<String>,
    signals: Vec<SignalDecl>,
    name_to_handle: BTreeMap<String, u32>,

    // Current block being built
    current_block_data: Vec<u8>,
    current_block_time_begin: u64,
    #[allow(dead_code)]
    current_block_changes: usize,

    // Statistics
    bytes_written: u64,
    blocks_written: usize,
    timestamps_count: usize,
    value_changes_count: usize,
}

impl FstWriter<File> {
    /// Create a new FST file writer
    #[allow(dead_code)]
    pub fn create(path: &Path, opts: FstOptions) -> Result<Self> {
        let file = File::create(path)?;
        Self::from_writer(file, opts)
    }
}

#[allow(dead_code)]
impl<W: Write> FstWriter<W> {
    /// Create from an existing writer
    pub fn from_writer(writer: W, opts: FstOptions) -> Result<Self> {
        let compressor = get_compressor(opts.compression);
        let mut w = Self {
            writer: BufWriter::new(writer),
            opts: opts.clone(),
            compressor,
            header: FstHeader::default(),
            scopes: Vec::new(),
            signals: Vec::new(),
            name_to_handle: BTreeMap::new(),
            current_block_data: Vec::with_capacity(1024 * 1024),
            current_block_time_begin: 0,
            current_block_changes: 0,
            bytes_written: 0,
            blocks_written: 0,
            timestamps_count: 0,
            value_changes_count: 0,
        };

        // Write placeholder header block
        w.write_header_block()?;
        Ok(w)
    }

    /// Create a variable in the FST file
    pub fn create_var(&mut self, name: &str, width: u32, vartype: VarType) -> u32 {
        let handle = (self.signals.len() + 1) as u32;
        let full_name = if self.scopes.is_empty() {
            name.to_string()
        } else {
            format!("{}.{}", self.scopes.join("."), name)
        };

        self.name_to_handle.insert(full_name.clone(), handle);
        self.signals.push(SignalDecl {
            handle,
            name: full_name,
            width,
            var_type: vartype,
        });

        handle
    }

    /// Get handle for a signal by name
    pub fn get_handle(&self, name: &str) -> Option<u32> {
        self.name_to_handle.get(name).copied()
    }

    /// Push a scope onto the hierarchy
    pub fn push_scope(&mut self, name: &str, _st: ScopeType) {
        let full_name = if self.scopes.is_empty() {
            name.to_string()
        } else {
            format!("{}.{}", self.scopes.join("."), name)
        };
        self.scopes.push(full_name);
    }

    /// Pop the current scope
    pub fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// Set the timescale exponent (10^n seconds)
    pub fn set_timescale(&mut self, exp: i8) {
        self.header.timescale_exp = exp;
    }

    /// Set the date string
    pub fn set_date(&mut self, date: &str) {
        self.header.date = date.to_string();
    }

    /// Set the version string
    pub fn set_version(&mut self, version: &str) {
        self.header.version = version.to_string();
    }

    /// Emit a time change
    pub fn emit_time_change(&mut self, timestamp: u64) {
        if self.current_block_data.is_empty() {
            self.current_block_time_begin = timestamp;
            self.header.end_time = timestamp;
        } else if timestamp != self.header.end_time {
            // Emit time delta before next value change
            let mut time_buf = [0u8; 10];
            let delta = timestamp.saturating_sub(self.header.end_time);
            let len = encode_varint_buf(delta, &mut time_buf);
            self.current_block_data.extend_from_slice(&time_buf[..len]);
            self.header.end_time = timestamp;
        }
        self.timestamps_count += 1;
    }

    /// Emit a value change for a signal
    #[inline]
    pub fn emit_value_change(&mut self, handle: u32, value: &[u8]) {
        // Format: [handle:varint][len:u8][data:bytes]
        // For single-bit values (1 byte), len is just 1 byte
        let mut varint_buf = [0u8; 10];
        let varint_len = encode_varint_buf(handle as u64, &mut varint_buf);

        self.current_block_data.extend_from_slice(&varint_buf[..varint_len]);
        self.current_block_data.push(value.len() as u8);
        self.current_block_data.extend_from_slice(value);
        self.value_changes_count += 1;

        // Check if block is full
        if self.current_block_data.len() >= self.opts.block_size {
            self.flush_block().unwrap();
        }
    }

    /// Flush current block to disk
    pub fn flush_block(&mut self) -> Result<()> {
        if self.current_block_data.is_empty() {
            return Ok(());
        }

        // Compress the block data
        let compressed = self.compressor.compress(&self.current_block_data);
        let uncompressed_len = self.current_block_data.len();
        let compressed_len = compressed.len();

        // Build VCDATA block
        let mut block = BlockWriter::new(BlockType::VcData);
        block.write_vcdata_header(
            self.current_block_time_begin,
            self.header.end_time,
            uncompressed_len as u64,
            compressed_len as u64,
            self.signals.len() as u64,
        );
        block.write_bytes(&compressed);

        let block_bytes = block.finalize();
        self.writer.write_all(&block_bytes)?;
        self.bytes_written += block_bytes.len() as u64;
        self.blocks_written += 1;
        self.current_block_data.clear();

        Ok(())
    }

    /// Close the FST file and return statistics
    pub fn close(mut self) -> Result<FstStats> {
        // Flush any remaining data
        self.flush_block()?;

        // Update end time in header if we have timestamps
        if self.timestamps_count > 0 {
            // We need to track the last timestamp - for now just close
        }

        // Write geometry block
        self.write_geom_block()?;

        // Write hierarchy block
        self.write_hier_block()?;

        self.writer.flush()?;

        let ratio = if self.bytes_written > 0 {
            self.bytes_written as f64 / self.bytes_written.max(1) as f64
        } else {
            1.0
        };

        Ok(FstStats {
            output_bytes: self.bytes_written,
            blocks_written: self.blocks_written,
            compression_ratio: ratio,
            signals: self.signals.len(),
            scopes: self.scopes.len(),
            timestamps: self.timestamps_count,
            value_changes: self.value_changes_count,
        })
    }

    /// Write header block (placeholder, will be overwritten on close)
    fn write_header_block(&mut self) -> Result<()> {
        let mut block = BlockWriter::new(BlockType::Hdr);
        block.write_header(&self.header, 0, 0, 0, 0);
        let block_bytes = block.finalize();
        self.writer.write_all(&block_bytes)?;
        self.bytes_written += block_bytes.len() as u64;
        self.blocks_written += 1;
        Ok(())
    }

    /// Write geometry block
    fn write_geom_block(&mut self) -> Result<()> {
        let mut block = BlockWriter::new(BlockType::Geom);
        block.write_geom(&self.signals, self.signals.len() as u64);
        let block_bytes = block.finalize();
        self.writer.write_all(&block_bytes)?;
        self.bytes_written += block_bytes.len() as u64;
        self.blocks_written += 1;
        Ok(())
    }

    /// Write hierarchy block
    fn write_hier_block(&mut self) -> Result<()> {
        let mut hier_data = Vec::new();

        // Encode scopes and variables
        for sig in &self.signals {
            hier_data.extend_from_slice(&encode_var_entry(
                sig.handle,
                &sig.name,
                sig.var_type,
                sig.width,
            ));
        }

        // Compress hierarchy
        let compressed = self.compressor.compress(&hier_data);

        let mut block = BlockWriter::new(BlockType::HierLz4);
        block.write_bytes(&compressed);
        let block_bytes = block.finalize();

        self.writer.write_all(&block_bytes)?;
        self.bytes_written += block_bytes.len() as u64;
        self.blocks_written += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_basic_write() {
        let buffer = Cursor::new(Vec::new());
        let mut writer = FstWriter::from_writer(buffer, FstOptions::default()).unwrap();

        writer.set_timescale(-9); // 1ns
        writer.push_scope("top", ScopeType::VcdModule);

        let clk_handle = writer.create_var("clk", 1, VarType::VcdWire);
        let data_handle = writer.create_var("data", 8, VarType::VcdWire);

        writer.pop_scope();

        // Emit some value changes
        writer.emit_time_change(0);
        writer.emit_value_change(clk_handle, &[b'0']);
        writer.emit_value_change(data_handle, &[b'0', b'0', b'0', b'0', b'0', b'0', b'0', b'0']);

        let stats = writer.close().unwrap();

        assert_eq!(stats.signals, 2);
        assert_eq!(stats.scopes, 0); // popped
        assert!(stats.timestamps >= 1);
    }
}
