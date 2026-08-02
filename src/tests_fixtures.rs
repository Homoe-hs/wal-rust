//! Self-contained test fixtures: deterministic VCD/FST generation.
//!
//! The test suite must not depend on `test_data/` (gitignored, large files).
//! These generators produce equivalent waveforms at test time.
//! Compiled only for tests: shared by lib unit tests and integration tests.

use std::path::{Path, PathBuf};

fn fixture_dir() -> PathBuf {
    std::env::temp_dir().join(format!("wal_rust_fixtures_{}", std::process::id()))
}

fn write_fixture(name: &str, content: &str) -> PathBuf {
    let dir = fixture_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    if p.exists() {
        return p;
    }
    // Atomic write: unique temp file + rename, so parallel tests never read
    // a half-written fixture or clobber each other's temp file.
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let uniq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!("{}.tmp{}_{}", name, std::process::id(), uniq));
    std::fs::write(&tmp, content).unwrap();
    let _ = std::fs::rename(&tmp, &p);
    p
}

/// Deterministic counter waveform matching the old `test_data/counter.vcd`
/// contract used by wal_integration_test:
///   6 signals, 523 timestamps (max_index 522), first signal
///   "counter_tb.count [7:0]", clk[0]='0', clk[1]='1', first rising @1.
pub fn counter_vcd_path() -> PathBuf {
    let p = fixture_dir().join("counter_fixture.vcd");
    if p.exists() {
        return p;
    }
    let mut s = String::with_capacity(64 * 1024);
    s.push_str("$timescale 1ns $end\n");
    s.push_str("$scope module counter_tb $end\n");
    s.push_str("$var wire 8 ! count [7:0] $end\n");
    s.push_str("$var wire 1 # clk $end\n");
    s.push_str("$var wire 1 $ rst $end\n");
    s.push_str("$scope module uut $end\n");
    s.push_str("$var wire 1 % clk $end\n");
    s.push_str("$upscope $end\n");
    s.push_str("$scope module monitor $end\n");
    s.push_str("$var wire 1 & done $end\n");
    s.push_str("$upscope $end\n");
    s.push_str("$var wire 1 ' b_ready $end\n");
    s.push_str("$upscope $end\n");
    s.push_str("$enddefinitions $end\n");
    s.push_str("$dumpvars\n");
    // Explicit #0 plus #1..#522 = 523 timestamps (indices 0..522)
    for i in 0..523usize {
        s.push_str(&format!("#{}\n", i));
        // clk toggles every step, starting low
        s.push_str(if i % 2 == 0 { "0#\n" } else { "1#\n" });
        // 8-bit counter increments every step
        s.push_str(&format!("b{:08b} !\n", (i % 256) as u8));
        // rst high for the first 3 steps
        s.push_str(if i < 3 { "1$\n" } else { "0$\n" });
        // uut.clk in phase with the top clk
        s.push_str(if i % 2 == 0 { "0%\n" } else { "1%\n" });
        // monitor.done pulses every 100 steps
        s.push_str(if i % 100 == 99 { "1&\n" } else { "0&\n" });
        // b_ready: mostly high
        s.push_str(if i % 7 == 0 { "0'\n" } else { "1'\n" });
    }
    s.push_str("$end\n");
    write_fixture("counter_fixture.vcd", &s)
}

/// Deterministic waveform matching the `gen_vcd_pyvcd.py` patterns used by
/// vcd_correctness_test, but small (20000 timestamps instead of 100M):
///   counter:  value = timestamp_idx % 2^32            (32-bit)
///   strobe:   value = (timestamp_idx / 1000) % 2      (1-bit)
///   data_bus: value = (timestamp_idx >> 2) ^ 0xDEADBEEFABCD1234 (64-bit)
///   sigN:     value = (timestamp_idx + N) % 2         (1-bit, N = 0..49)
pub fn pyvcd_vcd_path() -> PathBuf {
    let p = fixture_dir().join("pyvcd_fixture.vcd");
    if p.exists() {
        return p;
    }
    let mut s = String::with_capacity(24 * 1024 * 1024);
    s.push_str("$timescale 1ns $end\n");
    s.push_str("$scope module top $end\n");
    s.push_str("$var wire 32 ! counter [31:0] $end\n");
    s.push_str("$var wire 1 \" strobe $end\n");
    s.push_str("$var wire 64 # data_bus [63:0] $end\n");
    for i in 0..50 {
        s.push_str(&format!("$var wire 1 {} sig{} $end\n", char::from(b'$' + i as u8), i));
    }
    s.push_str("$upscope $end\n");
    s.push_str("$enddefinitions $end\n");
    s.push_str("$dumpvars\n");
    const N: usize = 20000;
    for t in 0..N {
        s.push_str(&format!("#{}\n", t));
        let counter = (t as i64) % (1i64 << 32);
        s.push_str(&format!("b{:032b} !\n", counter));
        let strobe = if (t / 1000) % 2 == 0 { '0' } else { '1' };
        s.push_str(&format!("{}\"\n", strobe));
        let data_bus = ((t as u64) >> 2) ^ 0xDEAD_BEEF_ABCD_1234u64;
        s.push_str(&format!("b{:064b} #\n", data_bus));
        for i in 0..50 {
            let b = if (t + i) % 2 == 0 { '0' } else { '1' };
            s.push_str(&format!("{}{}\n", b, char::from(b'$' + i as u8)));
        }
    }
    s.push_str("$end\n");
    write_fixture("pyvcd_fixture.vcd", &s)
}

