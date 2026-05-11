#![allow(dead_code)]
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::fst::writer::FstWriter;
use crate::fst::{FstOptions, VarType, ScopeType};

// ---- standalone 0x08 block builder ----

struct BlockEntry {
    handle: u32,
    time_idx: u32,
    value: Vec<u8>,
}

const FST_RCV_STR: &[u8] = b"xzhuwl-?";

fn build_08_block(
    timestamps: &[u64],
    entries: &[BlockEntry],
    signal_widths: &[u32],
    max_handle: u32,
    begin_time: u64,
    end_time: u64,
) -> Vec<u8> {
    if timestamps.is_empty() || entries.is_empty() {
        return Vec::new();
    }
    let mut time_raw = Vec::new();
    let mut prev = 0u64;
    for &ts in timestamps {
        let delta = ts.saturating_sub(prev);
        time_raw.extend_from_slice(&crate::fst::varint::encode_varint(delta));
        prev = ts;
    }
    use flate2::write::ZlibEncoder;
    use std::io::Write;
    let mut zlib = ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    zlib.write_all(&time_raw).unwrap();
    let time_comp = zlib.finish().unwrap();
    let tsec_uclen = time_raw.len() as u64;
    let tsec_clen = time_comp.len() as u64;
    let tsec_nitems = timestamps.len() as u64;

    let max_h = max_handle as usize;
    let mut chain_raw: Vec<Vec<u8>> = vec![Vec::new(); max_h + 1];
    let mut chain_has_data = vec![false; max_h + 1];
    let mut chain_last_tidx: Vec<u32> = vec![0; max_h + 1];

    for ent in entries {
        let h = ent.handle as usize;
        if h > max_h { continue; }
        let width = signal_widths.get(h.saturating_sub(1)).copied().unwrap_or(1);
        let tdelta = ent.time_idx.saturating_sub(chain_last_tidx[h]);
        chain_last_tidx[h] = ent.time_idx;
        chain_has_data[h] = true;

        let val = &ent.value;
        if width <= 1 && val.len() <= 1 {
            let byte = *val.first().unwrap_or(&b'x');
            if byte == b'0' || byte == b'1' {
                let val_bit = if byte == b'1' { 1u64 } else { 0 };
                let vli = (tdelta as u64) << 2 | (val_bit << 1) | 0;
                chain_raw[h].extend_from_slice(&crate::fst::varint::encode_varint(vli));
            } else {
                let val_idx = FST_RCV_STR.iter().position(|&c| c == byte).unwrap_or(0) as u64;
                let vli = (tdelta as u64) << 4 | (val_idx << 1) | 1;
                chain_raw[h].extend_from_slice(&crate::fst::varint::encode_varint(vli));
            }
        } else {
            let all_binary = val.iter().all(|&b| b == b'0' || b == b'1');
            if all_binary {
                let vli = (tdelta as u64) << 1 | 0;
                chain_raw[h].extend_from_slice(&crate::fst::varint::encode_varint(vli));
                let byte_count = ((width as usize) + 7) / 8;
                let mut packed = vec![0u8; byte_count];
                for (j, &b) in val.iter().enumerate().take(width as usize) {
                    if b == b'1' {
                        packed[j / 8] |= 1 << (7 - (j & 7));
                    }
                }
                chain_raw[h].extend_from_slice(&packed);
            } else {
                let vli = (tdelta as u64) << 1 | 1;
                chain_raw[h].extend_from_slice(&crate::fst::varint::encode_varint(vli));
                chain_raw[h].extend_from_slice(&crate::fst::varint::encode_varint(val.len() as u64));
                chain_raw[h].extend_from_slice(val);
            }
        }
    }

    let mut chain_offsets: Vec<u64> = vec![0; max_h + 1];
    let mut chain_data_area = Vec::new();
    let mut mem_required: u64 = 0;
    for h in 0..=max_h {
        if !chain_has_data[h] { continue; }
        let raw = std::mem::take(&mut chain_raw[h]);
        mem_required += raw.len() as u64;
        let compressed = lz4_flex::block::compress_prepend_size(&raw);
        let destlen = raw.len() as u64;
        chain_offsets[h] = chain_data_area.len() as u64;
        let dest_buf = crate::fst::varint::encode_varint(destlen);
        chain_data_area.extend_from_slice(&dest_buf);
        chain_data_area.extend_from_slice(&compressed);
    }

    let mut idx_buf = Vec::new();
    let idx_limit = max_handle as usize;
    let mut h = 0usize;
    let mut pval: u64 = 0;
    while h <= idx_limit {
        if !chain_has_data[h] {
            let mut skip = 0usize;
            while h <= idx_limit && !chain_has_data[h] { skip += 1; h += 1; }
            if skip > 0 {
                idx_buf.extend_from_slice(&crate::fst::varint::encode_fst_svarint((skip as i64) << 1));
            }
            continue;
        }
        let raw_delta = chain_offsets[h].saturating_sub(pval);
        pval = chain_offsets[h];
        idx_buf.extend_from_slice(&crate::fst::varint::encode_fst_svarint((raw_delta as i64) << 1 | 1));
        h += 1;
    }
    let chain_clen = idx_buf.len() as u64;

    let mut cp_buf = Vec::new();
    cp_buf.extend_from_slice(&crate::fst::varint::encode_varint(0));
    cp_buf.extend_from_slice(&crate::fst::varint::encode_varint(0));
    cp_buf.extend_from_slice(&crate::fst::varint::encode_varint(max_handle as u64));

    let mut vc_buf = Vec::new();
    vc_buf.extend_from_slice(&crate::fst::varint::encode_varint(max_handle as u64));
    vc_buf.push(b'4');

    let mut body = Vec::with_capacity(24 + cp_buf.len() + vc_buf.len() + chain_data_area.len() + 8 + idx_buf.len() + 8 + time_comp.len() + 24);
    body.extend_from_slice(&begin_time.to_le_bytes());
    body.extend_from_slice(&end_time.to_le_bytes());
    body.extend_from_slice(&mem_required.to_le_bytes());
    body.extend_from_slice(&cp_buf);
    body.extend_from_slice(&vc_buf);
    body.extend_from_slice(&chain_data_area);
    body.extend_from_slice(&idx_buf);
    body.extend_from_slice(&chain_clen.to_le_bytes());
    body.extend_from_slice(&time_comp);
    body.extend_from_slice(&tsec_uclen.to_le_bytes());
    body.extend_from_slice(&tsec_clen.to_le_bytes());
    body.extend_from_slice(&tsec_nitems.to_le_bytes());
    body
}

