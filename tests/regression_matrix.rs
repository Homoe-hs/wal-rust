//! 回归矩阵: 汇聚历届 bug 的修复语义,每项断言=当时修复的口径。
//! 运行: bash scripts/diff_find.sh 之外的第二道闸;P1(统一引擎)落地后
//! 该矩阵必须原样全绿(语义冻结的验收标准)。
use wal_rust::wal::eval::Evaluator;
use wal_rust::wal::ast::Value;
use wal_rust::trace::{FindCondition, ScalarValue, Trace, VcdTrace};
use wal_rust::wal::ast::WList;

fn tmp(name: &str, body: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("wal_reg_{}_{}_{}.vcd", name, std::process::id(), body.len()));
    std::fs::write(&p, body).unwrap();
    p
}

fn load(vcd: &std::path::Path) -> VcdTrace {
    VcdTrace::load(vcd, "t".to_string()).unwrap()
}

fn eval_with(vcd: &std::path::Path, code: &str) -> Value {
    let mut e = Evaluator::new();
    e.load_trace(&vcd.to_string_lossy(), "t").unwrap();
    e.eval(code).unwrap()
}

/// 1) delta 周期: 同一时间戳双写 → per-index 最后写入胜出;
///    count(字面量)==count(变量)==get(该索引) 三者一致(P0-b 根)。
#[test]
fn matrix_delta_cycle_last_write() {
    let p = tmp("delta", "$timescale 1ns $end\n$scope module t $end\n$var wire 4 ! v $end\n$enddefinitions $end\n\
#0\nb0000 !\n#10\nb1100 !\nb0011 !\n#20\nb0000 !\n");
    let t = load(&p);
    let name = t.signals().iter().find(|s| s.ends_with("v")).unwrap().clone();
    assert_eq!(t.signal_value(&name, 1).unwrap(), wal_rust::trace::ScalarValue::Vector(b"0011".to_vec()));
    assert_eq!(t.find_indices(&name, FindCondition::ValueI64(3)).unwrap(), vec![1]);
    let e = |c: &str| eval_with(&p, c);
    // fast path == per-step oracle(值语义由 oracle 裁决,不写死黄金数)
    assert_eq!(e("(count (= (get \"t.v\") 3))"), e("(count/step (= (get \"t.v\") 3))"));
    assert_eq!(e("(define v (get \"t.v\")) (count (= (get \"t.v\") v))"),
               e("(define v (get \"t.v\")) (count/step (= (get \"t.v\") v))"));
    assert!(e("(count (= (get \"t.v\") 3))") == Value::Int(1));
}

/// 2) 初值 x 段: count(is-x) 覆盖 0..首变化;is-x|x→x 不算 change(P0-b-1)。
#[test]
fn matrix_initial_x_run() {
    let p = tmp("initx", "$timescale 1ns $end\n$scope module t $end\n$var wire 8 ! v $end\n$enddefinitions $end\n\
#0\n#10\nbxxxxxxxx !\n#20\nb00001111 !\n#30\nb00000000 !\n");
    let e = |c: &str| eval_with(&p, c);
    assert_eq!(e("(count (is-x \"t.v\"))"), Value::Int(2), "idx0 x-run + idx1");
    assert_eq!(e("(count (changes \"t.v\"))"), Value::Int(2), "x→x 不算变化");
}

/// 3) $dumpvars 初值快照: get@0/at 首变化前 = 快照值(B5/B11)。
#[test]
fn matrix_dumpvars_initial() {
    let p = tmp("dv", "$timescale 1ns $end\n$scope module t $end\n$var wire 8 ! v $end\n$var wire 1 \" r $end\n$enddefinitions $end\n\
$dumpvars\nb00001111 !\n1\"\n$end\n#0\n#10\nb10101010 !\n#20\n0\"\n");
    let e = |c: &str| eval_with(&p, c);
    let list = |a: i64, b: i64| Value::List(WList::from_vec(vec![Value::Int(a), Value::Int(b)]));
    assert_eq!(e("(at \"t.v\" 0)"), list(0, 15));
    assert_eq!(e("(at \"t.r\" 0)"), list(0, 1));
    assert_eq!(e("(count/step (is-x \"t.v\"))"), Value::Int(0));
}

/// 4) P0-a: (&& (get-simple) (is-x ...)) 不得丢谓词。
#[test]
fn matrix_compound_is_x_not_dropped() {
    let p = tmp("a", "$timescale 1ns $end\n$scope module t $end\n$var wire 1 ! en $end\n$var wire 4 \" dat $end\n$enddefinitions $end\n\
#0\n1!\nb00x0\"\n#10\n1!\nb0010\"\n#20\n0!\nb0010\"\n#30\n1!\nb0010\"\n");
    let e = |c: &str| eval_with(&p, c);
    // && 分解路径必须与逐拍 oracle 一致(且不丢子谓词: 至少 en 与 is-x 都生效)
    assert_eq!(e("(count (&& (= (get \"t.en\") 1) (is-x \"t.dat\")))"),
               e("(count/step (&& (= (get \"t.en\") 1) (is-x \"t.dat\")))"));
    // P0-a 判别: 若 is-x 被丢, 结果为 en 计数 3; 参与则仅 idx0 满足
    assert_eq!(e("(count (&& (= (get \"t.en\") 1) (is-x \"t.dat\")))"), Value::Int(1));
    assert_eq!(e("(count (= (get \"t.en\") 1))"), Value::Int(3));
}

/// 5) P0-b 扫描起点: step 后 count 仍全时间线;INDEX 恢复。
#[test]
fn matrix_count_from_zero_and_cursor_restore() {
    let p = tmp("step", "$timescale 1ns $end\n$scope module t $end\n$var wire 1 ! c $end\n$enddefinitions $end\n\
#0\n0!\n#10\n1!\n#20\n0!\n#30\n1!\n#40\n0!\n");
    let mut e = Evaluator::new();
    e.load_trace(&p.to_string_lossy(), "t").unwrap();
    e.eval("(step 2)").unwrap();
    assert_eq!(e.eval("(count (= (get \"t.c\") 1))").unwrap(), Value::Int(2));
    assert_eq!(e.eval("INDEX").unwrap(), Value::Int(2));
}

/// 6) B9: set! 词法穿透(共享 cell)+ fn 局部 define 每次独立。
#[test]
fn matrix_set_penetrates_closure() {
    let p = tmp("b9", "$timescale 1ns $end\n$scope module t $end\n$var wire 1 ! c $end\n$enddefinitions $end\n#0\n0!\n");
    let mut e = Evaluator::new();
    e.load_trace(&p.to_string_lossy(), "t").unwrap();
    e.eval("(define b 0)(define f (fn [] (set! b 9)))(f)").unwrap();
    assert_eq!(e.eval("b").unwrap(), Value::Int(9));
    e.eval("(define acc 0)(define tick (fn [] (set! acc (+ acc 1)))) (tick) (tick)").unwrap();
    assert_eq!(e.eval("acc").unwrap(), Value::Int(2));
    e.eval("(define ctr (fn [] (define n 0) (set! n (+ n 1)) n))").unwrap();
    assert_eq!(e.eval("(ctr)").unwrap(), Value::Int(1));
    assert_eq!(e.eval("(ctr)").unwrap(), Value::Int(1));
}

/// 7) B2: fold 闭包;8) B3: search 每变更点一次;13) B13: import 参数绑定。
#[test]
fn matrix_fold_search_import() {
    let p = tmp("b2", "$timescale 1ns $end\n$scope module t $end\n$var wire 4 ! v $end\n$enddefinitions $end\n\
#0\nb0101 !\n#10\nb0110 !\n");
    let e = |c: &str| eval_with(&p, c);
    assert_eq!(e("(fold (fn [a b] (+ a b)) 0 (list 1 2 3))"), Value::Int(6));
    assert_eq!(e("(search \"t.v\" \"01\")"), Value::List(WList::from_vec(vec![Value::Int(0), Value::Int(10)])));

    let lib = tmp("lib", "(define inc (fn [x] (+ x 1)))");
    let mut e4 = Evaluator::new();
    e4.load_trace(&p.to_string_lossy(), "t").unwrap();
    e4.eval(&format!("(import \"{}\")", lib.display())).unwrap();
    assert_eq!(e4.eval("(inc 41)").unwrap(), Value::Int(42));
    let _ = std::fs::remove_file(&lib);
}

