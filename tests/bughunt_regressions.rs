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


// ============================================================================
// 第四轮(语言/语义覆盖): fn 多 body、递归、边沿计数、多参 count、组、虚拟信号、FST 初值
// ============================================================================

fn clk_vcd() -> String {
    vcd(
        "$var wire 1 ! clk $end\n$var wire 1 \" rst $end\n",
        "$dumpvars\n0!\n1\"\n$end\n#10\n1!\n#20\n0!\n0\"\n#30\n1!\n#40\n0!\n#50\n1!\n",
    )
}

/// BUG-020: `(fn (args) body1 body2 ...)` 在表达式位置必须只**创建闭包**(多 body 包 do),
/// 之前 rest.len()>2 被当成"创建并立即调用", 导致 set! 累加器/多 body 闭包全坏。
#[test]
fn bug020_fn_multi_body_is_closure_not_immediate_call() {
    let mut ev = Evaluator::new();
    // 多 body 闭包: 先 set! 再返回 —— 每次调用应累加(修复前恒为 1)
    ev.eval("(define cnt (let ((n 0)) (fn () (set! n (+ n 1)) n)))").unwrap();
    let v1 = ev.eval("(cnt)").unwrap();
    let v2 = ev.eval("(cnt)").unwrap();
    let v3 = ev.eval("(cnt)").unwrap();
    assert_eq!((v1, v2, v3), (Value::Int(1), Value::Int(2), Value::Int(3)), "set! 累加器必须跨调用保持");
    // IIFE 仍工作
    assert_eq!(ev.eval("((fn (x) (* x 2)) 21)").unwrap(), Value::Int(42));
    // 多 body 返回最后一个表达式
    assert_eq!(ev.eval("((fn (x) (+ x 1) (* x 10)) 4)").unwrap(), Value::Int(40));
}

/// BUG-021: define 的函数自引用(递归) —— 闭包捕获环境里必须能看见自己的名字。
#[test]
fn bug021_recursive_defines_work() {
    let mut ev = Evaluator::new();
    ev.eval("(defun fact (n) (if (<= n 1) 1 (* n (fact (- n 1)))))").unwrap();
    assert_eq!(ev.eval("(fact 5)").unwrap(), Value::Int(120));
    ev.eval("(define fib (fn (n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2))))))").unwrap();
    assert_eq!(ev.eval("(fib 7)").unwrap(), Value::Int(13));
}

/// BUG-022: count-rise/count-fall 必须和 count(rising)/count(falling) 一致:
/// 首个变更要拿**初值**判边沿(初值 0 → 首个 1 是上升沿)。
#[test]
fn bug022_count_rise_fall_include_initial_edge() {
    let body = clk_vcd();
    let mut ev = Evaluator::new();
    let p = load_vcd(&mut ev, "edge_clk", &body);
    let rise = ev.eval("(count-rise \"clk\")").unwrap();
    let fall = ev.eval("(count-fall \"clk\")").unwrap();
    let rise2 = ev.eval("(count (rising \"clk\"))").unwrap();
    let fall2 = ev.eval("(count (falling \"clk\"))").unwrap();
    assert_eq!(rise, Value::Int(3), "10/30/50 三个上升沿");
    assert_eq!(fall, Value::Int(2), "20/40 两个下降沿");
    assert_eq!(rise, rise2);
    assert_eq!(fall, fall2);
    // FST 后端同口径
    let fst = std::env::temp_dir().join(format!("wal_bughunt_edge_{}.fst", std::process::id()));
    let resume = std::env::temp_dir().join(format!("wal_bughunt_edge_{}.resume", std::process::id()));
    wal_rust::vcd::convert::vcd_to_fst_streaming(
        &p, &fst, &resume, std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
    ).unwrap();
    let mut ev_fst = Evaluator::new();
    ev_fst.load_trace(&fst.to_string_lossy(), "t").unwrap();
    assert_eq!(ev_fst.eval("(count-rise \"clk\")").unwrap(), Value::Int(3));
    assert_eq!(ev_fst.eval("(count-fall \"clk\")").unwrap(), Value::Int(2));
    let _ = std::fs::remove_file(p);
    let _ = std::fs::remove_file(fst);
    let _ = std::fs::remove_file(resume);
}

