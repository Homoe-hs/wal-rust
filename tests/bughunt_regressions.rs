//! bug-hunt 回归测试(docs/bug-list.md 中已修条目的最小复现)。
//!
//! 每条对应一个 bug 编号;夹具在临时目录现生成,不依赖外部波形。

use wal_rust::wal::ast::Value;
use wal_rust::wal::eval::Evaluator;

fn write_vcd(name: &str, body: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!(
        "wal_bughunt_{}_{}.vcd",
        name,
        std::process::id()
    ));
    std::fs::write(&p, body).expect("写临时 VCD");
    p
}

fn vcd(decls: &str, dump: &str) -> String {
    format!(
        "$timescale 1ns $end\n$scope module top $end\n{decls}$upscope $end\n$enddefinitions $end\n{dump}"
    )
}

fn load_vcd(ev: &mut Evaluator, name: &str, body: &str) -> std::path::PathBuf {
    let p = write_vcd(name, body);
    ev.load_trace(&p.to_string_lossy(), "t").expect("加载 VCD");
    p
}

/// BUG-001: 同一时间戳的多个 `#N` 段只能算**一个**索引。
#[test]
fn bug001_duplicate_timestamps_collapse_to_one_index() {
    let body = vcd(
        "$var wire 1 ! s $end\n",
        "#0\n0!\n#10\n1!\n#10\n0!\n#10\n1!\n#20\n0!\n",
    );
    let mut ev = Evaluator::new();
    let p = load_vcd(&mut ev, "dup_ts", &body);
    // 时间线 = [0, 10, 20] → MAX-INDEX = 2
    assert_eq!(ev.eval("(MAX-INDEX)").unwrap(), Value::Int(2));
    // 同刻多次写入在 getwave 里折叠成一个变更点(值取最后写入的 1)
    assert_eq!(
        ev.eval("(getwave \"s\")").unwrap(),
        Value::List(wal_rust::wal::ast::WList::from_vec(vec![
            Value::List(wal_rust::wal::ast::WList::from_vec(vec![
                Value::Int(0),
                Value::Int(0)
            ])),
            Value::List(wal_rust::wal::ast::WList::from_vec(vec![
                Value::Int(10),
                Value::Int(1)
            ])),
            Value::List(wal_rust::wal::ast::WList::from_vec(vec![
                Value::Int(20),
                Value::Int(0)
            ])),
        ]))
    );
    // 索引空间上的取值也按时间戳折叠
    assert_eq!(ev.eval("(sample-at \"s\" 1)").unwrap(), Value::Int(1));
    let _ = std::fs::remove_file(p);
}

/// BUG-011: `let` 绑定按顺序求值(let*),后面的绑定能看见前面的值。
#[test]
fn bug011_let_bindings_are_sequential() {
    let mut ev = Evaluator::new();
    assert_eq!(
        ev.eval("(let ((x 10) (y (+ x 5))) (+ x y))").unwrap(),
        Value::Int(25)
    );
    // 扁平写法同样按顺序
    assert_eq!(
        ev.eval("(let (x 2 y (* x 3)) (+ x y))").unwrap(),
        Value::Int(8)
    );
}

/// BUG-012: `cond` 的 `else` 是恒真默认分支(且子句 guard 不预求值)。
#[test]
fn bug012_cond_supports_else_clause() {
    let mut ev = Evaluator::new();
    assert_eq!(
        ev.eval("(cond ((= 1 2) \"no\") ((= 1 1) \"yes\") (else \"maybe\"))")
            .unwrap(),
        Value::String("yes".to_string())
    );
    assert_eq!(
        ev.eval("(cond ((= 1 2) \"no\") (else \"maybe\"))").unwrap(),
        Value::String("maybe".to_string())
    );
    // 没有命中且没有 else → nil
    assert_eq!(ev.eval("(cond ((= 1 2) \"no\"))").unwrap(), Value::Nil);
}

/// BUG-003: FST 的可见 scope 列表不含空根 scope。
#[test]
fn bug003_fst_scopes_exclude_empty_root() {
    use wal_rust::fst::{FstOptions, FstWriter, ScopeType, VarType};
    use wal_rust::trace::{FstTrace, Trace};
    let p = std::env::temp_dir().join(format!("wal_bughunt_fst_scopes_{}.fst", std::process::id()));
    {
        let mut w = FstWriter::create(&p, FstOptions::default()).unwrap();
        w.set_timescale(-9);
        w.push_scope("top", ScopeType::VcdModule);
        let h = w.create_var("clk", 1, VarType::VcdWire);
        w.pop_scope();
        w.emit_time_change(0);
        w.emit_value_change(h, &[b'0']);
        w.emit_time_change(10);
        w.emit_value_change(h, &[b'1']);
        w.close().unwrap();
    }
    let tr = FstTrace::load(&p, "t".to_string()).unwrap();
    let scopes = tr.scopes();
    assert_eq!(scopes, vec!["top".to_string()], "空根 scope 不应出现");
    let _ = std::fs::remove_file(p);
}

/// BUG-010: `(at s 负数时间)` 必须报错,不能被 `as u64` 回绕成末拍。
#[test]
fn bug010_at_negative_time_errors() {
    let body = vcd("$var wire 1 ! s $end\n", "#0\n0!\n#10\n1!\n");
    let mut ev = Evaluator::new();
    let p = load_vcd(&mut ev, "at_neg", &body);
    let err = ev.eval("(at \"s\" -5)").err().expect("负时间应报错");
    assert!(err.contains("负时间") || err.contains("不能为负"), "错误文案: {err}");
    let _ = std::fs::remove_file(p);
}

/// 既有约定(曾被误判成 BUG-009): `(sample-at s 越界索引)` 返回 nil(不是报错),
/// 负索引才报错。test_samples/verify_vcd.wal 已固化此行为,这里再守一道。
#[test]
fn sample_at_out_of_range_returns_nil_by_contract() {
    let body = vcd("$var wire 1 ! s $end\n", "#0\n0!\n#10\n1!\n");
    let mut ev = Evaluator::new();
    let p = load_vcd(&mut ev, "sample_oob", &body);
    assert_eq!(ev.eval("(sample-at \"s\" 99)").unwrap(), Value::Nil, "越界按约定返回 nil");
    assert!(ev.eval("(sample-at \"s\" -1)").is_err(), "负索引应报错");
    assert_eq!(ev.eval("(sample-at \"s\" 1)").unwrap(), Value::Int(1));
    let _ = std::fs::remove_file(p);
}
