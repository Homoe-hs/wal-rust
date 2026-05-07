#[test] fn x2() {
    use std::path::Path;
    match wal_rust::fst::reader::FstReader::from_path(Path::new("/tmp/fresh_counter.fst")) {
        Ok(r) => {
            eprintln!("signals: {}", r.file.signals.len());
            for s in &r.file.signals {
                eprintln!("  h={} name='{}' w={}", s.handle, s.name, s.width);
            }
        }
        Err(e) => eprintln!("ERROR: {}", e),
    }
}
TESTEOF
cargo test --test x2_test -- --nocapture 2>&1 | tail -20
rm -f tests/x2_test.rs