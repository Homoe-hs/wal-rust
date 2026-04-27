use std::path::Path;
use wal_rust::trace::{Trace, FstTrace, VcdTrace};

#[test]
fn test_fst_trace_load() {
    let path = Path::new("test_data/test_100M.fst");
    if !path.exists() {
        eprintln!("Skipping FST test - test file not found");
        return;
    }

    let trace = FstTrace::load(path, "test".to_string()).unwrap();

    assert_eq!(trace.id(), "test");
    assert_eq!(trace.filename(), "test_data/test_100M.fst");

    let signals = trace.signals();
    assert!(!signals.is_empty(), "Should have at least one signal");

    eprintln!("FST file loaded: {} signals, max_index={}",
              signals.len(), trace.max_index());
}

#[test]
fn test_fst_trace_read_vcdata() {
    let path = Path::new("test_data/test_100M.fst");
    if !path.exists() {
        eprintln!("Skipping FST VCDATA test - test file not found");
        return;
    }

    let trace = FstTrace::load(path, "test".to_string()).unwrap();

    eprintln!("After load (VCDATA included): max_index={}", trace.max_index());

    if trace.max_index() > 0 {
        let signals = trace.signals();
        if !signals.is_empty() {
            let sig_name = &signals[0];
            match trace.signal_value(sig_name, 0) {
                Ok(v) => eprintln!("Signal '{}' at index 0: {:?}", sig_name, v),
                Err(e) => eprintln!("Error reading signal '{}': {}", sig_name, e),
            }
        }
    }
}

#[test]
fn test_vcd_trace_load() {
    let path = Path::new("test_data/test_1M.vcd");
    if !path.exists() {
        eprintln!("Skipping VCD test - test file not found");
        return;
    }

    let trace = VcdTrace::load(path, "test".to_string()).unwrap();

    assert_eq!(trace.id(), "test");

    let signals = trace.signals();
    assert!(!signals.is_empty(), "Should have at least one signal");

    eprintln!("VCD file loaded: {} signals, max_index={}",
              signals.len(), trace.max_index());
}

#[test]
fn test_vcd_trace_signal_access() {
    let path = Path::new("test_data/test_1M.vcd");
    if !path.exists() {
        eprintln!("Skipping VCD signal access test - test file not found");
        return;
    }

    let trace = VcdTrace::load(path, "test".to_string()).unwrap();

    let signals = trace.signals();
    if signals.is_empty() {
        eprintln!("No signals found in VCD file");
        return;
    }

    let sig_name = &signals[0];
    match trace.signal_width(sig_name) {
        Ok(w) => eprintln!("Signal '{}' width: {}", sig_name, w),
        Err(e) => eprintln!("Error getting width for '{}': {}", sig_name, e),
    }

    if trace.max_index() > 0 {
        match trace.signal_value(sig_name, 0) {
            Ok(v) => eprintln!("Signal '{}' at index 0: {:?}", sig_name, v),
            Err(e) => eprintln!("Error reading signal '{}': {}", sig_name, e),
        }
    }
}