// ---- VCD byte-level parsing ----

fn parse_timestamp(line: &[u8]) -> u64 {
    let mut n: u64 = 0;
    for &b in &line[1..] {
        if b < b'0' || b > b'9' { break; }
        n = n * 10 + (b - b'0') as u64;
    }
    n
}

fn parse_value_change(line: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let len = line.len();
    if len < 2 { return None; }
    let mut sp = len;
    for i in (0..len).rev() {
        if line[i] == b' ' { sp = i; break; }
    }
    if sp == len {
        let id = line[1..].to_vec();
        if id.is_empty() { return None; }
        return Some((id, vec![line[0]]));
    }
    match line[0] {
        b'b' => {
            let bits = &line[1..sp];
            if bits.is_empty() { return None; }
            let id = line[sp + 1..].to_vec();
            let val: Vec<u8> = bits.iter().map(|&b| if b == b'1' { b'1' } else { b'0' }).collect();
            Some((id, val))
        }
        b'r' => { let val_str = &line[1..sp]; let id = line[sp + 1..].to_vec(); Some((id, val_str.to_vec())) }
        _ => { let id = line[sp + 1..].to_vec(); if id.is_empty() { return None; } Some((id, vec![line[0]])) }
    }
}

/// Parse VCD header AND register signals with FstWriter in one pass.
/// Returns (signal_map, widths, dump_start_offset, handle_count).
fn parse_header_and_register(
    data: &[u8],
    writer: &mut FstWriter<impl std::io::Write>,
) -> Result<(HashMap<Vec<u8>, u32>, Vec<u32>, usize, u32), String> {
    let mut signal_map: HashMap<Vec<u8>, u32> = HashMap::new();
    let mut widths: Vec<u32> = vec![0];
    let mut pos: usize = 0;
    let file_len = data.len();
    let mut in_directive = false;
    let mut dump_start = 0;

    while pos < file_len {
        let line_start = pos;
        let line_end = match data[pos..].iter().position(|&b| b == b'\n') {
            Some(nl) => { pos = line_start + nl + 1; line_start + nl }
            None => { pos = file_len; file_len }
        };
        let line = &data[line_start..line_end];
        if line.is_empty() || in_directive {
            if line.starts_with(b"$end") { in_directive = false; }
            continue;
        }
        if line[0] != b'$' { continue; }

        if line.starts_with(b"$scope") {
            if let Some(name_end) = line.windows(4).position(|w| w == b"$end") {
                let name_start = line.iter().position(|&b| b == b' ').unwrap_or(0);
                if let Some(second_space) = line[name_start + 1..].iter().position(|&b| b == b' ') {
                    let sname_bytes = &line[name_start + 1 + second_space + 1..name_end];
                    if let Ok(sname) = std::str::from_utf8(sname_bytes.trim_ascii_end()) {
                        if !sname.is_empty() {
                            writer.push_scope(sname, ScopeType::VcdModule);
                        }
                    }
                }
            } else { in_directive = true; }
        } else if line.starts_with(b"$var") {
            let parts: Vec<&[u8]> = line.split(|&b| b == b' ' || b == b'\t').filter(|p| !p.is_empty()).collect();
            let complete = parts.len() >= 5 && parts.last().map(|p| p.ends_with(b"$end")).unwrap_or(false);
            if complete {
                if let Ok(width) = std::str::from_utf8(parts[2]).unwrap_or("0").parse::<u32>() {
                    let id = parts[3].to_vec();
                    if width > 0 && !id.is_empty() {
                        let handle = widths.len() as u32;
                        signal_map.insert(id, handle);
                        widths.push(width);
                        // Register with FstWriter
                        let name_parts: Vec<&str> = parts[4..].iter()
                            .take_while(|p| !p.starts_with(b"$end") && **p != b"$end")
                            .map(|p| std::str::from_utf8(p).unwrap_or(""))
                            .collect();
                        let name = name_parts.join(" ");
                        let vcd_type = std::str::from_utf8(parts[1]).unwrap_or("wire");
                        writer.create_var(&name, width, VarType::from_vcd_type(vcd_type, width));
                    }
                }
            } else { in_directive = true; }
        } else if line.starts_with(b"$upscope") {
            writer.pop_scope();
        } else if line.starts_with(b"$enddefinitions") {
            dump_start = pos;
        } else if !line.windows(4).any(|w| w == b"$end") {
            in_directive = true;
        }
    }
    if dump_start == 0 {
        return Err("No $enddefinitions found in VCD".to_string());
    }
    let hc = widths.len().saturating_sub(1) as u32;
    Ok((signal_map, widths, dump_start, hc))
}

