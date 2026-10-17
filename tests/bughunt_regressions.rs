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

/// BUG-004/005: VCD 的 `(SIGNALS)` 用**引用名**(去掉尾部 range),与 FST 一致;
/// 旧写法 `data[3:0]` / `data [3:0]` 的查询仍可用;转义标识符的方括号保留。
#[test]
fn bug004_vcd_signal_names_are_references_only() {
    let body = vcd(
        "$var wire 4 ! data [3:0] $end\n$var wire 1 \" \\a[0]  $end\n",
        "#0\nb0000 !\n0\"\n#10\nb0101 !\n1\"\n",
    );
    let mut ev = Evaluator::new();
    let p = load_vcd(&mut ev, "name_canon", &body);
    let sigs = ev.eval("(SIGNALS)").unwrap();
    let text = format!("{sigs:?}");
    assert!(text.contains("\"top.data\""), "引用名应为 top.data: {text}");
    assert!(!text.contains("top.data [3:0]"), "不应再带 range: {text}");
    assert!(text.contains("\\a[0]"), "转义标识符的方括号要保留: {text}");
    // 三种写法都能解析到同一信号,宽度一致
    for name in ["top.data", "top.data[3:0]", "top.data [3:0]", "data"] {
        let w = ev.eval(&format!("(signal-width \"{name}\")")).unwrap();
        assert_eq!(w, Value::Int(4), "名字 {name} 解析失败");
    }
    let _ = std::fs::remove_file(p);
}

/// BUG-005: FST 的 `(SIGNALS)` 保持**声明序**,不再按字典序重排。
#[test]
fn bug005_fst_signal_order_is_declaration_order() {
    use wal_rust::fst::{FstOptions, FstWriter, ScopeType, VarType};
    use wal_rust::trace::{FstTrace, Trace};
    let p = std::env::temp_dir().join(format!("wal_bughunt_fst_order_{}.fst", std::process::id()));
    {
        let mut w = FstWriter::create(&p, FstOptions::default()).unwrap();
        w.set_timescale(-9);
        w.push_scope("top", ScopeType::VcdModule);
        // 故意用"非字典序"的声明顺序
        let z = w.create_var("z_sig", 1, VarType::VcdWire);
        let a = w.create_var("a_sig", 1, VarType::VcdWire);
        let m = w.create_var("m_sig", 1, VarType::VcdWire);
        w.pop_scope();
        w.emit_time_change(0);
        w.emit_value_change(z, &[b'0']);
        w.emit_value_change(a, &[b'0']);
        w.emit_value_change(m, &[b'0']);
        w.emit_time_change(10);
        w.emit_value_change(z, &[b'1']);
        w.emit_value_change(a, &[b'1']);
        w.emit_value_change(m, &[b'1']);
        w.close().unwrap();
    }
    let tr = FstTrace::load(&p, "t".to_string()).unwrap();
    assert_eq!(
        tr.signals(),
        vec!["top.z_sig".to_string(), "top.a_sig".to_string(), "top.m_sig".to_string()],
        "FST 信号顺序应为声明序"
    );
    let _ = std::fs::remove_file(p);
}

/// BUG-017: VCD→FST 转换必须保留 4-state(x/z),不能折叠成 0。
#[test]
fn bug017_vcd_to_fst_preserves_four_state() {
    let body = vcd(
        "$var wire 4 ! q [3:0] $end\n",
        "#0\nbxxxx !\n#10\nbx00z !\n#20\nb1x1x !\n#30\nbzzzz !\n#40\nb0000 !\n",
    );
    let vcd_path = write_vcd("fst_4state", &body);
    let fst_path = std::env::temp_dir().join(format!("wal_bughunt_fst_4state_{}.fst", std::process::id()));
    let resume = std::env::temp_dir().join(format!("wal_bughunt_fst_4state_{}.resume", std::process::id()));
    wal_rust::vcd::convert::vcd_to_fst_streaming(
        &vcd_path,
        &fst_path,
        &resume,
        std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
    )
    .expect("VCD->FST 转换");
    let mut ev_vcd = Evaluator::new();
    ev_vcd.load_trace(&vcd_path.to_string_lossy(), "t0").unwrap();
    let mut ev_fst = Evaluator::new();
    ev_fst.load_trace(&fst_path.to_string_lossy(), "t1").unwrap();
    assert_eq!(
        ev_vcd.eval("(getwave \"top.q\")").unwrap(),
        ev_fst.eval("(getwave \"top.q\")").unwrap(),
        "转换后 FST 的 x/z 值必须与 VCD 一致"
    );
    let _ = std::fs::remove_file(vcd_path);
    let _ = std::fs::remove_file(fst_path);
    let _ = std::fs::remove_file(resume);
}

/// BUG-008: 截断/非 VCD(缺 `$enddefinitions`)默认报错, 不再静默返回空/部分信号。
#[test]
fn bug008_truncated_vcd_errors_by_default() {
    let good = vcd("$var wire 1 ! s $end\n", "#0\n0!\n#10\n1!\n");
    // 截掉 $enddefinitions 之前的全部内容(只留前 80 字节)
    let cut = &good.as_bytes()[..80.min(good.len())];
    let p = std::env::temp_dir().join(format!("wal_bughunt_trunc_{}.vcd", std::process::id()));
    std::fs::write(&p, cut).unwrap();
    let mut ev = Evaluator::new();
    let err = ev
        .load_trace(&p.to_string_lossy(), "t")
        .err()
        .expect("截断 VCD 应报错");
    assert!(
        err.contains("$enddefinitions") || err.contains("不是完整 VCD"),
        "错误文案: {err}"
    );
    let _ = std::fs::remove_file(p);
}