/// 9) B12: 多 trace 查询(信号在第二条 trace)。
#[test]
fn matrix_multi_trace_get() {
    let p2 = tmp("b12", "$timescale 1ns $end\n$scope module t $end\n$var wire 8 ! v $end\n$enddefinitions $end\n\
#0\nb10101010 !\n");
    let mut e = Evaluator::new();
    // 第一条 trace 无 v;第二条有 → 必须解析到
    e.load_trace(&p2.to_string_lossy(), "t1").unwrap();
    e.load_trace(&p2.to_string_lossy(), "t2").unwrap();
    assert_eq!(e.eval("(get \"v\")").unwrap(), Value::Int(0b10101010));
}

/// 10) 列缓存 == 锚定路径(预算 0 vs 充足),计数一致。
#[test]
fn matrix_column_equals_anchored() {
    let p = tmp("col", "$timescale 1ns $end\n$scope module t $end\n$var wire 4 ! v $end\n$enddefinitions $end\n\
#0\nb0000 !\n#10\nb1100 !\nb0011 !\n#20\nb0000 !\n#30\nb1010 !\n");
    let run = |env: &str| -> Value {
        std::env::set_var("WAL_COL_CACHE_MB", env);
        let mut e = Evaluator::new();
        e.load_trace(&p.to_string_lossy(), "t").unwrap();
        e.eval("(count (= (get \"t.v\") 3))").unwrap()
    };
    assert_eq!(run("0"), run("1024"));
    std::env::remove_var("WAL_COL_CACHE_MB");
}

/// 内网轮(0.12.10)新增: >64 位向量与整数值比较 — 高位全 0 且低 64 位等于目标
/// 才算匹配(旧的 to_i64 对 >64 位直接 None → 静默给 0)。
#[test]
fn matrix_wide_vector_int_compare() {
    // 128-bit 信号: idx0 = 000…011111111(=255), idx1 = 000…010000000(=128)
    let zero120 = "0".repeat(120);
    let p = tmp("wide", &format!("$timescale 1ns $end\n$scope module t $end\n$var wire 128 ! d $end\n$enddefinitions $end\n\
#0\nb{}{}!\n#10\nb{}{}!\n", zero120, "11111111", zero120, "10000000"));
    let e = |c: &str| eval_with(&p, c);
    assert_eq!(e("(count (= (get \"t.d\") 255))"), Value::Int(1));
    assert_eq!(e("(count (= (get \"t.d\") 128))"), Value::Int(1));
    assert_eq!(e("(count/step (= (get \"t.d\") 255))"), Value::Int(1));
}

/// P1 统一引擎: 变更点并集区间扫描 —— 任意表达式快路径 == 逐拍 oracle。
#[test]
fn matrix_interval_engine_equals_oracle() {
    let p = tmp("eng", "$timescale 1ns $end\n$scope module t $end\n$var wire 8 ! d $end\n$enddefinitions $end\n\
#0\nb00000010 !\n#10\nb00000011 !\n#20\nb00001111 !\n#30\nb11111111 !\n#40\nb00000010 !\n");
    let e = |c: &str| eval_with(&p, c);
    // 区间恒定条件(无 x/z, 逐拍 oracle 可用)
    assert_eq!(e("(count (&& (> (get \"t.d\") 2) (< (get \"t.d\") 200)))"),
               e("(count/step (&& (> (get \"t.d\") 2) (< (get \"t.d\") 200)))"));
    assert_eq!(e("(count (|| (= (get \"t.d\") 2) (= (get \"t.d\") 255)))"),
               e("(count/step (|| (= (get \"t.d\") 2) (= (get \"t.d\") 255)))"));
    // 边沿谓词参与(只在边界为真)
    assert_eq!(e("(count (&& (changes \"t.d\") (> (get \"t.d\") 2)))"),
               e("(count/step (&& (changes \"t.d\") (> (get \"t.d\") 2)))"));
    assert_eq!(e("(count (rising \"t.d\"))"), e("(count/step (rising \"t.d\"))"));
    // find 与 oracle 一致(区间展开)
    let a = e("(length (find (&& (> (get \"t.d\") 2) (< (get \"t.d\") 200))))");
    let b = e("(length (find/step (&& (> (get \"t.d\") 2) (< (get \"t.d\") 200))))");
    assert_eq!(a, b);
    // 引擎不污染游标
    let mut ev = Evaluator::new();
    ev.load_trace(&p.to_string_lossy(), "t").unwrap();
    ev.eval("(step 3)").unwrap();
    ev.eval("(count (&& (> (get \"t.d\") 2) (< (get \"t.d\") 200)))").unwrap();
    assert_eq!(ev.eval("INDEX").unwrap(), Value::Int(3));
}

/// P1: whenever / count-step / find-step 走同一引擎,且不污染游标。
#[test]
fn matrix_engine_whenever_and_steps() {
    let p = tmp("eng2", "$timescale 1ns $end\n$scope module t $end\n$var wire 8 ! d $end\n$enddefinitions $end\n\
#0\nb00000010 !\n#10\nb00000011 !\n#20\nb00001111 !\n#30\nb11111111 !\n#40\nb00000010 !\n");
    let e = |c: &str| eval_with(&p, c);
    let cond = "(&& (> (get \"t.d\") 2) (< (get \"t.d\") 200))";
    // whenever 引擎路径的执行次数 == 逐拍命中数
    assert_eq!(
        e(&format!("(do (define c 0) (whenever {} (set! c (+ c 1))) c)", cond)),
        e(&format!("(count/step {})", cond))
    );
    // 边沿条件 whenever 同理
    assert_eq!(
        e("(do (define c 0) (whenever (changes \"t.d\") (set! c (+ c 1))) c)"),
        e("(count/step (changes \"t.d\"))")
    );
    // count/step 与 find/step 均走引擎, 结果 == 区间语义
    assert_eq!(e(&format!("(count/step {})", cond)), e(&format!("(count {})", cond)));
    assert_eq!(e(&format!("(length (find/step {}))", cond)),
               e(&format!("(length (find {}))", cond)));
    // 引擎路径不污染游标(count / count-step / find / whenever)
    let mut ev = Evaluator::new();
    ev.load_trace(&p.to_string_lossy(), "t").unwrap();
    ev.eval("(step 3)").unwrap();
    ev.eval(&format!("(count {})", cond)).unwrap();
    assert_eq!(ev.eval("INDEX").unwrap(), Value::Int(3));
    ev.eval(&format!("(count/step {})", cond)).unwrap();
    assert_eq!(ev.eval("INDEX").unwrap(), Value::Int(3));
    ev.eval(&format!("(find {})", cond)).unwrap();
    assert_eq!(ev.eval("INDEX").unwrap(), Value::Int(3));
    ev.eval("(whenever (changes \"t.d\") (define zz 1))").unwrap();
    assert_eq!(ev.eval("INDEX").unwrap(), Value::Int(3));
}