fn process_chunk(
    chunk: &[u8],
    signal_map: &HashMap<Vec<u8>, u32>,
    signal_widths: &[u32],
    max_handle: u32,
) -> Vec<u8> {
    let mut timestamps: Vec<u64> = Vec::new();
    let mut entries: Vec<BlockEntry> = Vec::new();
    let mut pos: usize = 0;
    let chunk_len = chunk.len();

    while pos < chunk_len {
        let line_start = pos;
        let line_end = match chunk[pos..].iter().position(|&b| b == b'\n') {
            Some(nl) => { pos = line_start + nl + 1; line_start + nl }
            None => { pos = chunk_len; chunk_len }
        };
        let line = &chunk[line_start..line_end];
        if line.is_empty() { continue; }
        let first = line[0];

        if first == b'#' {
            let ts = parse_timestamp(line);
            timestamps.push(ts);
        } else if first != b'$' {
            if let Some((id, val)) = parse_value_change(line) {
                if let Some(&handle) = signal_map.get(&id) {
                    entries.push(BlockEntry {
                        handle,
                        time_idx: (timestamps.len().saturating_sub(1)) as u32,
                        value: val,
                    });
                }
            }
        }
    }

    if timestamps.is_empty() {
        return Vec::new();
    }
    let begin_time = timestamps[0];
    let end_time = timestamps[timestamps.len() - 1];
    build_08_block(&timestamps, &entries, signal_widths, max_handle, begin_time, end_time)
}

