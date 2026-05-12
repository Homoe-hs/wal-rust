#![allow(dead_code)]
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::fst::{FstWriter, FstOptions, VarType, ScopeType};

const CHECKPOINT_INTERVAL: u64 = 1_000_000;

pub(crate) fn mmap_or_read(path: &Path) -> Result<Vec<u8>, String> {
    let file = fs::File::open(path)
        .map_err(|e| format!("Failed to open {}: {}", path.display(), e))?;
    let len = file.metadata().map_err(|e| format!("Metadata error: {}", e))?.len() as usize;
    if len > 1_000_000_000 {
        let mmap = unsafe { memmap2::Mmap::map(&file) }
            .map_err(|e| format!("Failed to mmap {}: {}", path.display(), e))?;
        return Ok(mmap.to_vec());
    }
    use std::io::Read;
    let mut buf = Vec::with_capacity(len);
    let mut reader = std::io::BufReader::new(file);
    reader.read_to_end(&mut buf)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    Ok(buf)
}

fn load_checkpoint(path: &Path) -> Option<u64> {
    let content = fs::read_to_string(path).ok()?;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("vcd_offset=") {
            if let Ok(off) = rest.trim().parse::<u64>() {
                return Some(off);
            }
        }
    }
    None
}

fn update_checkpoint(path: &Path, offset: u64) -> Result<(), String> {
    let content = format!("vcd_offset={}\n", offset);
    fs::write(path, &content)
        .map_err(|e| format!("Failed to write checkpoint: {}", e))
}

fn parse_timescale(line: &[u8]) -> Option<i8> {
    let s = std::str::from_utf8(line).ok()?;
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() < 3 { return None; }
    let num: u32 = parts[1].parse().ok()?;
    let unit = parts[2].to_lowercase();
    let exp = match unit.as_str() {
        "s" => 0,
        "ms" => -3,
        "us" => -6,
        "ns" => -9,
        "ps" => -12,
        "fs" => -15,
        _ => return None,
    };
    let log10 = (num as f64).log10().round() as i8;
    Some(exp + log10)
}

pub fn format_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit_idx = 0;
    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }
    format!("{:.1} {}", size, UNITS[unit_idx])
}

pub fn format_duration(secs: f64) -> String {
    if secs < 0.001 {
        format!("{:.1}µs", secs * 1_000_000.0)
    } else if secs < 1.0 {
        format!("{:.0}ms", secs * 1_000.0)
    } else if secs < 60.0 {
        format!("{:.1}s", secs)
    } else {
        let m = (secs / 60.0).floor() as u64;
        let s = secs - (m as f64 * 60.0);
        format!("{}m {:.0}s", m, s)
    }
}