/// VCD 与 FST 后端语义一致性: 同一波形的等价查询必须给出相同结果
/// (内网历轮 VCD/FST 分叉的回归防线)。
#[test]
fn matrix_vcd_fst_consistency() {
    use wal_rust::fst::{FstOptions, FstWriter, ScopeType, VarType};
    // 同一波形: d = 2,3,15,255,2 (8bit); clk 0/1 交替; vec 含 x
    let vcd = tmp("both", "$timescale 1ns $end\n$scope module t $end\n$var wire 8 ! d $end\n$var wire 1 \" clk $end\n$var wire 4 # vec $end\n$enddefinitions $end\n\
#0\nb00000010 !\n0\"\nb00x1 #\n#10\nb00000011 !\n1\"\nb0101 #\n#20\nb00001111 !\n0\"\nb0000 #\n#30\nb11111111 !\n1\"\nb0011 #\n#40\nb00000010 !\n0\"\nb0101 #\n#50\n1\"\n");
    let fst = std::env::temp_dir().join(format!("wal_reg_both_{}.fst", std::process::id()));
    {
        let mut w = FstWriter::create(&fst, FstOptions::default()).unwrap();
        w.push_scope("t", ScopeType::VcdModule);
        let d = w.create_var("d", 8, VarType::VcdWire);
        let clk = w.create_var("clk", 1, VarType::VcdWire);
        let vec = w.create_var("vec", 4, VarType::VcdWire);
        w.pop_scope();
        let dv: [&[u8]; 6] = [b"00000010", b"00000011", b"00001111", b"11111111", b"00000010", b"00000010"];
        let cv: [&[u8]; 6] = [b"0", b"1", b"0", b"1", b"0", b"1"];
        let vv: [&[u8]; 6] = [b"00x1", b"0101", b"0000", b"0011", b"0101", b"0101"];
        for i in 0..6u64 {
            w.emit_time_change(i * 10);
            w.emit_value_change(d, dv[i as usize]);
            w.emit_value_change(clk, cv[i as usize]);
            w.emit_value_change(vec, vv[i as usize]);
        }
        w.close().unwrap();
    }

    let ev = |path: &std::path::Path, code: &str| -> Value {
        let mut e = Evaluator::new();
        e.load_trace(&path.to_string_lossy(), "t").unwrap();
        e.eval(code).unwrap()
    };
    let queries = [
        "(count (= (get \"d\") 2))",
        "(count (!= (get \"d\") 3))",
        "(count (> (get \"d\") 2))",
        "(count (&& (> (get \"d\") 2) (< (get \"d\") 200)))",
        "(count (rising \"clk\"))",
        "(count (falling \"clk\"))",
        "(count (changes \"clk\"))",
        "(count (is-x \"vec\"))",
        "(count/step (is-x \"vec\"))",
        "(at \"d\" 25)",
        "(at \"vec\" 5)",
        "(length (find (&& (> (get \"d\") 2) (< (get \"d\") 200))))",
        "(count/step (= (get \"d\") 15))",
        // 混合条件 + "别的信号引入的边界"(idx5 只有 clk 变化)
        "(count (&& (rising \"clk\") (= (get \"d\") 2)))",
        "(count (&& (rising \"clk\") (= (get \"vec\") 5)))",
        "(count (&& (falling \"clk\") (= (get \"d\") 2)))",
        "(count (&& (not (rising \"clk\")) (= (get \"d\") 2)))",
        "(count (|| (rising \"clk\") (= (get \"d\") 15)))",
        "(find (&& (rising \"clk\") (= (get \"vec\") 5)))",
    ];
    for q in queries {
        let a = ev(&vcd, q);
        let b = ev(&fst, q);
        assert_eq!(a, b, "VCD/FST divergence on {}", q);
    }
    let _ = std::fs::remove_file(&fst);
}

/// id 后缀歧义(标量行 + 数字 id): 值字符与 id 字符同形(1-bit 值 `0`/`1`,
/// 而 id 也以 `1` 结尾)时, 任何"按行尾匹配 ID"的廉价预筛都必须再用解析出的
/// ID 复核 —— 行 `1 11` 以 `1` 结尾, 但它的 ID 是 `11`。
/// (2026-09-10: 融合索引路径漏了这次比较, count(= (get "a") 1) 由 3 变 4。)
#[test]
fn matrix_digit_id_scalar_suffix() {
    let p = tmp("digid", "$timescale 1ns $end\n$scope module t $end\n$var wire 1 1 a $end\n$var wire 1 11 c $end\n$enddefinitions $end\n\
$dumpvars\n0 1\n1 11\n$end\n#10\n1 1\n#20\n1 11\n#30\n0 1\n1 11\n#40\n1 1\n");
    let e = |c: &str| eval_with(&p, c);
    assert_eq!(e("(count (= (get \"t.a\") 1))"), Value::Int(3));
    assert_eq!(e("(at \"t.a\" 40)"), Value::List(WList::from_vec(vec![Value::Int(40), Value::Int(1)])));
    assert_eq!(e("(count (changes \"t.a\"))"), Value::Int(3));
}

/// id 后缀歧义: 目标 id "1" 不得命中更长的 id "11" 的行(锚定扫描误配回归)。
#[test]
fn matrix_id_suffix_ambiguity() {
    let p = tmp("suf", "$timescale 1ns $end\n$scope module t $end\n$var wire 8 1 a $end\n$var wire 8 11 b $end\n$enddefinitions $end\n\
#0\nb00000001 1\nb00000010 11\n#10\nb00000011 1\nb00000100 11\n");
    let e = |c: &str| eval_with(&p, c);
    assert_eq!(e("(length (getwave \"t.a\"))"), Value::Int(2));
    assert_eq!(e("(length (getwave \"t.b\"))"), Value::Int(2));
    assert_eq!(e("(count (= (get \"t.a\") 1))"), Value::Int(1));
    assert_eq!(e("(count (= (get \"t.b\") 2))"), Value::Int(1));
    assert_eq!(e("(count (= (get \"t.a\") 2))"), Value::Int(0));
}

