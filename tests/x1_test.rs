#[test] fn x1() {
    let r = wal_rust::fst::reader::FstReader::from_path(
        std::path::Path::new("/tmp/fresh_counter.fst")).unwrap();
    for s in &r.file.signals {
        eprintln!("SIG h={} name='{}' w={}", s.handle, s.name, s.width);
    }
}
TESTEOF
cargo test --test x1_test -- --nocapture 2>&1 | grep "^SIG\|test result"
rm -f tests/x1_test.rs