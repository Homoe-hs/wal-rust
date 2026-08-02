//! Roundtrip tests for the wellen-backed FstTrace:
//! write FST with the hand-rolled FstWriter, read back through wellen.

use std::path::{Path, PathBuf};

use wal_rust::fst::{FstOptions, FstWriter, ScopeType, VarType};
use wal_rust::trace::{FindCondition, FstTrace, ScalarValue, Trace};

fn temp_fst_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("wal_rust_{}_{}.fst", name, std::process::id()));
    p
}

/// Build a 2-state waveform:
///   clk:  0 1 0 1 0 1 0 1 0 1   (1-bit, toggle)
///   nib:  x 1010, then 0101, then 1111   (4-bit vector, multiple of 8? no: 4)
///   wide: 101011001100 held, then 001100111100   (12-bit vector, partial byte)
///   data: 32-bit counter-like values
fn write_two_state_fst(path: &Path) {
    let mut w = FstWriter::create(path, FstOptions::default()).unwrap();
    w.push_scope("top", ScopeType::VcdModule);
    let clk = w.create_var("clk", 1, VarType::VcdWire);
    let nib = w.create_var("nib", 4, VarType::VcdWire);
    let wide = w.create_var("wide", 12, VarType::VcdWire);
    let data = w.create_var("data", 32, VarType::VcdWire);
    w.pop_scope();

    for t in 0..10u64 {
        w.emit_time_change(t * 100);
        w.emit_value_change(clk, &[if t % 2 == 0 { b'0' } else { b'1' }]);
        w.emit_value_change(nib, match t % 3 {
            0 => b"1010".as_slice(),
            1 => b"0101".as_slice(),
            _ => b"1111".as_slice(),
        });
        w.emit_value_change(wide, if t < 5 { b"101011001100".as_slice() } else { b"001100111100".as_slice() });
        let d = (t as u32) * 0x1000_0003;
        let bits = format!("{:032b}", d);
        w.emit_value_change(data, bits.as_bytes());
    }
    w.close().unwrap();
}

/// Build a 4-state waveform (RCV scalar codes + non-binary vectors):
///   sig:  0 x 1 z 0  ...  (1-bit with x/z states)
///   vec:  "10x1", "001z", "1x1x"  (4-bit vector with unknown bits)
fn write_four_state_fst(path: &Path) {
    let mut w = FstWriter::create(path, FstOptions::default()).unwrap();
    w.push_scope("top", ScopeType::VcdModule);
    let sig = w.create_var("sig", 1, VarType::VcdWire);
    let vec = w.create_var("vec", 4, VarType::VcdWire);
    w.pop_scope();

    let vals: [&[u8]; 6] = [b"0", b"x", b"1", b"z", b"0", b"1"];
    let vecs: [&[u8]; 6] = [b"10x1", b"001z", b"1x1x", b"0000", b"zzzz", b"1100"];
    for t in 0..6u64 {
        w.emit_time_change(t * 100);
        w.emit_value_change(sig, vals[t as usize]);
        w.emit_value_change(vec, vecs[t as usize]);
    }
    w.close().unwrap();
}

fn assert_vector(trace: &FstTrace, name: &str, idx: usize, expected: &[u8]) {
    match trace.signal_value(name, idx).unwrap() {
        ScalarValue::Vector(v) => assert_eq!(v, expected, "{} at {}", name, idx),
        other => panic!("{} at {}: expected Vector got {:?}", name, idx, other),
    }
}

fn assert_bit(trace: &FstTrace, name: &str, idx: usize, expected: u8) {
    match trace.signal_value(name, idx).unwrap() {
        ScalarValue::Bit(b) => assert_eq!(b, expected, "{} at {}", name, idx),
        other => panic!("{} at {}: expected Bit got {:?}", name, idx, other),
    }
}