/// Memory-mapped VCD file — zero heap copy even for 150GB files.
fn mmap_vcd(path: &Path) -> Result<memmap2::Mmap, String> {
    let file = fs::File::open(path)
        .map_err(|e| format!("Failed to open {}: {}", path.display(), e))?;

    // Hint sequential access for large files
    #[cfg(target_os = "linux")]
    if let Ok(meta) = file.metadata() {
        if meta.len() > 1_000_000_000 {
            unsafe {
                let fd = std::os::unix::io::AsRawFd::as_raw_fd(&file);
                libc::posix_fadvise(fd, 0, 0, libc::POSIX_FADV_SEQUENTIAL);
            }
        }
    }

    let mmap = unsafe { memmap2::Mmap::map(&file) }
        .map_err(|e| format!("Failed to mmap {}: {}", path.display(), e))?;
    Ok(mmap)
}

pub fn vcd_to_fst_parallel(
    vcd_path: &Path,
    fst_path: &Path,
    _progress: Arc<AtomicU64>,
) -> Result<(), String> {
    let mmap = mmap_vcd(vcd_path)?;
    let data: &[u8] = &mmap;
    let file_len = data.len() as u64;

    // Create FstWriter + parse header + register signals in one pass
    let fst_file = fs::File::create(fst_path)
        .map_err(|e| format!("Failed to create {}: {}", fst_path.display(), e))?;
    let mut writer = FstWriter::from_writer(fst_file, FstOptions::default())
        .map_err(|e| format!("FST writer init: {}", e))?;
    writer.set_version("wal-rust vcd2fst");

    let (signal_map, signal_widths, dump_start, handle_count) = parse_header_and_register(data, &mut writer)?;

    let dump_data = &data[dump_start..];

    // Find # positions for chunking
    let hash_pos: Vec<usize> = dump_data.iter().enumerate()
        .filter(|&(i, &b)| b == b'#' && (i == 0 || dump_data[i - 1] == b'\n'))
        .map(|(i, _)| i)
        .collect();

    if hash_pos.is_empty() {
        return Err("No timestamps found in VCD".to_string());
    }

    // Adaptive thread count: large files use fewer threads to limit memory
    let total_gb = file_len / (1 << 30);
    let max_threads = if total_gb > 100 { 2 } else if total_gb > 50 { 4 } else { num_cpus::get() };
    let num_threads = std::cmp::max(1, max_threads.min(hash_pos.len()));
    let mut chunk_boundaries: Vec<usize> = vec![0; num_threads + 1];
    chunk_boundaries[num_threads] = dump_data.len();
    let per_chunk = hash_pos.len() / num_threads;
    for i in 1..num_threads {
        chunk_boundaries[i] = hash_pos[i * per_chunk];
    }

    // Parallel processing
    use rayon::prelude::*;
    let signal_map = Arc::new(signal_map);
    let signal_widths = Arc::new(signal_widths);

    let blocks: Vec<Vec<u8>> = (0..num_threads).into_par_iter().map(|i| {
        let chunk = &dump_data[chunk_boundaries[i]..chunk_boundaries[i + 1]];
        process_chunk(chunk, &signal_map, &signal_widths, handle_count)
    }).filter(|b| !b.is_empty()).collect();

    // Write all 0x08 blocks
    for block in &blocks {
        writer.write_raw_block_data(0x08, block)
            .map_err(|e| format!("Write block: {}", e))?;
    }

    // Close writes GEOM + HIER
    writer.close_external()
        .map_err(|e| format!("FST close: {}", e))?;

    _progress.store(file_len, Ordering::Relaxed);
    Ok(())
}
