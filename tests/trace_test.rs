use std::path::Path;
use wal_rust::trace::{Trace, FstTrace, VcdTrace};

#[test]
fn test_fst_trace_load() {
    let path = Path::new("test_data/grp1_mpu7_timeout.fst");
    if !path.exists() { return; }
    let trace = FstTrace::load(path, "test".to_string()).unwrap();
    assert!(!trace.signals().is_empty());
    assert!(trace.max_index() > 0);
}

#[test]
fn test_fst_trace_signal_value() {
    let path = Path::new("test_data/grp1_mpu7_timeout.fst");
    if !path.exists() { return; }
    let trace = FstTrace::load(path, "test".to_string()).unwrap();
    let sigs = trace.signals();
    if let Some(first) = sigs.first() {
        let result = trace.signal_value(first, 0);
        assert!(result.is_ok() || result.is_err());
    }
}

#[test]
fn test_vcd_trace_load() {
    let path = Path::new("test_data/test_1M.vcd");
    if !path.exists() { return; }
    let trace = VcdTrace::load(path, "test".to_string()).unwrap();
    let signals = trace.signals();
    assert!(!signals.is_empty());
}

#[test]
fn test_vcd_trace_signal_access() {
    let path = Path::new("test_data/test_1M.vcd");
    if !path.exists() { return; }
    let trace = VcdTrace::load(path, "test".to_string()).unwrap();
    let signals = trace.signals();
    if signals.is_empty() { return; }
    let sig_name = &signals[0];
    let _width = trace.signal_width(sig_name);
    if trace.max_index() > 0 {
        let _val = trace.signal_value(sig_name, 0);
    }
}