fn write_edge_fixture(name: &str, body: &str) -> PathBuf {
    write_fixture(name, body)
}

/// Mixed edge-case waveform (2 scopes, vectors, events)
pub fn edge_cases_path() -> PathBuf {
    write_edge_fixture(
        "edge_cases_fixture.vcd",
        "$timescale 1ns $end\n\
         $scope module top $end\n\
         $var wire 1 ! clk $end\n\
         $var wire 8 # data [7:0] $end\n\
         $scope module sub $end\n\
         $var wire 1 $ en $end\n\
         $upscope $end\n\
         $upscope $end\n\
         $enddefinitions $end\n\
         $dumpvars\n\
         0!\n\
         b00000000 #\n\
         1$\n\
         $end\n\
         #10\n\
         1!\n\
         b10101010 #\n\
         0$\n\
         $end\n",
    )
}

/// Waveform with no timestamps after the initial values
pub fn edge_empty_time_path() -> PathBuf {
    write_edge_fixture(
        "edge_empty_time_fixture.vcd",
        "$timescale 1ns $end\n\
         $scope module top $end\n\
         $var wire 1 ! a $end\n\
         $var wire 1 # b $end\n\
         $upscope $end\n\
         $enddefinitions $end\n\
         $dumpvars\n\
         #0\n\
         0!\n\
         1#\n\
         $end\n\
         #10\n\
         1!\n\
         0#\n\
         $end\n",
    )
}

/// Waveform with wide (128-bit) vectors
pub fn edge_large_vectors_path() -> PathBuf {
    write_edge_fixture(
        "edge_large_vectors_fixture.vcd",
        "$timescale 1ns $end\n\
         $scope module top $end\n\
         $var wire 128 ! wide [127:0] $end\n\
         $var wire 64 # mid [63:0] $end\n\
         $upscope $end\n\
         $enddefinitions $end\n\
         $dumpvars\n\
         b00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001 !\n\
         b0000000000000000000000000000000000000000000000000000000000000001 #\n\
         $end\n\
         #10\n\
         b10000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000 !\n\
         b1000000000000000000000000000000000000000000000000000000000000000 #\n\
         $end\n",
    )
}

/// Multi-scope hierarchy waveform
pub fn edge_multi_scope_path() -> PathBuf {
    write_edge_fixture(
        "edge_multi_scope_fixture.vcd",
        "$timescale 1ns $end\n\
         $scope module top $end\n\
         $var wire 1 ! a $end\n\
         $scope module m1 $end\n\
         $scope module m2 $end\n\
         $var wire 1 # deep $end\n\
         $upscope $end\n\
         $var wire 1 $ mid $end\n\
         $upscope $end\n\
         $upscope $end\n\
         $enddefinitions $end\n\
         $dumpvars\n\
         0!\n\
         1#\n\
         0$\n\
         $end\n",
    )
}

/// Waveform with no signals at all
pub fn edge_no_signals_path() -> PathBuf {
    write_edge_fixture(
        "edge_no_signals_fixture.vcd",
        "$timescale 1ns $end\n\
         $scope module empty $end\n\
         $upscope $end\n\
         $enddefinitions $end\n\
         $dumpvars\n\
         $end\n",
    )
}

