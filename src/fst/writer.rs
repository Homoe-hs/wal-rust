//! FST file writer
//!
//! High-level interface for creating FST files with:
//! - Block-based output (automatic flushing)
//! - Signal management
//! - Scope hierarchy
//! - Value change emission

use super::blocks::BlockWriter;
use super::types::{BlockType, Compression, FstHeader, ScopeType, SignalDecl, VarType};
use super::varint::{encode_varint, encode_fst_svarint};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Result, Write};
use std::path::Path;

struct BlockEntry {
    handle: u32,
    time_idx: u32,
    value: Vec<u8>,
}

/// Standard FST hierarchy entry, recorded in emission order (per fstapi)
enum HierEvent {
    Scope { scope_type: ScopeType, name: String },
    Upscope,
    Var { name: String, width: u32, var_type: VarType },
}

/// FST_RCV_STR — lookup for multi-state values, from fstapi.c
const FST_RCV_STR: &[u8] = b"xzhuwl-?";

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

    // File state
    header: FstHeader,
    scopes: Vec<String>,
    signals: Vec<SignalDecl>,
    name_to_handle: BTreeMap<String, u32>,
    /// Standard hierarchy entries (scopes + vars) in emission order
    hier_events: Vec<HierEvent>,
    /// Current value per signal (for checkpoints), updated on emit
    current_values: Vec<Vec<u8>>,
    /// Value snapshot at the start of the current block (checkpoint data)
    block_checkpoint: Vec<Vec<u8>>,

    // Current block being built (0x08 VcDataDynAlias2 format)
    block_timestamps: Vec<u64>,
    block_entries: Vec<BlockEntry>,
    current_block_time_begin: u64,

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
        let mut w = Self {
            writer: BufWriter::new(writer),
            opts: opts.clone(),
            header: FstHeader::default(),
            scopes: Vec::new(),
            signals: Vec::new(),
            name_to_handle: BTreeMap::new(),
            hier_events: Vec::new(),
            current_values: Vec::new(),
            block_checkpoint: Vec::new(),
            block_timestamps: Vec::with_capacity(1024 * 1024 / 32),
            block_entries: Vec::with_capacity(1024 * 1024 / 16),
            current_block_time_begin: 0,
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
            direction: 0,
        });
        self.hier_events.push(HierEvent::Var {
            name: name.to_string(),
            width,
            var_type: vartype,
        });
        // Initial value per fstapi: all-x
        self.current_values.push(vec![b'x'; width as usize]);
        self.block_checkpoint.push(vec![b'x'; width as usize]);

        handle
    }

    /// Get handle for a signal by name
    pub fn get_handle(&self, name: &str) -> Option<u32> {
        self.name_to_handle.get(name).copied()
    }

    /// Push a scope onto the hierarchy
    pub fn push_scope(&mut self, name: &str, st: ScopeType) {
        self.scopes.push(name.to_string());
        self.hier_events.push(HierEvent::Scope {
            scope_type: st,
            name: name.to_string(),
        });
    }

    /// Pop the current scope
    pub fn pop_scope(&mut self) {
        self.scopes.pop();
        self.hier_events.push(HierEvent::Upscope);
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
        if self.block_timestamps.is_empty() {
            self.current_block_time_begin = timestamp;
        }
        if self.block_timestamps.last() != Some(&timestamp) {
            self.block_timestamps.push(timestamp);
        }
        self.header.end_time = timestamp;
        self.timestamps_count += 1;
    }

    /// Return current time index (position within current block's timestamp list)
    fn current_time_idx(&self) -> u32 {
        self.block_timestamps.len().saturating_sub(1) as u32
    }

    /// Emit a value change for a signal
    #[inline]
    pub fn emit_value_change(&mut self, handle: u32, value: &[u8]) {
        let time_idx = self.current_time_idx();
        // Normalize to the declared width (VCD semantics: value is left-padded with '0')
        let width = self.signals
            .get(handle.saturating_sub(1) as usize)
            .map(|s| s.width as usize)
            .unwrap_or_else(|| value.len());
        let mut v = value.to_vec();
        if v.len() < width {
            let mut padded = vec![b'0'; width - v.len()];
            padded.extend_from_slice(&v);
            v = padded;
        } else if v.len() > width {
            v.drain(..v.len() - width);
        }

        // Snapshot the checkpoint at the start of each block
        if self.block_entries.is_empty() {
            self.block_checkpoint = self.current_values.clone();
        }
        if let Some(cur) = self.current_values.get_mut(handle.saturating_sub(1) as usize) {
            *cur = v.clone();
        }

        self.block_entries.push(BlockEntry {
            handle,
            time_idx,
            value: v,
        });
        self.value_changes_count += 1;

        let est = self.block_entries.len() * 16 + self.block_timestamps.len() * 8;
        if est >= self.opts.block_size {
            self.flush_block().unwrap_or(());
        }
    }

    /// Build and write a 0x08 VcDataDynAlias2 block.
    /// Layout: [header24] [checkpoint] [vc_header] [chain_data] [index] [time_section]
    fn build_block_08(&mut self) -> Result<Vec<u8>> {
        let max_handle = self.signals.len() as u32;
        let timestamps = std::mem::take(&mut self.block_timestamps);
        let entries = std::mem::take(&mut self.block_entries);

        if timestamps.is_empty() {
            return Ok(Vec::new());
        }

        // -- 1. Time section --
        let mut time_raw = Vec::new();
        let mut prev = 0u64;
        for &ts in &timestamps {
            let delta = ts.saturating_sub(prev);
            time_raw.extend_from_slice(&encode_varint(delta));
            prev = ts;
        }
        use flate2::write::ZlibEncoder;
        use flate2::Compression as ZlComp;
        use std::io::Write;
        let mut zlib = ZlibEncoder::new(Vec::new(), ZlComp::best());
        zlib.write_all(&time_raw).unwrap();
        let time_comp = zlib.finish().unwrap();
        let tsec_uclen = time_raw.len() as u64;
        let tsec_clen = time_comp.len() as u64;
        let tsec_nitems = timestamps.len() as u64;
        drop(timestamps);

        // -- 2. Per-signal chain data --
        // Build chain entries per signal: for each handle, compute tdelta between consecutive
        // value changes and encode using the FST chain format.
        let max_h = max_handle as usize;
        let mut chain_raw: Vec<Vec<u8>> = vec![Vec::new(); max_h + 1];
        let mut chain_has_data = vec![false; max_h + 1];
        let mut chain_last_tidx: Vec<u32> = vec![0; max_h + 1];

        for ent in &entries {
            let h = ent.handle as usize;
            if h > max_h { continue; }

            let width = self.signals.get(h.saturating_sub(1))
                .map(|s| s.width)
                .unwrap_or(1);

            let tdelta = ent.time_idx.saturating_sub(chain_last_tidx[h]);
            chain_last_tidx[h] = ent.time_idx;
            chain_has_data[h] = true;

            // Encode chain entry
            let val = &ent.value;
            if width <= 1 && val.len() <= 1 {
                let byte = *val.first().unwrap_or(&b'x');
                if byte == b'0' || byte == b'1' {
                    // Binary scalar: LSB=0, val_bit=(vli>>1)&1, tdelta=vli>>2
                    let val_bit = if byte == b'1' { 1u64 } else { 0 };
                    let vli = (tdelta as u64) << 2 | (val_bit << 1) | 0;
                    chain_raw[h].extend_from_slice(&encode_varint(vli));
                } else {
                    // Multi-state scalar: LSB=1, val=FST_RCV_STR[(vli>>1)&7], tdelta=vli>>4
                    let val_idx = FST_RCV_STR.iter().position(|&c| c == byte).unwrap_or(0) as u64;
                    let vli = (tdelta as u64) << 4 | (val_idx << 1) | 1;
                    chain_raw[h].extend_from_slice(&encode_varint(vli));
                }
            } else {
                // Vector: check if all binary
                let all_binary = val.iter().all(|&b| b == b'0' || b == b'1');
                if all_binary {
                    // Binary vector: LSB=0, tdelta=vli>>1, followed by bit-packed bytes
                    let vli = (tdelta as u64) << 1 | 0;
                    chain_raw[h].extend_from_slice(&encode_varint(vli));
                    // Bit-pack: MSB of first byte is first bit of signal
                    let byte_count = ((width as usize) + 7) / 8;
                    let mut packed = vec![0u8; byte_count];
                    for (j, &b) in val.iter().enumerate() {
                        if b == b'1' {
                            let bit = 7 - (j & 7);
                            packed[j / 8] |= 1 << bit;
                        }
                    }
                    chain_raw[h].extend_from_slice(&packed);
                } else {
                    // Non-binary vector: LSB=1, tdelta=vli>>1, then exactly `width` literal chars
                    // (per fstapi: the value is written raw, no length prefix)
                    let vli = (tdelta as u64) << 1 | 1;
                    chain_raw[h].extend_from_slice(&encode_varint(vli));
                    let mut v = val.clone();
                    if v.len() < width as usize {
                        let mut padded = vec![b'0'; width as usize - v.len()];
                        padded.extend_from_slice(&v);
                        v = padded;
                    }
                    v.truncate(width as usize);
                    chain_raw[h].extend_from_slice(&v);
                }
            }
        }

        // Build per-signal compressed chain data and track offsets
        let mut chain_offsets: Vec<u64> = vec![0; max_h + 1];
        let mut chain_lengths: Vec<i64> = vec![0; max_h + 1];
        let mut chain_data_area = Vec::new();
        let mut mem_required: u64 = 0;

        for h in 0..=max_h {
            if !chain_has_data[h] {
                continue;
            }
            let raw = std::mem::take(&mut chain_raw[h]);
            mem_required += raw.len() as u64;

            let compressed = {
                let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
                z.write_all(&raw).unwrap();
                z.finish().unwrap()
            };
            let destlen = raw.len() as u64;

            // Offsets are relative to vc_start (the packtype byte occupies offset 0)
            chain_offsets[h] = chain_data_area.len() as u64 + 1;
            let dest_buf = encode_varint(destlen);
            chain_data_area.extend_from_slice(&dest_buf);
            chain_data_area.extend_from_slice(&compressed);
            chain_lengths[h] = (chain_data_area.len() as u64 - chain_offsets[h]) as i64;
        }

        // -- 3. Index table (DYNALIAS2) --
        // One entry per signal handle (1..=max_handle), per fstapi fstWriterFlushContextPrivate:
        //   even (LSB=0) = skip N handles with no data: varint(N << 1)
        //   odd + positive = chain offset delta: svarint((delta << 1) | 1)
        //   odd + zero = repeat previous alias: svarint(1)
        //   odd + negative = alias: svarint(((-alias-1) << 1) | 1)
        // Signed varints are sign-extended LEB128.
        let mut idx_buf = Vec::new();
        let mut pval: u64 = 0;
        let mut h = 1usize; // handles are 1-based
        while h <= max_h {
            if !chain_has_data[h] {
                // Skip: count how many consecutive handles without data
                let mut skip = 0usize;
                while h <= max_h && !chain_has_data[h] {
                    skip += 1;
                    h += 1;
                }
                if skip > 0 {
                    idx_buf.extend_from_slice(&encode_varint((skip as u64) << 1));
                }
                continue;
            }
            // Real chain: delta from previous chain offset
            let raw_delta = chain_offsets[h].saturating_sub(pval);
            pval = chain_offsets[h];
            let sval = (raw_delta as i64) << 1 | 1;
            idx_buf.extend_from_slice(&encode_fst_svarint(sval));
            h += 1;
        }

        // The last chain's length: from its offset to end of chain area
        // Chain length = chain_data_area.len() - chain_offset(last_chain)

        let chain_clen = idx_buf.len() as u64;

        // -- 4. Checkpoint (per fstapi fstWriterEmitSectionHeader):
        // [maxvalpos varint][frame_clen varint][frame_maxhandle varint][values]
        // values = one entry per signal: `width` literal chars (real: 8-byte f64 NaN)
        let mut cp_data = Vec::new();
        for (i, sig) in self.signals.iter().enumerate() {
            if matches!(sig.var_type, VarType::Real) {
                cp_data.extend_from_slice(&f64::NAN.to_le_bytes());
            } else if sig.width > 0 {
                let v = self.block_checkpoint.get(i)
                    .cloned()
                    .unwrap_or_else(|| vec![b'x'; sig.width as usize]);
                cp_data.extend_from_slice(&v);
            }
            // variable-length signals (width 0) have no checkpoint value
        }
        let maxvalpos = cp_data.len() as u64;
        let mut cp_buf = Vec::new();
        cp_buf.extend_from_slice(&encode_varint(maxvalpos)); // maxvalpos = uncompressed length
        cp_buf.extend_from_slice(&encode_varint(maxvalpos)); // frame_clen (raw = uncompressed)
        cp_buf.extend_from_slice(&encode_varint(max_handle as u64)); // frame_maxhandle
        cp_buf.extend_from_slice(&cp_data);

        // -- 5. VC header --
        let mut vc_buf = Vec::new();
        vc_buf.extend_from_slice(&encode_varint(max_handle as u64)); // vc_maxhandle
        vc_buf.push(b'Z'); // packtype: zlib

        // -- 6. Assemble --
        let mut body = Vec::with_capacity(
            24 + cp_buf.len() + vc_buf.len() + chain_data_area.len() + 8 + idx_buf.len() + 8
            + time_comp.len() + 24,
        );

        // HDR24: begin_time, end_time, mem_required (big-endian per FST format)
        body.extend_from_slice(&self.current_block_time_begin.to_be_bytes());
        body.extend_from_slice(&self.header.end_time.to_be_bytes());
        body.extend_from_slice(&mem_required.to_be_bytes());

        // Checkpoint
        body.extend_from_slice(&cp_buf);

        // VC header
        body.extend_from_slice(&vc_buf);

        // Chain data
        body.extend_from_slice(&chain_data_area);

        // Index table
        body.extend_from_slice(&idx_buf);
        body.extend_from_slice(&chain_clen.to_be_bytes());

        // Time section
        body.extend_from_slice(&time_comp);
        // Trailer (big-endian per FST format)
        body.extend_from_slice(&tsec_uclen.to_be_bytes());
        body.extend_from_slice(&tsec_clen.to_be_bytes());
        body.extend_from_slice(&tsec_nitems.to_be_bytes());

        // Now fix up the last chain's length: chain_lengths[last_real] = chain_data_area end - its offset
        // This is used by the reader's "chain_table_lengths[last] = indx_pntr - 8 - vc_start - chain_table[last]"
        // but since we write the chain data fully, the reader's formula should still work:
        // chain_end = indx_pntr - 8 - vc_start = indx_pos - vc_start = total chain data bytes
        // So the last chain length is implicitly computed from (indx_pos - chain_offsets[last])

        // Build final block: [0x08][section_length:be u64][body]
        // section_length covers the whole section after the type byte, including
        // the length field itself (per fstapi: endpos - section_start)
        let section_length = body.len() as u64 + 8;
        let mut block = Vec::with_capacity(1 + 8 + body.len());
        block.push(0x08);
        block.extend_from_slice(&section_length.to_be_bytes());
        block.extend_from_slice(&body);

        Ok(block)
    }

    /// Flush current block to disk (0x08 VcDataDynAlias2 format)
    pub fn flush_block(&mut self) -> Result<()> {
        if self.block_entries.is_empty() && self.block_timestamps.is_empty() {
            return Ok(());
        }

        let block = self.build_block_08()?;
        if block.is_empty() {
            return Ok(());
        }

        self.writer.write_all(&block)?;
        self.bytes_written += block.len() as u64;
        self.blocks_written += 1;
        self.block_entries.clear();
        self.block_timestamps.clear();

        Ok(())
    }

    /// Close the FST file and return statistics
    /// Write a pre-built block (type + length prefix + body) directly to the output.
    /// Used by parallel converter to inject 0x08 VcDataDynAlias2 blocks.
    pub fn write_raw_block_data(&mut self, block_type: u8, body: &[u8]) -> Result<()> {
        let mut buf = Vec::with_capacity(1 + 8 + body.len());
        buf.push(block_type);
        buf.extend_from_slice(&(body.len() as u64).to_be_bytes());
        buf.extend_from_slice(body);
        self.writer.write_all(&buf)?;
        self.bytes_written += buf.len() as u64;
        self.blocks_written += 1;
        Ok(())
    }

    /// Close the FST file without flushing internal VCDATA blocks.
    /// Used when external code has already written all VCDATA blocks.
    pub fn close_external(mut self) -> Result<FstStats> {
        self.write_geom_block()?;
        self.write_hier_block()?;
        self.writer.flush()?;
        Ok(FstStats {
            output_bytes: self.bytes_written,
            blocks_written: self.blocks_written,
            compression_ratio: 0.0,
            signals: self.signals.len(),
            scopes: self.scopes.len(),
            timestamps: self.timestamps_count,
            value_changes: self.value_changes_count,
        })
    }

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

        let ratio = 0.0; // compression ratio not tracked

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

    /// Write header block (standard FST: [0x00][section_length=329][fields])
    fn write_header_block(&mut self) -> Result<()> {
        let mut block = BlockWriter::new(BlockType::Hdr);
        block.write_header(&self.header, 0, 0, 0, 0);
        let block_bytes = block.finalize_with_section_length(329);
        self.writer.write_all(&block_bytes)?;
        self.bytes_written += block_bytes.len() as u64;
        self.blocks_written += 1;
        Ok(())
    }

    /// Write geometry block (standard FST: [0x03][section_length=24+data][uclen][max][varints])
    fn write_geom_block(&mut self) -> Result<()> {
        let mut block = BlockWriter::new(BlockType::Geom);
        block.write_geom(&self.signals, self.signals.len() as u64);
        let section_length = block.finalize_len_plus(8); // 16 prefix + data + section_length field
        let block_bytes = block.finalize_with_section_length(section_length);
        self.writer.write_all(&block_bytes)?;
        self.bytes_written += block_bytes.len() as u64;
        self.blocks_written += 1;
        Ok(())
    }

    /// Write hierarchy block (standard FST, per fstapi):
    /// [type:0x06][section_length:8][uncompressed_length:8][raw LZ4 data]
    /// Entries: 0xFE scope [scope_type][name cstr][component cstr],
    ///          0xFF upscope,
    ///          var [var_type][direction][name cstr][length varint][alias varint]
    fn write_hier_block(&mut self) -> Result<()> {
        let mut hier_data = Vec::new();

        for ev in &self.hier_events {
            match ev {
                HierEvent::Scope { scope_type, name } => {
                    hier_data.push(0xFE); // HIERARCHY_TPE_VCD_SCOPE
                    hier_data.push(*scope_type as u8);
                    hier_data.extend_from_slice(name.as_bytes());
                    hier_data.push(0); // name terminator
                    hier_data.push(0); // empty component (terminator)
                }
                HierEvent::Upscope => {
                    hier_data.push(0xFF); // HIERARCHY_TPE_VCD_UP_SCOPE
                }
                HierEvent::Var { name, width, var_type } => {
                    hier_data.push(*var_type as u8);
                    hier_data.push(0); // direction
                    hier_data.extend_from_slice(name.as_bytes());
                    hier_data.push(0); // name terminator
                    hier_data.extend_from_slice(&encode_varint(*width as u64));
                    hier_data.extend_from_slice(&encode_varint(0)); // alias
                }
            }
        }

        // Raw LZ4 block (no size prefix — the section header carries the sizes)
        let compressed = lz4_flex::block::compress(&hier_data);
        let uclen = hier_data.len() as u64;
        let clen = compressed.len() as u64;

        let mut block = Vec::with_capacity(1 + 8 + 8 + compressed.len());
        block.push(0x06); // HIER_LZ4
        block.extend_from_slice(&(clen + 16).to_be_bytes()); // section_length
        block.extend_from_slice(&uclen.to_be_bytes()); // uncompressed_length
        block.extend_from_slice(&compressed);

        self.writer.write_all(&block)?;
        self.bytes_written += block.len() as u64;
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
