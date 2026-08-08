//! Tests for the time-aware waveform builtins: getwave/wave/at/count-rise/
//! edges/period/freq/search/find-sig/assert-eq/save.

mod common;
use common::*;

use wal_rust::wal::eval::Evaluator;
use wal_rust::wal::ast::Value;

fn new_eval(fixture: &std::path::Path) -> Evaluator {
    let mut eval = Evaluator::new();
    eval.load_trace(&fixture.to_string_lossy(), "t0").unwrap();
    eval
}

fn eval_str(eval: &mut Evaluator, src: &str) -> String {
    match eval.eval(src) {
        Ok(v) => format!("{}", v),
        Err(e) => format!("ERR {}", e),
    }
}

#[test]
fn test_getwave_change_points() {
    let mut eval = new_eval(&fst_fixture_path());
    // top.clk toggles every 100ns over 20 timestamps
    let r = eval_str(&mut eval, "(getwave \"top.clk\")");
    // ((0 0) (100 1) (200 0) ...)
    assert!(r.starts_with("((0 0) (100 1)"), "got {}", r);
    let parts = r.split(") (").count();
    assert!(parts >= 20, "expected >=20 change points, got {}", parts);
}

#[test]
fn test_wave_window_and_at() {
    let mut eval = new_eval(&fst_fixture_path());
    let w = eval_str(&mut eval, "(wave \"top.clk\" 150 450)");
    // held value before 150: 100ns=1; changes at 200,300,400
    assert_eq!(w, "((100 1) (200 0) (300 1) (400 0))");
    let at1 = eval_str(&mut eval, "(at \"top.clk\" 150)");
    assert_eq!(at1, "(100 1)");
    let at2 = eval_str(&mut eval, "(at \"top.clk\" 200)");
    assert_eq!(at2, "(200 0)");
}

#[test]
fn test_edge_counts_and_period() {
    let mut eval = new_eval(&fst_fixture_path());
    assert_eq!(eval_str(&mut eval, "(count-rise \"top.clk\")"), "10");
    assert_eq!(eval_str(&mut eval, "(count-fall \"top.clk\")"), "9");
    assert_eq!(eval_str(&mut eval, "(length (edges \"top.clk\" 0 500))"), "6");
    // clk period = 200ns; fixture timescale = 1ns
    let p = eval_str(&mut eval, "(period \"top.clk\")");
    let seconds: f64 = p.parse().expect("period should be a float");
    assert!((seconds - 200e-9).abs() < 1e-12, "period {} != 200ns", seconds);
    let f = eval_str(&mut eval, "(freq \"top.clk\")");
    let hz: f64 = f.parse().expect("freq should be a float");
    assert!((hz - 5e6).abs() < 1e-3, "freq {} != 5MHz", hz);
}

#[test]
fn test_search_find_sig_assert() {
    let mut eval = new_eval(&fst_fixture_path());
    let s = eval_str(&mut eval, "(length (search \"top.clk\" \"1\" 0 500))");
    assert_eq!(s, "3");
    let fs = eval_str(&mut eval, "(find-sig \"clk\")");
    assert!(fs.contains("top.clk"), "got {}", fs);
    // data == 0b101011001100 for t < 1000 (indices 0..10)
    let ok = eval_str(&mut eval, "(assert-eq \"top.data\" 0 900 0b101011001100)");
    assert_eq!(ok, "true");
    let bad = eval_str(&mut eval, "(assert-eq \"top.data\" 0 900 0)");
    assert_eq!(bad, "false");
}

#[test]
fn test_unknown_operator_suggestion() {
    let mut eval = new_eval(&fst_fixture_path());
    let err = eval_str(&mut eval, "(getwav \"top.clk\")");
    assert!(err.contains("getwave"), "expected suggestion, got {}", err);
}

#[test]
fn test_while_special_form() {
    // while must be lazy: cond/body re-evaluated per iteration, set! advances
    let mut eval = new_eval(&fst_fixture_path());
    eval_str(&mut eval, "(define j 0)");
    eval_str(&mut eval, "(while (< j 3) (set! j (+ j 1)))");
    assert_eq!(eval_str(&mut eval, "j"), "3");
    // nested set! inside while body
    eval_str(&mut eval, "(define acc 0)");
    eval_str(&mut eval, "(define i 0)");
    eval_str(&mut eval, "(while (< i 5) (set! acc (+ acc i)) (set! i (+ i 1)))");
    assert_eq!(eval_str(&mut eval, "acc"), "10");
    assert_eq!(eval_str(&mut eval, "i"), "5");
}

#[test]
fn test_save_csv() {
    let mut eval = new_eval(&fst_fixture_path());
    let out = std::env::temp_dir().join(format!("wal_save_test_{}.csv", std::process::id()));
    let src = format!("(save \"{}\" \"top.clk\")", out.display());
    eval_str(&mut eval, &src);
    let csv = std::fs::read_to_string(&out).unwrap();
    assert!(csv.starts_with("time,top.clk\n"), "csv header: {}", csv);
    let lines = csv.lines().count();
    assert!(lines >= 20, "expected >=20 rows, got {}", lines);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn test_vcd_change_points_include_first() {
    use wal_rust::trace::{Trace, VcdTrace};
    let t = VcdTrace::load(&counter_vcd_path(), "t".into()).unwrap();
    let cp = t.change_points("counter_tb.clk").unwrap();
    assert!(!cp.is_empty());
    // first point is at index 0 (clk[0] = 0)
    assert_eq!(cp[0].0, 0);
    let timed: Vec<u64> = cp.iter().map(|(i, _)| t.timestamp_at(*i).unwrap()).collect();
    assert_eq!(timed[0], 0);
    assert!(timed.windows(2).all(|w| w[0] < w[1]), "timestamps must be ascending");
}
