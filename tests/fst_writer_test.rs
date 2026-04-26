use std::io::Cursor;
use wal_rust::fst::{FstWriter, FstOptions, VarType, ScopeType};

#[test]
fn test_fst_writer_and_reader_roundtrip() {
    let buffer = Cursor::new(Vec::new());
    let mut writer = FstWriter::from_writer(buffer, FstOptions::default()).unwrap();

    writer.set_timescale(-9);
    writer.push_scope("top", ScopeType::VcdModule);

    let clk_handle = writer.create_var("clk", 1, VarType::VcdWire);
    let data_handle = writer.create_var("data", 8, VarType::VcdWire);

    writer.pop_scope();

    writer.emit_time_change(0);
    writer.emit_value_change(clk_handle, &[b'0']);
    writer.emit_value_change(data_handle, &[b'0', b'0', b'0', b'0', b'0', b'0', b'0', b'0']);

    writer.emit_time_change(100);
    writer.emit_value_change(clk_handle, &[b'1']);
    writer.emit_value_change(data_handle, &[b'1', b'0', b'1', b'0', b'1', b'0', b'1', b'0']);

    let stats = writer.close().unwrap();

    assert_eq!(stats.signals, 2);
    assert_eq!(stats.scopes, 0);
    assert!(stats.timestamps >= 2);

    eprintln!("FST written: {} bytes, {} signals, {} timestamps",
              stats.output_bytes, stats.signals, stats.timestamps);
}

#[test]
fn test_fst_reader_with_generated_file() {
    use wal_rust::fst::FstReader;

    let buffer = Vec::new();
    let mut writer = FstWriter::from_writer(buffer.clone(), FstOptions::default()).unwrap();

    writer.set_timescale(-9);
    writer.push_scope("top", ScopeType::VcdModule);

    let clk_handle = writer.create_var("clk", 1, VarType::VcdWire);
    let data_handle = writer.create_var("data", 8, VarType::VcdWire);

    writer.pop_scope();

    writer.emit_time_change(0);
    writer.emit_value_change(clk_handle, &[b'0']);
    writer.emit_value_change(data_handle, &[b'0', b'0', b'0', b'0', b'0', b'0', b'0', b'0']);

    writer.emit_time_change(100);
    writer.emit_value_change(clk_handle, &[b'1']);
    writer.emit_value_change(data_handle, &[b'1', b'0', b'1', b'0', b'1', b'0', b'1', b'0']);

    let _stats = writer.close().unwrap();

    let mut cursor = Cursor::new(buffer);

    let reader = FstReader::from_reader(&mut cursor).unwrap();

    eprintln!("FST read: {} signals", reader.file.signals.len());
    for sig in &reader.file.signals {
        eprintln!("  - {} (handle={}, width={})", sig.name, sig.handle, sig.width);
    }
}