/// Waveform with real-valued signals
pub fn edge_real_values_path() -> PathBuf {
    write_edge_fixture(
        "edge_real_values_fixture.vcd",
        "$timescale 1ns $end\n\
         $scope module top $end\n\
         $var real ! temp $end\n\
         $var wire 1 # flag $end\n\
         $upscope $end\n\
         $enddefinitions $end\n\
         $dumpvars\n\
         r3.14159 !\n\
         0#\n\
         $end\n\
         #10\n\
         r-2.5 !\n\
         1#\n\
         $end\n",
    )
}

/// 4+ signals across a hierarchy (hierarchy.vcd replacement)
pub fn hierarchy_vcd_path() -> PathBuf {
    write_edge_fixture(
        "hierarchy_fixture.vcd",
        "$timescale 1ns $end\n\
         $scope module top $end\n\
         $var wire 1 ! clk $end\n\
         $scope module cpu $end\n\
         $var wire 32 # pc [31:0] $end\n\
         $scope module alu $end\n\
         $var wire 1 $ carry $end\n\
         $var wire 8 % op [7:0] $end\n\
         $upscope $end\n\
         $upscope $end\n\
         $upscope $end\n\
         $enddefinitions $end\n\
         $dumpvars\n\
         0!\n\
         b00000000000000000000000000000000 #\n\
         0$\n\
         b00000000 %\n\
         $end\n\
         #10\n\
         1!\n\
         b00000000000000000000000000010000 #\n\
         1$\n\
         b10101010 %\n\
         $end\n",
    )
}

/// 5+ signals of different var types (types.vcd replacement)
pub fn types_vcd_path() -> PathBuf {
    write_edge_fixture(
        "types_fixture.vcd",
        "$timescale 1ns $end\n\
         $scope module top $end\n\
         $var wire 1 ! wire_sig $end\n\
         $var reg 1 # reg_sig $end\n\
         $var integer 32 $ int_sig [31:0] $end\n\
         $var real % real_sig $end\n\
         $var time 64 & time_sig [63:0] $end\n\
         $var wire 1 ( extra $end\n\
         $upscope $end\n\
         $enddefinitions $end\n\
         $dumpvars\n\
         0!\n\
         0#\n\
         b00000000000000000000000000000000 $\n\
         r1.5 %\n\
         b0000000000000000000000000000000000000000000000000000000000000000 &\n\
         $end\n\
         #10\n\
         1!\n\
         1#\n\
         b00000000000000000000000000000101 $\n\
         r-0.25 %\n\
         b0000000000000000000000000000000000000000000000000000000000000101 &\n\
         $end\n",
    )
}

/// 100+ signals (many_signals.vcd replacement)
pub fn many_signals_vcd_path() -> PathBuf {
    let mut s = String::with_capacity(32 * 1024);
    s.push_str("$timescale 1ns $end\n");
    s.push_str("$scope module top $end\n");
    for i in 0..120 {
        s.push_str(&format!("$var wire 1 {} sig{} $end\n", char::from(b'!' + (i % 94) as u8), i));
    }
    s.push_str("$upscope $end\n");
    s.push_str("$enddefinitions $end\n");
    s.push_str("$dumpvars\n");
    for i in 0..120 {
        s.push_str(&format!("0{}\n", char::from(b'!' + (i % 94) as u8)));
    }
    s.push_str("$end\n");
    s.push_str("#10\n");
    for i in 0..120 {
        s.push_str(&format!("1{}\n", char::from(b'!' + (i % 94) as u8)));
    }
    s.push_str("$end\n");
    write_edge_fixture("many_signals_fixture.vcd", &s)
}

/// 2000+ timestamps (long.vcd replacement)
pub fn long_vcd_path() -> PathBuf {
    let mut s = String::with_capacity(64 * 1024);
    s.push_str("$timescale 1ns $end\n");
    s.push_str("$scope module top $end\n");
    s.push_str("$var wire 1 ! clk $end\n");
    s.push_str("$var wire 8 # data [7:0] $end\n");
    s.push_str("$upscope $end\n");
    s.push_str("$enddefinitions $end\n");
    s.push_str("$dumpvars\n");
    for t in 0..2500usize {
        s.push_str(&format!("#{}\n", t));
        s.push_str(if t % 2 == 0 { "0!\n" } else { "1!\n" });
        s.push_str(&format!("b{:08b} #\n", (t % 256) as u8));
    }
    s.push_str("$end\n");
    write_edge_fixture("long_fixture.vcd", &s)
}