#[test]
fn test_wellen_backend_two_state_values() {
    let path = temp_fst_path("two_state");
    write_two_state_fst(&path);

    let trace = FstTrace::load(&path, "t".to_string()).unwrap();
    assert_eq!(trace.max_index(), 9);
    assert_eq!(trace.signal_width("top.clk").unwrap(), 1);
    assert_eq!(trace.signal_width("top.wide").unwrap(), 12);

    // 1-bit: exact chars
    assert_bit(&trace, "top.clk", 0, b'0');
    assert_bit(&trace, "top.clk", 1, b'1');
    assert_bit(&trace, "top.clk", 9, b'1');

    // 4-bit vector (partial byte, LSB-aligned in wellen layout)
    assert_vector(&trace, "top.nib", 0, b"1010");
    assert_vector(&trace, "top.nib", 1, b"0101");
    assert_vector(&trace, "top.nib", 3, b"1010");

    // 12-bit vector (partial byte: byte0 holds top 4 bits)
    assert_vector(&trace, "top.wide", 0, b"101011001100");
    assert_vector(&trace, "top.wide", 4, b"101011001100");
    assert_vector(&trace, "top.wide", 5, b"001100111100");
    assert_vector(&trace, "top.wide", 9, b"001100111100");

    // 32-bit vector int comparisons (value held → every timestamp in range)
    let d = 0u32.wrapping_mul(0x1000_0003);
    assert_eq!(
        trace.find_indices("top.data", FindCondition::ValueI64(d as i64)).unwrap(),
        vec![0]
    );

    // 12-bit int comparison (0xACC = 2764, held for indices 0..5)
    assert_eq!(
        trace.find_indices("top.wide", FindCondition::ValueI64(0b101011001100)).unwrap(),
        vec![0, 1, 2, 3, 4]
    );
    assert_eq!(
        trace.find_indices("top.wide", FindCondition::ValueI64(0b001100111100)).unwrap(),
        vec![5, 6, 7, 8, 9]
    );

    // 1-bit conditions
    assert_eq!(
        trace.find_indices("top.clk", FindCondition::High).unwrap(),
        vec![1, 3, 5, 7, 9]
    );
    assert_eq!(
        trace.find_indices("top.clk", FindCondition::Rising).unwrap(),
        vec![1, 3, 5, 7, 9]
    );
    assert_eq!(
        trace.find_indices("top.clk", FindCondition::Falling).unwrap(),
        vec![2, 4, 6, 8]
    );
    assert_eq!(
        trace.find_indices("top.clk", FindCondition::Value(0)).unwrap(),
        vec![0, 2, 4, 6, 8]
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_wellen_backend_four_state_values() {
    let path = temp_fst_path("four_state");
    write_four_state_fst(&path);

    let trace = FstTrace::load(&path, "t".to_string()).unwrap();
    assert_eq!(trace.max_index(), 5);

    // 1-bit x/z through RCV scalar encoding
    assert_bit(&trace, "top.sig", 0, b'0');
    assert_bit(&trace, "top.sig", 1, b'x');
    assert_bit(&trace, "top.sig", 2, b'1');
    assert_bit(&trace, "top.sig", 3, b'z');
    assert_bit(&trace, "top.sig", 5, b'1');

    // 4-bit vector with x/z: FST literal (non-binary) path
    assert_vector(&trace, "top.vec", 0, b"10x1");
    assert_vector(&trace, "top.vec", 1, b"001z");
    assert_vector(&trace, "top.vec", 2, b"1x1x");
    assert_vector(&trace, "top.vec", 3, b"0000");
    assert_vector(&trace, "top.vec", 4, b"zzzz");
    assert_vector(&trace, "top.vec", 5, b"1100");

    // 1-bit conditions on x/z states
    assert_eq!(
        trace.find_indices("top.sig", FindCondition::High).unwrap(),
        vec![2, 5]
    );
    assert_eq!(
        trace.find_indices("top.sig", FindCondition::Low).unwrap(),
        vec![0, 4]
    );
    // x/z are neither high nor low, but they are "not 0" (matches VCD backend semantics)
    assert_eq!(
        trace.find_indices("top.sig", FindCondition::Neq(0)).unwrap(),
        vec![1, 2, 3, 5]
    );

    // Vectors with x/z must not match int comparisons;
    // "0000" at idx 3 is held until idx 4, "1100" at idx 5 to the end
    assert_eq!(
        trace.find_indices("top.vec", FindCondition::ValueI64(0)).unwrap(),
        vec![3]
    );
    assert_eq!(
        trace.find_indices("top.vec", FindCondition::ValueI64(12)).unwrap(),
        vec![5]
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_wellen_backend_step_and_index() {
    let path = temp_fst_path("step");
    write_two_state_fst(&path);

    let mut trace = FstTrace::load(&path, "t".to_string()).unwrap();
    assert_eq!(trace.index(), 0);
    trace.step(3).unwrap();
    assert_eq!(trace.index(), 3);
    assert_bit(&trace, "top.clk", 3, b'1');
    assert!(trace.step(100).is_err());

    let _ = std::fs::remove_file(&path);
}
