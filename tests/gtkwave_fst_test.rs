use std::path::Path;
use wal_rust::fst::reader::FstReader;

#[test]
fn test_gtkwave_des_fst() {
    let path = Path::new("/tmp/gtkwave_examples/des.fst");
    if !path.exists() { eprintln!("SKIP: des.fst not found"); return; }
    let reader = FstReader::from_path(path).unwrap();
    eprintln!("des.fst: {} signals, {} scopes, t={}->{}, ver='{}'",
        reader.file.signals.len(),
        reader.file.scopes.len(),
        reader.file.header.start_time,
        reader.file.header.end_time,
        reader.file.header.version);
    // HDR is correctly parsed (end_time=704, version='Icarus Verilog')
    assert_eq!(reader.file.header.end_time, 704, "des.fst should end at time 704");
    assert!(reader.file.header.version.contains("Icarus"), "des.fst version should contain 'Icarus'");
}

#[test]
fn test_gtkwave_transaction_fst() {
    let path = Path::new("/tmp/gtkwave_examples/transaction.fst");
    if !path.exists() { eprintln!("SKIP: transaction.fst not found"); return; }
    let reader = FstReader::from_path(path).unwrap();
    eprintln!("transaction.fst: {} signals, {} scopes, t={}->{}, ver='{}'",
        reader.file.signals.len(),
        reader.file.scopes.len(),
        reader.file.header.start_time,
        reader.file.header.end_time,
        reader.file.header.version);
    // ZWRAP file — decompression succeeds but inner data may be truncated
    assert!(reader.file.header.start_time <= reader.file.header.end_time, "start <= end");
}