/// Single-signal waveform
pub fn edge_single_signal_path() -> PathBuf {
    write_edge_fixture(
        "edge_single_signal_fixture.vcd",
        "$timescale 1ns $end\n\
         $scope module top $end\n\
         $var wire 1 ! only $end\n\
         $upscope $end\n\
         $enddefinitions $end\n\
         $dumpvars\n\
         0!\n\
         $end\n\
         #5\n\
         1!\n\
         $end\n",
    )
}

/// Signal names longer than 100 characters
pub fn edge_long_names_path() -> PathBuf {
    let long_name = "very_long_signal_name_abcdefghijklmnopqrstuvwxyz_ABCDEFGHIJKLMNOPQRSTUVWXYZ_0123456789_abcdefghijklmnopqrstuvwxyz_extra";
    write_edge_fixture(
        "edge_long_names_fixture.vcd",
        &format!(
            "$timescale 1ns $end\n\
             $scope module top $end\n\
             $var wire 1 ! {long_name} $end\n\
             $upscope $end\n\
             $enddefinitions $end\n\
             $dumpvars\n\
             0!\n\
             $end\n\
             #10\n\
             1!\n\
             $end\n"
        ),
    )
}

/// Multiple value types in one waveform
pub fn edge_all_value_types_path() -> PathBuf {
    write_edge_fixture(
        "edge_all_value_types_fixture.vcd",
        "$timescale 1ns $end\n\
         $scope module top $end\n\
         $var wire 1 ! a $end\n\
         $var wire 4 # b [3:0] $end\n\
         $var real $ c $end\n\
         $upscope $end\n\
         $enddefinitions $end\n\
         $dumpvars\n\
         #0\n\
         0!\n\
         b0000 #\n\
         r0.0 $\n\
         $end\n\
         #10\n\
         1!\n\
         b1010 #\n\
         r3.5 $\n\
         $end\n",
    )
}

/// 1000+ flat signals (large_flat.vcd replacement)
pub fn large_flat_vcd_path() -> PathBuf {
    let mut s = String::with_capacity(256 * 1024);
    s.push_str("$timescale 1ns $end\n");
    s.push_str("$scope module top $end\n");
    for i in 0..1100 {
        s.push_str(&format!("$var wire 1 {} f{} $end\n", char::from(b'!' + (i % 94) as u8), i));
    }
    s.push_str("$upscope $end\n");
    s.push_str("$enddefinitions $end\n");
    s.push_str("$dumpvars\n");
    for t in 0..100usize {
        s.push_str(&format!("#{}\n", t));
        for i in 0..1100 {
            s.push_str(&format!("{}{}\n", (t + i) % 2, char::from(b'!' + (i % 94) as u8)));
        }
    }
    s.push_str("$end\n");
    write_edge_fixture("large_flat_fixture.vcd", &s)
}

/// Small FST fixture built with the hand-rolled FstWriter (2-state + 4-state).
pub fn fst_fixture_path() -> PathBuf {
    use crate::fst::{FstOptions, FstWriter, ScopeType, VarType};
    let p = fixture_dir().join("fst_fixture.fst");
    if p.exists() {
        return p;
    }
    std::fs::create_dir_all(fixture_dir()).unwrap();
    // Atomic write: build into a unique temp file, then rename (parallel tests
    // may generate the same fixture concurrently)
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let uniq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = fixture_dir().join(format!("fst_fixture.tmp{}_{}", std::process::id(), uniq));
    let _ = std::fs::remove_file(&tmp);
    let mut w = FstWriter::create(Path::new(&tmp), FstOptions::default()).unwrap();
    w.push_scope("top", ScopeType::VcdModule);
    let clk = w.create_var("clk", 1, VarType::VcdWire);
    let data = w.create_var("data", 12, VarType::VcdWire);
    let sig = w.create_var("sig", 1, VarType::VcdWire);
    w.pop_scope();
    for t in 0..20u64 {
        w.emit_time_change(t * 100);
        w.emit_value_change(clk, &[if t % 2 == 0 { b'0' } else { b'1' }]);
        w.emit_value_change(data, if t < 10 { b"101011001100".as_slice() } else { b"001100111100".as_slice() });
        w.emit_value_change(sig, &[b'0', b'x', b'1', b'z'][(t % 4) as usize..][..1]);
    }
    w.close().unwrap();
    let _ = std::fs::rename(&tmp, &p);
    p
}
