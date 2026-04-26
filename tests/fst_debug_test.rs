use std::io::Cursor;
use wal_rust::fst::{FstWriter, FstOptions, VarType, ScopeType, FstReader};

#[test]
fn test_fst_reader_debug() {
    let buffer = Vec::new();
    let mut writer = FstWriter::from_writer(buffer, FstOptions::default()).unwrap();

    writer.set_timescale(-9);
    writer.push_scope("top", ScopeType::VcdModule);

    let clk_handle = writer.create_var("clk", 1, VarType::VcdWire);
    let data_handle = writer.create_var("data", 8, VarType::VcdWire);

    writer.pop_scope();

    writer.emit_time_change(0);
    writer.emit_value_change(clk_handle, &[b'0']);
    writer.emit_value_change(data_handle, &[b'0'; 8]);

    writer.emit_time_change(100);
    writer.emit_value_change(clk_handle, &[b'1']);
    writer.emit_value_change(data_handle, &[b'1', b'0', b'1', b'0', b'1', b'0', b'1', b'0']);

    let stats = writer.close().unwrap();

    // Note: buffer is moved into writer, so we can't access it here
    // Let's use a different approach - write to a temp file
    eprintln!("Written {} signals, {} bytes", stats.signals, stats.output_bytes);
}

#[test]
fn test_fst_file_roundtrip() {
    use std::fs;
    use std::io::{Read, Write};
    use wal_rust::fst::FstReader;

    let temp_path = "/tmp/test_fst_roundtrip.fst";

    {
        let file = fs::File::create(temp_path).unwrap();
        let mut writer = FstWriter::from_writer(file, FstOptions::default()).unwrap();

        writer.set_timescale(-9);
        writer.push_scope("top", ScopeType::VcdModule);

        let clk_handle = writer.create_var("clk", 1, VarType::VcdWire);
        let data_handle = writer.create_var("data", 8, VarType::VcdWire);

        writer.pop_scope();

        writer.emit_time_change(0);
        writer.emit_value_change(clk_handle, &[b'0']);
        writer.emit_value_change(data_handle, &[b'0'; 8]);

        writer.emit_time_change(100);
        writer.emit_value_change(clk_handle, &[b'1']);
        writer.emit_value_change(data_handle, &[b'1', b'0', b'1', b'0', b'1', b'0', b'1', b'0']);

        let stats = writer.close().unwrap();
        eprintln!("Written {} signals, {} bytes", stats.signals, stats.output_bytes);
    }

    // Now read it back
    let mut file = fs::File::open(temp_path).unwrap();
    let mut contents = Vec::new();
    file.read_to_end(&mut contents).unwrap();

    eprintln!("File size: {} bytes", contents.len());

    // Parse manually
    eprintln!("\nManual parsing:");
    let mut pos = 0;
    // Block 1: HDR
    let block_type = contents[pos];
    eprintln!("  Block 1 type: 0x{:02x}", block_type);
    pos += 1;
    let block_len = u64::from_le_bytes(contents[pos..pos+8].try_into().unwrap());
    eprintln!("  Block 1 length: {}", block_len);
    pos += 8 + block_len as usize;

    // Block 2: VCDATA
    if pos < contents.len() {
        let block2_type = contents[pos];
        eprintln!("  Block 2 type: 0x{:02x}", block2_type);
        pos += 1;
        let block2_len = u64::from_le_bytes(contents[pos..pos+8].try_into().unwrap());
        eprintln!("  Block 2 length: {}", block2_len);
        pos += 8 + block2_len as usize;
    }

    // Block 3: GEOM
    if pos < contents.len() {
        let block3_type = contents[pos];
        eprintln!("  Block 3 type: 0x{:02x}", block3_type);
        pos += 1;
        let block3_len = u64::from_le_bytes(contents[pos..pos+8].try_into().unwrap());
        eprintln!("  Block 3 length: {}", block3_len);
        pos += 8;
        // section_length
        let section_len = u64::from_le_bytes(contents[pos..pos+8].try_into().unwrap());
        eprintln!("  GEOM section_length: {}", section_len);
        pos += 8;
        // uncompressed_length
        let uncomp_len = u64::from_le_bytes(contents[pos..pos+8].try_into().unwrap());
        eprintln!("  GEOM uncompressed_length: {}", uncomp_len);
        pos += 8;
        // max_handle
        let max_handle = u64::from_le_bytes(contents[pos..pos+8].try_into().unwrap());
        eprintln!("  GEOM max_handle: {}", max_handle);
        pos += 8;

        // Now signal data
        eprintln!("  Signal data starts at offset {}", pos);
        let end_sig_data = pos + section_len as usize - 24;
        eprintln!("  Signal data ends at offset {}", end_sig_data);
        eprintln!("  Next 64 bytes: {:02x?}", &contents[pos..std::cmp::min(pos+64, contents.len())]);
    }

    // Block 4: HIER
    if pos < contents.len() {
        let block4_type = contents[pos];
        eprintln!("  Block 4 type: 0x{:02x}", block4_type);
    }

    // Now use FstReader
    let mut cursor = Cursor::new(contents);
    let reader = FstReader::from_reader(&mut cursor).unwrap();

    eprintln!("\nFstReader result:");
    eprintln!("Read {} signals from file", reader.file.signals.len());
    eprintln!("Header: timescale={}, version={}, date={}",
              reader.file.header.timescale_exp,
              reader.file.header.version,
              reader.file.header.date);

    for sig in &reader.file.signals {
        eprintln!("  Signal: {} (handle={}, width={}, type={:?})",
                  sig.name, sig.handle, sig.width, sig.var_type);
    }

    // Clean up
    let _ = fs::remove_file(temp_path);
}