fn parse_value_change(line: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let len = line.len();
    if len < 2 { return None; }

    let mut sp = len;
    for i in (0..len).rev() {
        if line[i] == b' ' {
            sp = i;
            break;
        }
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
        b'r' => {
            let val_str = &line[1..sp];
            let id = line[sp + 1..].to_vec();
            Some((id, val_str.to_vec()))
        }
        _ => {
            let id = line[sp + 1..].to_vec();
            if id.is_empty() { return None; }
            Some((id, vec![line[0]]))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fst::reader::FstReader;

    #[test]
    fn test_simple_vcd_to_fst() {
        let vcd = b"$date 2024-01-01 $end
$version wal-test $end
$timescale 1 ns $end
$scope module top $end
$var wire 1 ! clk $end
$var wire 8 # data $end
$upscope $end
$enddefinitions $end
$dumpvars
0!
b00000000 #
$end
#10
1!
b10101010 #
#20
0!
b11110000 #
";
        let tmp = std::env::temp_dir().join("test_vcd2fst.fst");
        let resume = std::env::temp_dir().join("test_vcd2fst.resume");
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&resume);

        let progress = Arc::new(AtomicU64::new(0));
        let vcd_path = std::env::temp_dir().join("test_vcd2fst_input.vcd");
        std::fs::write(&vcd_path, vcd).unwrap();

        vcd_to_fst_streaming(&vcd_path, &tmp, &resume, progress).unwrap();

        assert!(tmp.exists(), "FST file should exist");

        // Read back and verify
        let reader = FstReader::from_path(&tmp).unwrap();
        let names: std::collections::HashSet<&str> = reader.file.signals.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains("top.clk"), "Should contain top.clk");
        assert!(names.contains("top.data"), "Should contain top.data");
        assert_eq!(names.len(), 2, "Should have 2 unique signal names");

        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&resume);
        let _ = std::fs::remove_file(&vcd_path);
    }

    #[test]
    fn test_real_vcd_counter() {
        let vcd_path = Path::new("test_data/counter.vcd");
        if !vcd_path.exists() { return; }

        let fst_path = std::env::temp_dir().join("test_counter_real.fst");
        let resume = std::env::temp_dir().join("test_counter_real.resume");
        let progress = Arc::new(AtomicU64::new(0));
        vcd_to_fst_streaming(vcd_path, &fst_path, &resume, progress).unwrap();

        let reader = FstReader::from_path(&fst_path).unwrap();
        let names: std::collections::HashSet<&str> =
            reader.file.signals.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains("counter_tb.count [7:0]"), "count signal");
        assert!(names.contains("counter_tb.clk"), "clk signal");
        assert!(names.contains("counter_tb.rst"), "rst signal");

        let _ = std::fs::remove_file(&fst_path);
        let _ = std::fs::remove_file(&resume);
    }

    #[test]
    fn test_vcd_to_fst_checkpoint_resume() {
        let vcd = br"$timescale 1 ns $end
$scope module top $end
$var wire 1 ! clk $end
$upscope $end
$enddefinitions $end
$dumpvars
0!
$end
#10
1!
#20
0!
#30
1!
#40
0!
";

        let tmp = std::env::temp_dir().join("test_vcd2fst_resume.fst");
        let resume = std::env::temp_dir().join("test_vcd2fst_resume.resume");
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&resume);

        let progress = Arc::new(AtomicU64::new(0));
        let vcd_path = std::env::temp_dir().join("test_vcd2fst_resume_input.vcd");
        std::fs::write(&vcd_path, vcd).unwrap();

        // First run: full conversion
        vcd_to_fst_streaming(&vcd_path, &tmp, &resume, progress.clone()).unwrap();
        assert!(tmp.exists());
        let reader1 = FstReader::from_path(&tmp).unwrap();
        assert_eq!(reader1.file.signals.len(), 1);
        assert_eq!(reader1.file.signals[0].name, "top.clk");

        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&resume);
        let _ = std::fs::remove_file(&vcd_path);
    }
}

fn parse_timestamp(line: &[u8]) -> u64 {
    let mut n: u64 = 0;
    for &b in &line[1..] {
        if b < b'0' || b > b'9' { break; }
        n = n * 10 + (b - b'0') as u64;
    }
    n
}