/// BUG-023: 多参数 count 里含边沿/混合条件时必须返回**列表**, 不能静默只算第一个参数。
#[test]
fn bug023_multi_arg_count_returns_list_for_edge_conditions() {
    let body = clk_vcd();
    let mut ev = Evaluator::new();
    let p = load_vcd(&mut ev, "multi_count", &body);
    assert_eq!(ev.eval("(count (rising \"rst\"))").unwrap(), Value::Int(0));
    assert_eq!(
        ev.eval("(count (rising \"clk\") (rising \"rst\"))").unwrap(),
        Value::List(wal_rust::wal::ast::WList::from_vec(vec![Value::Int(3), Value::Int(0)]))
    );
    // 混合: 简单条件 + 边沿条件
    let mixed = ev.eval("(count (rising \"clk\") (= (get \"rst\") 1))").unwrap();
    match mixed {
        Value::List(l) => {
            assert_eq!(l.0[0], Value::Int(3));
            assert_eq!(l.0[1], Value::Int(1));
        }
        other => panic!("混合条件应返回列表, 得到 {other:?}"),
    }
    let _ = std::fs::remove_file(p);
}

/// BUG-024/025/026: `groups` 接受符号字面量; `#name` 兼容带点/不带点的组名; in-group 同步 CS。
#[test]
fn bug024_026_groups_and_in_group_semantics() {
    let body = vcd(
        "$scope module in_ $end\n$var wire 1 ! ready $end\n$var wire 1 \" valid $end\n$upscope $end\n$scope module out_ $end\n$var wire 1 $ ready $end\n$upscope $end\n",
        "$dumpvars\n0!\n1\"\n0$\n$end\n#10\n1!\n",
    );
    let mut ev = Evaluator::new();
    let p = load_vcd(&mut ev, "grp_sem", &body);
    // 符号字面量(手册示例写法)必须能拿到组前缀(修复前 (groups ready) → ())
    let groups = format!("{:?}", ev.eval("(groups ready)").unwrap());
    assert!(groups.contains("top.in_."), "groups 前缀应带尾点: {groups}");
    assert!(groups.contains("top.out_."), "{groups}");
    // 带点后缀写法同样可用
    let groups2 = format!("{:?}", ev.eval("(groups \".ready\")").unwrap());
    assert!(groups2.contains("top.in_"), "{groups2}");
    // in-group: 组名带点/不带点都要能解析 #ready; CS 同步
    assert_eq!(ev.eval("(in-group \"top.in_\" #ready)").unwrap(), Value::Int(1));
    assert_eq!(ev.eval("(in-group \"top.in_.\" #ready)").unwrap(), Value::Int(1));
    assert_eq!(
        ev.eval("(in-group \"top.in_\" (list CG CS))").unwrap(),
        Value::List(wal_rust::wal::ast::WList::from_vec(vec![
            Value::String("top.in_".to_string()),
            Value::String("top.in_".to_string()),
        ])),
        "CG 用原组名, CS 用去掉尾点的组名"
    );
    let _ = std::fs::remove_file(p);
}

/// BUG-027: 虚拟信号出现在条件里必须展开它引用的真实信号(不能当常量折叠)。
#[test]
fn bug027_virtual_signal_conditions_are_per_index() {
    let body = clk_vcd();
    let mut ev = Evaluator::new();
    let p = load_vcd(&mut ev, "virt_cond", &body);
    assert_eq!(ev.eval("(count (= (+ clk 1) 2))").unwrap(), Value::Int(3));
    assert_eq!(
        ev.eval("(do (defsig myv (+ clk 1)) (count (= myv 2)))").unwrap(),
        Value::Int(3),
        "虚拟信号条件按每拍求值"
    );
    assert_eq!(
        ev.eval("(do (defsig is1 (= clk 1)) (count is1))").unwrap(),
        Value::Int(3)
    );
    let _ = std::fs::remove_file(p);
}

/// BUG-028: FST `(initial s)` 取时间表第 0 帧的值(之前恒为 "x", 与自身 getwave 首点矛盾)。
#[test]
fn bug028_fst_initial_value_uses_zeroth_frame() {
    use wal_rust::fst::{FstOptions, FstWriter, ScopeType, VarType};
    use wal_rust::trace::{FstTrace, Trace};
    let p = std::env::temp_dir().join(format!("wal_bughunt_fst_init_{}.fst", std::process::id()));
    {
        let mut w = FstWriter::create(&p, FstOptions::default()).unwrap();
        w.set_timescale(-9);
        w.push_scope("top", ScopeType::VcdModule);
        let a = w.create_var("a", 1, VarType::VcdWire);
        w.pop_scope();
        w.emit_time_change(0);
        w.emit_value_change(a, &[b'1']);
        w.emit_time_change(10);
        w.emit_value_change(a, &[b'0']);
        w.close().unwrap();
    }
    let tr = FstTrace::load(&p, "t".to_string()).unwrap();
    assert_eq!(tr.initial_value("top.a"), Some(wal_rust::trace::ScalarValue::Bit(b'1')));
    let mut ev = Evaluator::new();
    ev.load_trace(&p.to_string_lossy(), "t").unwrap();
    assert_eq!(ev.eval("(initial \"top.a\")").unwrap(), Value::Int(1));
    let _ = std::fs::remove_file(p);
}