/// 跨进程缓存(§8): 命中与未命中结果一致; 波形改动后失效。
#[test]
fn matrix_cache_roundtrip_and_invalidation() {
    let dir = std::env::temp_dir().join(format!("wal_reg_cache_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::env::set_var("WAL_CACHE_DIR", &dir);
    let p = tmp("cache", "$timescale 1ns $end\n$scope module t $end\n$var wire 8 ! d $end\n$enddefinitions $end\n\
#0\nb00000010 !\n#10\nb00000011 !\n#20\nb00001111 !\n");
    let q = "(count (&& (> (get \"t.d\") 2) (< (get \"t.d\") 200)))";
    // auto 模式对小于阈值的波形**不写**缓存(小文件解析本来就快,避免污染目录)
    let _ = eval_with(&p, q);
    assert!(std::fs::read_dir(&dir).map(|d| d.count() == 0).unwrap_or(true),
        "auto 模式不应为小文件写缓存文件");
    // build 显式写回 → read/auto 命中必须与冷加载一致
    std::env::set_var("WAL_CACHE", "build");
    let first = eval_with(&p, q);
    std::env::remove_var("WAL_CACHE");
    assert!(std::fs::read_dir(&dir).map(|d| d.count() > 0).unwrap_or(false),
        "build 模式应写出缓存文件");
    std::env::set_var("WAL_CACHE", "read");
    let second = eval_with(&p, q);
    std::env::remove_var("WAL_CACHE");
    assert_eq!(first, second, "cache hit must match the cold result");
    // 波形内容变化 → mtime/指纹变化 → 失效重建, 结果更新
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(&p, "$timescale 1ns $end\n$scope module t $end\n$var wire 8 ! d $end\n$enddefinitions $end\n\
#0\nb00000100 !\n#10\nb00000101 !\n#20\nb00000110 !\n").unwrap();
    std::env::set_var("WAL_CACHE", "build");
    let third = eval_with(&p, q);
    std::env::remove_var("WAL_CACHE");
    assert_eq!(third, Value::Int(3), "after invalidation the new waveform must be read");
    std::env::remove_var("WAL_CACHE_DIR");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 引擎在多 trace 场景下必须安全回退(信号不在第一条 trace)。
#[test]
fn matrix_engine_multitrace_fallback() {
    let a = tmp("mta", "$timescale 1ns $end\n$scope module a $end\n$var wire 1 ! c $end\n$enddefinitions $end\n#0\n0!\n#10\n1!\n");
    let b = tmp("mtb", "$timescale 1ns $end\n$scope module b $end\n$var wire 8 \" v $end\n$enddefinitions $end\n#0\nb00000010 \"\n#10\nb00000011 \"\n");
    let mut e = Evaluator::new();
    e.load_trace(&a.to_string_lossy(), "t1").unwrap();
    e.load_trace(&b.to_string_lossy(), "t2").unwrap();
    // 引擎可处理的表达式, 但信号在第二条 trace → 必须回退逐拍而不是报错
    let fast = e.eval("(count (= (+ (get \"v\") 1) 3))").unwrap();
    let slow = e.eval("(count/step (= (+ (get \"v\") 1) 3))").unwrap();
    assert_eq!(fast, slow, "multi-trace engine fallback must match the per-step oracle");
    assert_eq!(fast, Value::Int(1));
}

/// FST 同索引毛刺 + 宽向量 与 VCD 的语义一致性(扩展现有一致性矩阵)。
#[test]
fn matrix_vcd_fst_glitch_and_wide() {
    use wal_rust::fst::{FstOptions, FstWriter, ScopeType, VarType};
    // VCD: #10 同拍双写 (1100 → 0011, 最后写入胜出); 128 位向量
    let zero120 = "0".repeat(120);
    let vcd_src = format!("$timescale 1ns $end\n$scope module t $end\n$var wire 4 ! d $end\n$var wire 128 \" w $end\n$enddefinitions $end\n\
#0\nb0000 !\nb{}11111111 \"\n#10\nb1100 !\nb0011 !\nb{}10000000 \"\n#20\nb0000 !\nb{}00000000 \"\n", zero120, zero120, zero120);
    let vcd = tmp("gwide", &vcd_src);
    let fst = std::env::temp_dir().join(format!("wal_reg_gwide_{}.fst", std::process::id()));
    {
        let mut w = FstWriter::create(&fst, FstOptions::default()).unwrap();
        w.push_scope("t", ScopeType::VcdModule);
        let d = w.create_var("d", 4, VarType::VcdWire);
        let wv = w.create_var("w", 128, VarType::VcdWire);
        w.pop_scope();
        w.emit_time_change(0);
        w.emit_value_change(d, b"0000");
        w.emit_value_change(wv, format!("{}11111111", zero120).as_bytes());
        w.emit_time_change(10);
        w.emit_value_change(d, b"1100");
        w.emit_value_change(d, b"0011"); // 同索引毛刺: 最后写入胜出
        w.emit_value_change(wv, format!("{}10000000", zero120).as_bytes());
        w.emit_time_change(20);
        w.emit_value_change(d, b"0000");
        w.emit_value_change(wv, format!("{}00000000", zero120).as_bytes());
        w.close().unwrap();
    }
    let ev = |path: &std::path::Path, code: &str| -> Value {
        let mut e = Evaluator::new();
        e.load_trace(&path.to_string_lossy(), "t").unwrap();
        e.eval(code).unwrap()
    };
    for q in [
        "(count (= (get \"t.d\") 3))",
        "(count (changes \"t.d\"))",
        "(count (= (get \"t.w\") 255))",
        "(count (= (get \"t.w\") 128))",
        "(count (!= (get \"t.w\") 0))",
        "(at \"t.w\" 5)",
        "(count (&& (> (get \"t.w\") 100) (< (get \"t.w\") 300)))",
    ] {
        assert_eq!(ev(&vcd, q), ev(&fst, q), "VCD/FST divergence on {}", q);
    }
    let _ = std::fs::remove_file(&fst);
}

/// 20) 统一引擎: 边沿谓词必须在"任何边界"上比较相邻索引值。
///     回归点: prev 只在信号自身变化处推进 → 别的信号引发的边界上
///     (rising s) 会用陈旧 prev 误报(&& (rising clk) (= (get state) 3) 类查询过计数)。
///     同时验证混合条件(边沿∨电平)在区间内部按长度累加。
#[test]
fn matrix_engine_edge_prev_advances_every_boundary() {
    let p = tmp("prevb", "$timescale 1ns $end\n$scope module t $end\n$var wire 8 ! d $end\n$var wire 1 \" c $end\n$enddefinitions $end\n\
$dumpvars\nb00000011 !\n0\"\n$end\n#0\n#10\n1\"\n#20\n#30\nb00001111 !\n#40\n");
    let e = |c: &str| eval_with(&p, c);
    let ints = |v: Vec<i64>| Value::List(WList::from_vec(v.into_iter().map(Value::Int).collect()));
    // d: idx0..4 = 3,3,3,15,15   c: 0,1,1,1,1 → rising 仅在 idx1
    // 边界 = {0,1,3}; idx3 由 d 的变化引入, 此时 c 未变化 → 不得报 rising
    assert_eq!(e("(count (&& (rising \"t.c\") (= (get \"t.d\") 15)))"), Value::Int(0),
        "idx3 边界由 d 引入, c 未变化 → 不得用陈旧 prev 报 rising");
    assert_eq!(e("(find (&& (rising \"t.c\") (= (get \"t.d\") 15)))"), ints(vec![]));
    assert_eq!(e("(count (&& (rising \"t.c\") (= (get \"t.d\") 3)))"), Value::Int(1));
    assert_eq!(e("(find (&& (rising \"t.c\") (= (get \"t.d\") 3)))"), ints(vec![1]));
    // 混合条件: (|| (rising c) (= (get d) 3)) 的真值集 = {0,1,2}
    assert_eq!(e("(count (|| (rising \"t.c\") (= (get \"t.d\") 3)))"), Value::Int(3));
    assert_eq!(e("(find (|| (rising \"t.c\") (= (get \"t.d\") 3)))"), ints(vec![0, 1, 2]),
        "区间内部电平为真时, find 必须展开区间内全部索引");
    assert_eq!(e("(count (&& (not (rising \"t.c\")) (= (get \"t.d\") 3)))"), Value::Int(2));
    assert_eq!(e("(find (&& (not (rising \"t.c\")) (= (get \"t.d\") 3)))"), ints(vec![0, 2]));
    // INDEX 依赖: 区间扫描不适用(区间内部 INDEX 会变), 结果必须与逐拍一致
    assert_eq!(e("(count (&& (= (get \"t.d\") 3) (= INDEX 2)))"), Value::Int(1));
    assert_eq!(e("(find (&& (= (get \"t.c\") 1) (= INDEX 3)))"), ints(vec![3]));
}

/// 21) find 的 (&& ...)/(|| ...) 分解不得丢弃无法解析的子条件。
///     回归点: 只取"可解析部分"的交集 → (find (&& (rising c) (= (get d) 3)))
///     曾返回电平段 (0 1 2) 而非 (1)。
#[test]
fn matrix_find_decomposition_no_predicate_drop() {
    let p = tmp("fdrop", "$timescale 1ns $end\n$scope module t $end\n$var wire 8 ! d $end\n$var wire 1 \" c $end\n$enddefinitions $end\n\
$dumpvars\nb00000011 !\n0\"\n$end\n#0\n#10\n1\"\n#20\n#30\nb00001111 !\n#40\n");
    let e = |c: &str| eval_with(&p, c);
    let ints = |v: Vec<i64>| Value::List(WList::from_vec(v.into_iter().map(Value::Int).collect()));
    assert_eq!(e("(find (&& (rising \"t.c\") (= (get \"t.d\") 3)))"), ints(vec![1]));
    assert_eq!(e("(find (|| (rising \"t.c\") (= (get \"t.d\") 3)))"), ints(vec![0, 1, 2]));
    // 两个可解析子条件仍走分解(结果必须与语义一致)
    assert_eq!(e("(find (&& (= (get \"t.d\") 3) (= (get \"t.c\") 1)))"), ints(vec![1, 2]));
}

fn run_cli(bin: &str, query: &str, vcd: &std::path::Path, no_engine: bool) -> String {
    let mut cmd = std::process::Command::new(bin);
    cmd.arg(query).arg("-l").arg(vcd);
    if no_engine { cmd.env("WAL_NO_ENGINE", "1"); }
    let out = cmd.output().expect("spawn wal-rust");
    assert!(out.status.success(), "cli failed for {}: {}", query, String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// 22) 独立 oracle: 子进程里用 WAL_NO_ENGINE 禁用统一引擎(纯逐拍路径),
///     逐条件比对 count/find 输出。这条闸不依赖"引擎与逐拍共享的实现"。
#[test]
fn matrix_engine_matches_step_oracle_subprocess() {
    let src = "$timescale 1ns $end\n$scope module t $end\n$var wire 8 ! d $end\n$var wire 1 \" c $end\n$var wire 1 # e $end\n$var wire 4 $ q $end\n$enddefinitions $end\n\
$dumpvars\nb00000011 !\n0\"\n0#\nbxxxx $\n$end\n#0\n#10\n1\"\n#20\nb00001111 !\nb0011 $\n#30\n1#\n#40\n0\"\n#50\nb00000011 !\nbxx00 $\n#60\n0#\n";
    let vcd = tmp("oracle", src);
    let bin = env!("CARGO_BIN_EXE_wal-rust");
    let conds = [
        "(= (get \"t.d\") 3)",
        "(!= (get \"t.d\") 3)",
        "(not (= (get \"t.d\") 15))",
        "(rising \"t.c\")",
        "(falling \"t.c\")",
        "(changes \"t.c\")",
        "(rising \"t.e\")",
        "(falling \"t.e\")",
        "(is-x \"t.q\")",
        "(changes \"t.q\")",
        "(&& (rising \"t.c\") (= (get \"t.e\") 1))",
        "(&& (rising \"t.c\") (= (get \"t.d\") 3))",
        "(&& (rising \"t.e\") (= (get \"t.d\") 15))",
        "(&& (rising \"t.e\") (falling \"t.c\"))",
        "(&& (rising \"t.c\") (changes \"t.e\"))",
        "(|| (rising \"t.c\") (= (get \"t.d\") 15))",
        "(|| (rising \"t.c\") (= (get \"t.d\") 3))",
        "(|| (changes \"t.e\") (falling \"t.c\"))",
        "(|| (is-x \"t.q\") (= (get \"t.d\") 3))",
        "(&& (is-x \"t.q\") (rising \"t.c\"))",
        "(&& (not (rising \"t.c\")) (= (get \"t.d\") 3))",
        "(&& (not (falling \"t.c\")) (is-z \"t.q\"))",
        // INDEX/TS 在区间内部会变化 → 引擎必须放弃(逐拍)
        "(&& (= (get \"t.d\") 3) (= INDEX 2))",
        "(|| (= (get \"t.d\") 3) (= INDEX 5))",
        "(&& (rising \"t.c\") (= INDEX 1))",
        "(&& (= (get \"t.d\") 15) (= TS 30))",
    ];
    for cond in conds {
        for form in ["count", "find"] {
            let q = format!("({} {})", form, cond);
            let engine = run_cli(bin, &q, &vcd, false);
            let step = run_cli(bin, &q, &vcd, true);
            assert_eq!(engine, step, "engine 与纯逐拍 oracle 不一致: {}", q);
        }
    }
}

/// 23) 损坏 FST(值含非法字符,如 vcd2fst 处理"向量单字符值"写出的坏文件):
///     必须给出可读错误(含修复提示),不得 panic、不得静默返回 0。
#[test]
fn matrix_fst_corrupt_value_clean_error() {
    use wal_rust::fst::{FstOptions, FstWriter, ScopeType, VarType};
    let fst = std::env::temp_dir().join(format!("wal_reg_corrupt_{}.fst", std::process::id()));
    {
        let mut w = FstWriter::create(&fst, FstOptions::default()).unwrap();
        w.push_scope("t", ScopeType::VcdModule);
        let d = w.create_var("d", 8, VarType::VcdWire);
        let ok = w.create_var("c", 1, VarType::VcdWire);
        w.pop_scope();
        w.emit_time_change(0);
        w.emit_value_change(d, b"0000zz!!"); // 非法字符 → wellen 解码 panic
        w.emit_value_change(ok, b"0");
        w.emit_time_change(10);
        w.emit_value_change(d, b"00000001");
        w.emit_value_change(ok, b"1");
        w.close().unwrap();
    }
    let mut e = Evaluator::new();
    e.load_trace(&fst.to_string_lossy(), "t").unwrap();
    // 健康信号仍可查(不因别的信号损坏而误报)
    assert_eq!(e.eval("(count (= (get \"t.c\") 1))").unwrap(), Value::Int(1));
    for q in ["(get \"t.d\")", "(count (= (get \"t.d\") 1))", "(count/step (is-x \"t.d\"))", "(at \"t.d\" 1)"] {
        let r = e.eval(q);
        assert!(r.is_err(), "损坏 FST 必须报错, {} 得到 {:?}", q, r);
        let msg = r.unwrap_err();
        assert!(msg.contains("无法解码") || msg.contains("decode"),
            "错误信息应说明解码失败: {}", msg);
    }
    let _ = std::fs::remove_file(&fst);
}

/// 24) 短名/叶子名解析必须对所有读取路径一致(op_get 能解析,边沿/X 谓词也曾不能):
///     内网现象为 count/step、find、whenever 回退路径对短名静默给 0。
#[test]
fn matrix_short_name_resolution_all_paths() {
    let p = tmp("shortname", "$timescale 1ns $end\n$scope module t $end\n$var wire 8 ! d $end\n$var wire 1 \" c $end\n$enddefinitions $end\n\
$dumpvars\nb00000000 !\n0\"\n$end\n#0\n#10\n1\"\nb00001111 !\n#20\n0\"\n#30\nb11110000 !\n");
    let e = |c: &str| eval_with(&p, c);
    // 短名与全名结果必须一致(四条路径)
    assert_eq!(e("(count (rising \"c\"))"), e("(count (rising \"t.c\"))"));
    assert_eq!(e("(count/step (rising \"c\"))"), Value::Int(1));
    assert_eq!(e("(find (rising \"c\"))"), e("(find (rising \"t.c\"))"));
    assert_eq!(e("(count/step (is-x \"d\"))"), e("(count/step (is-x \"t.d\"))"));
    assert_eq!(e("(find (changes \"d\"))"), e("(find (changes \"t.d\"))"));
    assert_eq!(e("(count/step (falling \"c\"))"), Value::Int(1));
    assert_eq!(e("(count/step (changes \"c\"))"), Value::Int(2));
}

/// 25) FST writer: 压缩无收益时必须按 fstapi 约定写原始字节(clen==uclen)。
///     回归点: 11 个时间点、每步 10 → raw=11 字节、zlib 后也 11 字节,
///     旧实现写 zlib 数据但 clen==uclen,读端按未压缩解析 → 整块报废。
#[test]
fn matrix_fst_writer_time_section_uncompressed_fallback() {
    use wal_rust::fst::{FstOptions, FstWriter, ScopeType, VarType};
    let fst = std::env::temp_dir().join(format!("wal_reg_ts11_{}.fst", std::process::id()));
    {
        let mut w = FstWriter::create(&fst, FstOptions::default()).unwrap();
        w.push_scope("t", ScopeType::VcdModule);
        let d = w.create_var("d", 8, VarType::VcdWire);
        w.pop_scope();
        for i in 0..11u64 {
            w.emit_time_change(i * 10);
            w.emit_value_change(d, if i % 2 == 0 { b"00000000" } else { b"00001111" });
        }
        w.close().unwrap();
    }
    let t = wal_rust::trace::FstTrace::load(&fst, "t".to_string()).expect("11 时间点 FST 必须可读");
    assert_eq!(t.max_index() + 1, 11, "时间轴必须完整");
    let name = t.signals().iter().find(|s| s.ends_with('d')).unwrap().clone();
    assert_eq!(t.signal_value(&name, 1).unwrap(), ScalarValue::Vector(b"00001111".to_vec()));
    assert_eq!(t.signal_value(&name, 10).unwrap(), ScalarValue::Vector(b"00000000".to_vec()));
    let _ = std::fs::remove_file(&fst);
}

/// 26) 常量条件折叠: 不引用信号/INDEX 且无副作用的条件在每个索引取值相同,
///     count/find/step 直接 O(1) 给出结果(而非逐索引求值), 且语义与逐拍一致。
#[test]
fn matrix_constant_condition_fold() {
    let p = tmp("constfold", "$timescale 1ns $end\n$scope module t $end\n$var wire 8 ! d $end\n$enddefinitions $end\n\
#0\nb00000010 !\n#10\nb00000011 !\n#20\nb00001111 !\n#30\nb00000010 !\n");
    let e = |c: &str| eval_with(&p, c);
    let ints = |v: Vec<i64>| Value::List(WList::from_vec(v.into_iter().map(Value::Int).collect()));
    let all = e("(count/step (= 1 1))");
    assert_eq!(all, Value::Int(4));
    assert_eq!(e("(count (= 1 1))"), all);
    assert_eq!(e("(count (> 5 3))"), all);
    assert_eq!(e("(count (&& (= 1 1) (= 2 2)))"), all);
    assert_eq!(e("(count (not (= 1 2)))"), all);
    assert_eq!(e("(count (= 1 2))"), Value::Int(0));
    assert_eq!(e("(find (= 1 2))"), ints(vec![]));
    assert_eq!(e("(length (find (= 1 1)))"), all);
    assert_eq!(e("(find (= 1 1) 2)"), ints(vec![0, 1]));
    // 引用信号的表达式不得被当成常量
    assert_eq!(e("(count (= (get \"t.d\") 3))"), e("(count/step (= (get \"t.d\") 3))"));
    assert_eq!(e("(count (= (get \"t.d\") 3))"), Value::Int(1));
}

/// 27) 多 trace: 信号分布在多条 trace 时, 引擎/&&||分解/getwave/at 都必须跨 trace 查找。
///     回归点: 分解优化对"不含该信号的 trace"用原始名去查 → 报错 → 塞空集 → && 恒 0;
///     getwave/wave/at 曾只看第一条 trace。
#[test]
fn matrix_multitrace_engine_and_builtins() {
    let a = tmp("mta2", "$timescale 1ns $end\n$scope module a $end\n$var wire 1 ! c $end\n$enddefinitions $end\n\
#0\n0!\n#10\n1!\n#20\n0!\n#30\n1!\n");
    let b = tmp("mtb2", "$timescale 1ns $end\n$scope module b $end\n$var wire 8 \" v $end\n$enddefinitions $end\n\
#0\nb00000010 \"\n#10\nb00000011 \"\n#20\nb00000011 \"\n#30\nb00000100 \"\n");
    let mut e = Evaluator::new();
    e.load_trace(&a.to_string_lossy(), "t1").unwrap();
    e.load_trace(&b.to_string_lossy(), "t2").unwrap();
    let mut ev = |c: &str| e.eval(c).unwrap();
    // c = 0,1,0,1 → changes {1,2,3}; v = 2,3,3,4 → v==3 {1,2}, v==4 {3}
    assert_eq!(ev("(find (changes \"c\"))"), Value::List(WList::from_vec(vec![Value::Int(1), Value::Int(2), Value::Int(3)])));
    assert_eq!(ev("(count (&& (rising \"c\") (= (get \"v\") 3)))"), Value::Int(1));
    assert_eq!(ev("(count (&& (rising \"c\") (= (get \"v\") 3)))"),
               ev("(count/step (&& (rising \"c\") (= (get \"v\") 3)))"), "引擎必须等于逐拍 oracle");
    assert_eq!(ev("(count (|| (rising \"c\") (= (get \"v\") 4)))"), Value::Int(2));
    assert_eq!(ev("(count (&& (changes \"c\") (= (get \"v\") 3)))"), Value::Int(2));
    // 第二条 trace 里的信号: getwave / at 必须找得到
    assert_eq!(ev("(getwave \"v\")"), Value::List(WList::from_vec(vec![
        Value::List(WList::from_vec(vec![Value::Int(0), Value::Int(2)])),
        Value::List(WList::from_vec(vec![Value::Int(10), Value::Int(3)])),
        Value::List(WList::from_vec(vec![Value::Int(30), Value::Int(4)])),
    ])));
    assert_eq!(ev("(at \"v\" 10)"), Value::List(WList::from_vec(vec![Value::Int(10), Value::Int(3)])));
    // 找不到的信号给出跨 trace 的清晰错误
    let err = e.eval("(getwave \"nosuchsig\")").unwrap_err();
    assert!(err.contains("not found in any loaded trace"), "unexpected error: {}", err);
}

/// 28) 同进程重复查询: 第二次起必须复用已解码变更列(性能),但语义不得变化
///     (缓存的是变更列而非查询结果 → 不同条件仍需正确)。
#[test]
fn matrix_warm_repeat_query_matches_cold() {
    let p = tmp("warm", "$timescale 1ns $end\n$scope module t $end\n$var wire 8 ! d $end\n$var wire 1 \" c $end\n$enddefinitions $end\n\
#0\nb00000010 !\n0\"\n#10\nb00000011 !\n1\"\n#20\nb00001111 !\n0\"\n#30\nb00000011 !\n1\"\n");
    let e = |c: &str| eval_with(&p, c);
    let q = "(count (= (get \"t.d\") 3))";
    let cold = e(q);
    assert_eq!(cold, Value::Int(2));
    assert_eq!(e(q), cold, "第二次同查询必须一致");
    assert_eq!(e(q), cold, "第三次同查询必须一致");
    // 缓存的是变更列: 换条件/换信号仍要正确
    assert_eq!(e("(count (rising \"t.c\"))"), Value::Int(2));
    assert_eq!(e("(find (= (get \"t.d\") 3))"), Value::List(WList::from_vec(vec![Value::Int(1), Value::Int(3)])));
    assert_eq!(e("(count (changes \"t.d\"))"), Value::Int(3));
    assert_eq!(e(q), cold, "交叉查询后原查询仍一致");
}

/// 29) 跨进程列缓存旁挂文件: 首次冷扫描后按信号落盘, 下一个进程直接命中
///     (不再扫全文件)。用子进程验证, 避免进程内环境变量竞态。
#[test]
fn matrix_col_sidecar_cross_process_hit() {
    let bin = env!("CARGO_BIN_EXE_wal-rust");
    let dir = std::env::temp_dir().join(format!("wal_reg_sidecar_{}", std::process::id()));
    let cdir = dir.join("cache");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&cdir).unwrap();
    let vcd = dir.join("w.vcd");
    let mut src = String::from("$timescale 1ns $end\n$scope module t $end\n$var wire 8 ! d $end\n$enddefinitions $end\n");
    for i in 0..64 {
        src.push_str(&format!("#{}\nb{:08b} !\n", i * 10, i % 4));
    }
    std::fs::write(&vcd, src).unwrap();

    let run = |q: &str| -> (String, String) {
        let out = std::process::Command::new(bin)
            .arg(q).arg("-l").arg(&vcd)
            .env("WAL_CACHE", "auto")
            .env("WAL_CACHE_DIR", &cdir)
            .env("WAL_CACHE_MIN_MB", "0")   // 小夹具也允许落盘
            .env("WAL_DEBUG_FIND", "1")
            .output().expect("spawn wal-rust");
        assert!(out.status.success(), "cli 失败: {}", String::from_utf8_lossy(&out.stderr));
        (String::from_utf8_lossy(&out.stdout).trim().to_string(),
         String::from_utf8_lossy(&out.stderr).to_string())
    };

    let (out1, err1) = run("(count (= (get \"t.d\") 1))");
    assert!(!err1.contains("col-sidecar hit"), "首次不应命中: {}", err1);
    let cols_dir = std::fs::read_dir(&cdir).unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.file_name().to_string_lossy().ends_with(".cols"))
        .expect("应生成 .cols 目录");
    assert!(std::fs::read_dir(cols_dir.path()).unwrap().count() > 0, "应写出信号列文件");

    let (out2, err2) = run("(count (= (get \"t.d\") 1))");
    assert!(err2.contains("col-sidecar hit"), "第二次应命中旁挂列缓存: {}", err2);
    assert_eq!(out1, out2, "命中结果必须与冷扫描一致");
    // 换条件仍命中同一列(缓存的是变更列, 不是查询结果)
    let (out3, err3) = run("(find (= (get \"t.d\") 2))");
    assert!(err3.contains("col-sidecar hit"), "不同条件也应命中变更列: {}", err3);
    assert_eq!(out3, "=> (2 6 10 14 18 22 26 30 34 38 42 46 50 54 58 62)");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 30) `==`/`!==` 别名: WAL 词法把 `==` 拆成两个 `=`, 以前 `(== a b)` 静默为 false。
#[test]
fn matrix_double_equals_alias() {
    let p = tmp("eqeq", "$timescale 1ns $end\n$scope module t $end\n$var wire 1 ! c $end\n$enddefinitions $end\n\
#0\n0!\n#10\n1!\n#20\n0!\n");
    let e = |c: &str| eval_with(&p, c);
    // 游标在索引 0, 该处 c=0
    assert_eq!(e("(== (get \"t.c\") 0)"), Value::Bool(true));
    assert_eq!(e("(== (get \"t.c\") 1)"), Value::Bool(false));
    assert_eq!(e("(count (== (get \"t.c\") 1))"), Value::Int(1));
    assert_eq!(e("(count (== (get \"t.c\") 1))"), e("(count (= (get \"t.c\") 1))"));
    assert_eq!(e("(!== (get \"t.c\") 1)"), Value::Bool(true));
    assert_eq!(e("(=== (get \"t.c\") 0)"), Value::Bool(true));
}

/// 31) 1bit 标量的 x 是未知态, 不等于 0(此前 `(get)` 给 0、count/step 与 count 分叉)。
#[test]
fn matrix_scalar_x_is_not_zero() {
    let p = tmp("xz", "$timescale 1ns $end\n$scope module t $end\n$var wire 1 ! v $end\n$enddefinitions $end\n\
#0\nx!\n#10\nx!\n#20\n0!\n#30\n1!\n#40\nx!\n");
    let e = |c: &str| eval_with(&p, c);
    assert_eq!(e("(get \"t.v\")"), Value::String("x".to_string()));
    assert_eq!(e("(count (= (get \"t.v\") 0))"), Value::Int(1), "只有 idx2 的确定 0");
    assert_eq!(e("(count/step (= (get \"t.v\") 0))"), Value::Int(1), "逐拍必须与快路径一致");
    assert_eq!(e("(count (is-x \"t.v\"))"), Value::Int(3));
    // find_indices 直查(与 count 子命令同一路径)
    let t = load(&p);
    let name = t.signals().iter().find(|s| s.ends_with('v')).unwrap().clone();
    assert_eq!(t.find_indices(&name, FindCondition::ValueI64(0)).unwrap(), vec![2]);
}

/// 32) $dumpvars 确定初值 → 索引 0 的首条变化算边沿/变化(此前丢失首个上升沿)。
#[test]
fn matrix_dumpvars_first_edge_at_index_zero() {
    let p = tmp("firstedge", "$timescale 1ns $end\n$scope module t $end\n$var wire 1 ! c $end\n$enddefinitions $end\n\
$dumpvars\n0!\n$end\n#0\n1!\n#10\n0!\n#20\n1!\n");
    let e = |c: &str| eval_with(&p, c);
    // c = 1,0,1: 索引 0 的 0→1 是上升沿
    assert_eq!(e("(count (rising \"t.c\"))"), Value::Int(2));
    assert_eq!(e("(find (rising \"t.c\"))"), Value::List(WList::from_vec(vec![Value::Int(0), Value::Int(2)])));
    assert_eq!(e("(count/step (rising \"t.c\"))"), Value::Int(2));
    assert_eq!(e("(count (falling \"t.c\"))"), Value::Int(1));
    // c = 1,0,1 → 三次变化(含索引 0 的 dumpvars 0→1)
    assert_eq!(e("(count (changes \"t.c\"))"), Value::Int(3));
    assert_eq!(e("(count/step (changes \"t.c\"))"), Value::Int(3));
    // 初值为 x(无 dumpvars)时索引 0 不算边沿
    let p2 = tmp("noxinit", "$timescale 1ns $end\n$scope module t $end\n$var wire 1 ! c $end\n$enddefinitions $end\n\
#0\n1!\n#10\n0!\n");
    let e2 = |c: &str| eval_with(&p2, c);
    assert_eq!(e2("(count (rising \"t.c\"))"), Value::Int(0), "x→1 不是上升沿");
    assert_eq!(e2("(count (changes \"t.c\"))"), Value::Int(1), "x→1 不算变化");
}

/// 33) 拼写错误的信号名必须报错, 不能静默返回 0/()/false。
#[test]
fn matrix_unknown_signal_errors() {
    let p = tmp("unknown", "$timescale 1ns $end\n$scope module t $end\n$var wire 1 ! c $end\n$enddefinitions $end\n#0\n0!\n#10\n1!\n");
    let mut e = Evaluator::new();
    e.load_trace(&p.to_string_lossy(), "t").unwrap();
    for q in [
        "(count (= (get \"nosuch\") 1))",
        "(find (= (get \"nosuch\") 1))",
        "(count (rising \"nosuch\"))",
        "(count (is-x \"nosuch\"))",
        "(count (changes \"nosuch\"))",
        "(count (&& (= (get \"nosuch\") 1) (= (get \"t.c\") 1)))",
    ] {
        let r = e.eval(q);
        assert!(r.is_err(), "{} 必须报错, 得到 {:?}", q, r);
        assert!(r.unwrap_err().contains("not found"), "{}: 错误信息应含 not found", q);
    }
    // 正常查询仍然工作
    assert_eq!(e.eval("(count (= (get \"t.c\") 1))").unwrap(), Value::Int(1));
}

/// 34) 压缩波形/FSDB 必须明确报错, 不能静默给 0。
#[test]
fn matrix_reject_compressed_and_fsdb() {
    let dir = std::env::temp_dir().join(format!("wal_reg_fmt_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let gz = dir.join("a.vcd.gz");
    std::fs::write(&gz, [0x1f, 0x8b, 0x08, 0x00]).unwrap();
    let bz2 = dir.join("a.vcd.bz2");
    std::fs::write(&bz2, b"BZh91AY&SY").unwrap();
    let fsdb = dir.join("a.fsdb");
    std::fs::write(&fsdb, b"\x00\x00\x00\x00FSDB\x00\x00").unwrap();
    for (path, want) in [(&gz, "gzip"), (&bz2, "bzip2"), (&fsdb, "FSDB")] {
        let mut e = Evaluator::new();
        let err = e.load_trace(&path.to_string_lossy(), "t").unwrap_err();
        assert!(err.contains(want), "{}: 错误信息应提到 {}: {}", path.display(), want, err);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// 35) dump-trace: 字符串值跳过并告警、.fst 明确拒绝、输出用真实时间戳。
#[test]
fn matrix_dump_trace_writer_contract() {
    let p = tmp("dumptr", "$timescale 1ns $end\n$scope module t $end\n$var wire 1 ! c $end\n$enddefinitions $end\n\
#0\n0!\n#10\n1!\n#20\n0!\n");
    let out = std::env::temp_dir().join(format!("wal_reg_dump_{}.vcd", std::process::id()));
    let out_s = out.to_string_lossy().to_string();
    let mut e = Evaluator::new();
    e.load_trace(&p.to_string_lossy(), "t").unwrap();
    // 字符串虚拟信号不可表示 → 跳过(不写入非法 VCD), 数值信号正常导出
    e.eval(&format!("(defsig s \"clk\") (defsig v (get \"t.c\")) (dump-trace \"{}\")", out_s)).unwrap();
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(!text.contains("bclk"), "字符串值不得当位串写出: {}", text);
    assert!(text.contains("#10") && text.contains("#20"), "应使用源波形时间戳: {}", text);
    // .fst 路径明确拒绝
    let fst = out.with_extension("fst");
    let err = e.eval(&format!("(dump-trace \"{}\")", fst.to_string_lossy())).unwrap_err();
    assert!(err.contains(".fst"), "{}", err);
    // 导出的 VCD 可被读回
    let mut e2 = Evaluator::new();
    e2.load_trace(&out_s, "t").unwrap();
    assert_eq!(e2.eval("(count (rising \"v\"))").unwrap(), Value::Int(1));
    let _ = std::fs::remove_file(&out);
}

/// 36) 整数除法 (div): 向零取整; `/` 仍是浮点除法。
#[test]
fn matrix_integer_division() {
    let p = tmp("div", "$timescale 1ns $end\n$scope module t $end\n$var wire 1 ! c $end\n$enddefinitions $end\n#0\n0!\n");
    let e = |c: &str| eval_with(&p, c);
    assert_eq!(e("(div 10 3)"), Value::Int(3));
    assert_eq!(e("(div -10 3)"), Value::Int(-3));
    assert_eq!(e("(div 10 -3)"), Value::Int(-3));
    assert_eq!(e("(div 9 3)"), Value::Int(3));
    assert_eq!(e("(/ 10 3)"), Value::Float(10.0 / 3.0));
}

/// 37) 歧义信号名: 有候选但不唯一时必须说"歧义"并列出候选, 而不是"未找到"。
#[test]
fn matrix_ambiguous_signal_message() {
    let p = tmp("ambig", "$timescale 1ns $end\n$scope module top $end\n$var wire 1 ! c $end\n$scope module sub $end\n$var wire 1 \" c $end\n$upscope $end\n$upscope $end\n$enddefinitions $end\n#0\n0!\n0\"\n#10\n1!\n1\"\n");
    let mut e = Evaluator::new();
    e.load_trace(&p.to_string_lossy(), "t").unwrap();
    let err = e.eval("(at \"c\" 10)").unwrap_err();
    assert!(err.contains("ambiguous"), "应提示歧义: {}", err);
    let err2 = e.eval("(at \"nosuchsig\" 10)").unwrap_err();
    assert!(err2.contains("not found"), "不存在的名字应提示未找到: {}", err2);
}

/// 38) VCD 解析健壮性: real 值行(VCS 双空格)、CRLF、制表符分隔、无扩展名(magic 识别)、
///     空 dump 段、$timescale 带乘数、EVCD 明确拒绝。
#[test]
fn matrix_vcd_parser_robustness() {
    // real 值行: VCS 写 "r1.500000000000000  #"(两个空格)
    let real_vcd = "$timescale 1ns $end\n$scope module t $end\n$var real 64 ! r $end\n$enddefinitions $end\n\
#0\nr0.000000000000000  !\n#10\nr1.500000000000000  !\n#20\nr-0.250000000000000  !\n";
    let p = tmp("real2sp", real_vcd);
    let e = |c: &str| eval_with(&p, c);
    assert_eq!(e("(count (changes \"t.r\"))"), Value::Int(2));
    assert_eq!(e("(at \"t.r\" 10)"), Value::List(WList::from_vec(vec![Value::Int(10), Value::Float(1.5)])));
    assert_eq!(e("(at \"t.r\" 20)"), Value::List(WList::from_vec(vec![Value::Int(20), Value::Float(-0.25)])));
    assert_eq!(e("(count (> (get \"t.r\") 0))"), Value::Int(1), "索引 0 是 0.0, 之后 1.5 与 -0.25");

    // CRLF: 行尾 \r 不能影响时间戳/值/变更列表
    let lf = "$timescale 1ns $end\n$scope module t $end\n$var wire 8 ! d $end\n$var wire 1 \" c $end\n$enddefinitions $end\n\
#0\nb00000001 !\n0\"\n#10\nb00000010 !\n1\"\n#20\nb00000011 !\n0\"\n";
    let crlf: String = lf.replace('\n', "\r\n");
    let pl = tmp("lf", lf);
    let pc = tmp("crlf", &crlf);
    for q in ["(count (changes \"d\"))", "(count (rising \"c\"))", "(getwave \"d\")", "(at \"d\" 20)", "(length (SIGNALS))"] {
        assert_eq!(eval_with(&pl, q), eval_with(&pc, q), "CRLF 与 LF 必须一致: {}", q);
    }
    assert_eq!(eval_with(&pc, "(count (changes \"d\"))"), Value::Int(2));

    // 制表符分隔: 值不能被 tab 污染(否则 00000001 会变成 2)
    let tabbed = "$timescale 1ns $end\n$scope module t $end\n$var wire 8 ! d $end\n$enddefinitions $end\n\
#0\nb00000001\t!\n#10\nb00000010\t!\n";
    let pt = tmp("tab", tabbed);
    assert_eq!(eval_with(&pt, "(getwave \"t.d\")"), Value::List(WList::from_vec(vec![
        Value::List(WList::from_vec(vec![Value::Int(0), Value::Int(1)])),
        Value::List(WList::from_vec(vec![Value::Int(10), Value::Int(2)])),
    ])));

    // 无扩展名: 靠 magic($date/$timescale)识别
    let noext = std::env::temp_dir().join(format!("wal_reg_noext_{}", std::process::id()));
    std::fs::write(&noext, lf).unwrap();
    let mut e2 = Evaluator::new();
    e2.load_trace(&noext.to_string_lossy(), "t").unwrap();
    assert_eq!(e2.eval("(count (changes \"t.d\"))").unwrap(), Value::Int(2));

    // 空 dump 段: 时间线为空 → 0, 不报越界
    let empty = "$timescale 1ns $end\n$scope module t $end\n$var wire 1 ! c $end\n$enddefinitions $end\n";
    let pe = tmp("empty", empty);
    assert_eq!(eval_with(&pe, "(count (changes \"t.c\"))"), Value::Int(0));
    assert_eq!(eval_with(&pe, "(count (= (get \"t.c\") 0))"), Value::Int(0));

    // $timescale 带乘数: 10ps = 10^-11 s
    let ts10 = "$timescale 10ps $end\n$scope module t $end\n$var wire 1 ! c $end\n$enddefinitions $end\n#0\n0!\n#10\n1!\n";
    let pt2 = tmp("ts10", ts10);
    let t = load(&pt2);
    assert_eq!(t.timescale_exp(), Some(-11));

    // EVCD($dumpports)明确拒绝
    let evcd = "$date x $end\n$version x $end\n$timescale 1ps $end\n$scope module t $end\n$upscope $end\n$enddefinitions $end\n#0\n$dumpports\n$end\n";
    let pev = std::env::temp_dir().join(format!("wal_reg_evcd_{}.vcd", std::process::id()));
    std::fs::write(&pev, evcd).unwrap();
    let mut e3 = Evaluator::new();
    let err = e3.load_trace(&pev.to_string_lossy(), "t").unwrap_err();
    assert!(err.contains("EVCD"), "{}", err);

    let _ = std::fs::remove_file(&noext);
    let _ = std::fs::remove_file(&pev);
}

/// 39) FST 实数信号: writer/reader 往返 + 变更列表。
///     回归点: real 链编码曾按"位宽 64 的字符"写出 → wellen 解码越界;
///     现在按 fstapi 规则写 LSB=1 的 varint + 8 字节 f64。
#[test]
fn matrix_fst_real_roundtrip() {
    use wal_rust::fst::{FstOptions, FstWriter, ScopeType, VarType};
    let fst = std::env::temp_dir().join(format!("wal_reg_real_{}.fst", std::process::id()));
    {
        let mut w = FstWriter::create(&fst, FstOptions::default()).unwrap();
        w.push_scope("t", ScopeType::VcdModule);
        let r = w.create_var("r", 64, VarType::Real);
        let c = w.create_var("c", 1, VarType::VcdWire);
        w.pop_scope();
        for i in 0..4u64 {
            w.emit_time_change(i * 10);
            w.emit_value_change(r, &(i as f64 * 1.5).to_le_bytes());
            w.emit_value_change(c, if i % 2 == 0 { b"0" } else { b"1" });
        }
        w.close().unwrap();
    }
    let t = wal_rust::trace::FstTrace::load(&fst, "t".to_string()).expect("real FST 必须可读");
    let rname = t.signals().iter().find(|s| s.ends_with('r')).cloned().unwrap();
    assert_eq!(t.find_indices(&rname, FindCondition::Changed).unwrap(), vec![1, 2, 3]);
    assert_eq!(t.signal_value(&rname, 0).unwrap(), ScalarValue::Real(0.0));
    assert_eq!(t.signal_value(&rname, 3).unwrap(), ScalarValue::Real(4.5));
    let _ = std::fs::remove_file(&fst);
}

/// 40) 分块扫描的时间戳归属: 当"单个时间戳的变化行"超过一个 chunk 时,
///     该 chunk 内没有 '#' 的值行曾因 local_idx 缺失被静默丢弃
///     (大文件/每拍变化数极多的波形会丢数据)。这里构造 8 个时间戳 × 200k 行。
#[test]
fn matrix_cross_chunk_timestamp_attribution() {
    let dir = std::env::temp_dir();
    let p = dir.join(format!("wal_reg_chunk_{}.vcd", std::process::id()));
    let n_sig = 200_000usize;
    let n_ts = 8usize;
    {
        use std::io::Write;
        let f = std::fs::File::create(&p).unwrap();
        let mut w = std::io::BufWriter::new(f);
        writeln!(w, "$timescale 1ns $end").unwrap();
        writeln!(w, "$scope module t $end").unwrap();
        for i in 0..n_sig {
            let c1 = 33 + (i % 94);
            let c2 = 33 + ((i / 94) % 94);
            let c3 = 33 + ((i / 94 / 94) % 94);
            writeln!(w, "$var reg 8 {}{}{} sig_{} $end", c1 as u8 as char, c2 as u8 as char, c3 as u8 as char, i).unwrap();
        }
        writeln!(w, "$upscope $end").unwrap();
        writeln!(w, "$enddefinitions $end").unwrap();
        for t in 0..n_ts {
            writeln!(w, "#{}", t * 10).unwrap();
            for i in 0..n_sig {
                let c1 = 33 + (i % 94);
                let c2 = 33 + ((i / 94) % 94);
                let c3 = 33 + ((i / 94 / 94) % 94);
                writeln!(w, "b{:08b} {}{}{}", t, c1 as u8 as char, c2 as u8 as char, c3 as u8 as char).unwrap();
            }
        }
    }
    let t = load(&p);
    // 每个信号都有 n_ts 个变更点(值 0..7 互不相同)
    for name in ["sig_0", "sig_100000", "sig_199999"] {
        let full = t.signals().iter().find(|s| s.contains(name)).cloned()
            .unwrap_or_else(|| panic!("{} not found", name));
        let idx = t.find_indices(&full, FindCondition::Changed).unwrap();
        assert_eq!(idx.len(), n_ts - 1, "{}: 跨 chunk 的变更点不得丢失 (got {:?})", name, idx);
    }
    let _ = std::fs::remove_file(&p);
}