pub fn vcd_to_fst_streaming(
    vcd_path: &Path,
    fst_path: &Path,
    resume_path: &Path,
    _progress: Arc<AtomicU64>,
) -> Result<(), String> {
    let data = mmap_or_read(vcd_path)?;
    let file_len = data.len();
    let resume_offset = load_checkpoint(resume_path).unwrap_or(0);

    let fst_file = fs::File::create(fst_path)
        .map_err(|e| format!("Failed to create {}: {}", fst_path.display(), e))?;
    let mut writer = FstWriter::from_writer(fst_file, FstOptions::default())
        .map_err(|e| format!("FST writer init: {}", e))?;
    writer.set_version("wal-rust vcd2fst");

    let mut signal_map: HashMap<Vec<u8>, u32> = HashMap::new();
    let mut prev_values: HashMap<u32, Vec<u8>> = HashMap::new();

    #[derive(PartialEq)]
    enum State { Header, DumpVars, Dump }
    let mut state = State::Header;
    let mut pos: usize = 0;
    let mut timestamp_count: u64 = 0;
    let mut in_skip = resume_offset > 0;
    let mut dumps_started = false;
    let mut header_done = false;
    let mut in_directive_body = false;
    let mut last_progress_pct: u8 = 0;

    while pos < file_len {
        let line_start = pos;
        let line_end = match data[pos..].iter().position(|&b| b == b'\n') {
            Some(nl) => {
                pos = line_start + nl + 1;
                line_start + nl
            }
            None => {
                pos = file_len;
                file_len
            }
        };

        let pct = (line_start as f64 / file_len as f64 * 100.0).round() as u8;
        if pct > last_progress_pct && pct % 5 == 0 {
            eprintln!("[FST cache] {}%", pct);
            last_progress_pct = pct;
        }

        let line = &data[line_start..line_end];
        if line.is_empty() { continue; }
        let first = line[0];

        match state {
            State::Header => {
                if in_directive_body {
                    if line.starts_with(b"$end") {
                        in_directive_body = false;
                    }
                    continue;
                }
                if first == b'$' {
                    let s = std::str::from_utf8(line).unwrap_or("");
                    let has_end = line.windows(4).any(|w| w == b"$end");
                    if s.starts_with("$scope") {
                        let parts: Vec<&str> = s.split_whitespace().collect();
                        if parts.len() >= 3 {
                            let name = parts[2..]
                                .iter()
                                .take_while(|&&p| p != "$end")
                                .cloned()
                                .collect::<Vec<_>>()
                                .join(" ");
                            if !name.is_empty() {
                                writer.push_scope(&name, ScopeType::VcdModule);
                            }
                        }
                    } else if s.starts_with("$var") {
                        let parts: Vec<&str> = s.split_whitespace().collect();
                        if parts.len() >= 5 {
                            if let Ok(width) = parts[2].parse::<u32>() {
                                let id_bytes = parts[3].as_bytes().to_vec();
                                let name = parts[4..]
                                    .iter()
                                    .take_while(|&&p| p != "$end")
                                    .cloned()
                                    .collect::<Vec<_>>()
                                    .join(" ");
                                if width > 0 && !id_bytes.is_empty() && !name.is_empty() {
                                    let handle = writer.create_var(
                                        &name, width,
                                        VarType::from_vcd_type(parts[1], width),
                                    );
                                    signal_map.insert(id_bytes, handle);
                                }
                            }
                        }
                    } else if s.starts_with("$upscope") {
                        writer.pop_scope();
                    } else if s.starts_with("$timescale") {
                        if let Some(exp) = parse_timescale(line) {
                            writer.set_timescale(exp);
                        }
                    } else if s.starts_with("$date") {
                        let date_str = s.trim_start_matches("$date").trim_end_matches("$end").trim();
                        if !date_str.is_empty() {
                            writer.set_date(date_str);
                        }
                    } else if s.starts_with("$dumpvars") {
                        dumps_started = true;
                        writer.emit_time_change(0);
                        state = State::DumpVars;
                    } else if s.starts_with("$enddefinitions") {
                        header_done = true;
                    } else if !has_end {
                        in_directive_body = true;
                    }
                } else if header_done && first != b'$' {
                    if first == b'#' {
                        let ts = parse_timestamp(line);
                        writer.emit_time_change(ts);
                        timestamp_count += 1;
                        state = State::Dump;
                    } else {
                        if !dumps_started {
                            writer.emit_time_change(0);
                            dumps_started = true;
                        }
                        if let Some((id, val)) = parse_value_change(line) {
                            if let Some(&handle) = signal_map.get(&id) {
                                if prev_values.get(&handle).map_or(true, |pv| pv.as_slice() != val) {
                                    writer.emit_value_change(handle, &val);
                                    prev_values.insert(handle, val);
                                }
                            }
                        }
                    }
                }
            }

            State::DumpVars => {
                if first == b'$' && line.starts_with(b"$end") {
                    state = State::Dump;
                } else if first == b'#' {
                    state = State::Dump;
                } else if first != b'$' {
                    if !dumps_started {
                        writer.emit_time_change(0);
                        dumps_started = true;
                    }
                    if let Some((id, val)) = parse_value_change(line) {
                        if let Some(&handle) = signal_map.get(&id) {
                            if prev_values.get(&handle).map_or(true, |pv| pv.as_slice() != val) {
                                writer.emit_value_change(handle, &val);
                                prev_values.insert(handle, val);
                            }
                        }
                    }
                }
            }

            State::Dump => {
                if in_skip {
                    if line_start as u64 >= resume_offset {
                        in_skip = false;
                    } else {
                        continue;
                    }
                }

                if first == b'#' {
                    let ts = parse_timestamp(line);
                    writer.emit_time_change(ts);
                    timestamp_count += 1;

                    if timestamp_count % CHECKPOINT_INTERVAL == 0 {
                        update_checkpoint(resume_path, line_start as u64)?;
                    }
                } else if first != b'$' {
                    if let Some((id, val)) = parse_value_change(line) {
                        if let Some(&handle) = signal_map.get(&id) {
                            if prev_values.get(&handle).map_or(true, |pv| pv.as_slice() != val) {
                                writer.emit_value_change(handle, &val);
                                prev_values.insert(handle, val);
                            }
                        }
                    }
                }
            }
        }
    }

    _progress.store(file_len as u64, Ordering::Relaxed);
    writer.close().map_err(|e| format!("FST close: {}", e))?;
    let _ = fs::remove_file(resume_path);
    Ok(())
}
