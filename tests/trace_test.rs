use std::path::Path;
use wal_rust::trace::{Trace, FstTrace, VcdTrace, FindCondition, ScalarValue};

mod common;
use common::*;

#[test]
fn test_fst_trace_load() {
    let path = fst_fixture_path();
    let trace = FstTrace::load(&path, "test".to_string()).unwrap();
    assert_eq!(trace.id(), "test");
    assert!(!trace.signals().is_empty());
    assert!(trace.max_index() == 19, "expected 20 timestamps, got {}", trace.max_index());
    assert!(trace.signals().contains(&"top.clk".to_string()));
}

#[test]
fn test_fst_trace_signal_value() {
    let path = fst_fixture_path();
    let trace = FstTrace::load(&path, "test".to_string()).unwrap();
    // 1-bit toggling clock
    match trace.signal_value("top.clk", 0).unwrap() {
        ScalarValue::Bit(b) => assert_eq!(b, b'0'),
        other => panic!("expected Bit, got {:?}", other),
    }
    match trace.signal_value("top.clk", 1).unwrap() {
        ScalarValue::Bit(b) => assert_eq!(b, b'1'),
        other => panic!("expected Bit, got {:?}", other),
    }
    // 12-bit vector (partial-byte width)
    match trace.signal_value("top.data", 0).unwrap() {
        ScalarValue::Vector(v) => assert_eq!(v, b"101011001100"),
        other => panic!("expected Vector, got {:?}", other),
    }
    // 4-state scalar
    match trace.signal_value("top.sig", 1).unwrap() {
        ScalarValue::Bit(b) => assert_eq!(b, b'x'),
        other => panic!("expected Bit(x), got {:?}", other),
    }
}

#[test]
fn test_fst_trace_find() {
    let path = fst_fixture_path();
    let trace = FstTrace::load(&path, "test".to_string()).unwrap();
    // Rising edges of the toggling clock
    let rises = trace.find_indices("top.clk", FindCondition::Rising).unwrap();
    assert_eq!(rises, vec![1, 3, 5, 7, 9, 11, 13, 15, 17, 19]);
    // Integer comparison on a held vector value
    let hits = trace.find_indices("top.data", FindCondition::ValueI64(0b101011001100)).unwrap();
    assert_eq!(hits, vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
    // High on a 1-bit signal
    let highs = trace.find_indices("top.sig", FindCondition::Value(1)).unwrap();
    assert_eq!(highs, vec![2, 6, 10, 14, 18]);
}

#[test]
fn test_vcd_trace_load() {
    let path = counter_vcd_path();
    let trace = VcdTrace::load(&path, "test".to_string()).unwrap();
    assert_eq!(trace.id(), "test");
    let signals = trace.signals();
    assert!(!signals.is_empty());
}

#[test]
fn test_vcd_trace_signal_access() {
    let path = counter_vcd_path();
    let trace = VcdTrace::load(&path, "test".to_string()).unwrap();
    let signals = trace.signals();
    assert_eq!(signals.len(), 6);
    let sig_name = "counter_tb.clk".to_string();
    match trace.signal_value(&sig_name, 0).unwrap() {
        ScalarValue::Bit(b) => assert_eq!(b, b'0'),
        other => panic!("expected Bit, got {:?}", other),
    }
    match trace.signal_value(&sig_name, 1).unwrap() {
        ScalarValue::Bit(b) => assert_eq!(b, b'1'),
        other => panic!("expected Bit, got {:?}", other),
    }
    assert_eq!(trace.max_index(), 522);
}
