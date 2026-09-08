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
#0\nb00000010 !\n0\"\nb00x1 #\n#10\nb00000011 !\n1\"\nb0101 #\n#20\nb00001111 !\n0\"\nb0000 #\n#30\nb11111111 !\n1\"\nb0011 #\n#40\nb00000010 !\n0\"\nb0101 #\n");
    let fst = std::env::temp_dir().join(format!("wal_reg_both_{}.fst", std::process::id()));
    {
        let mut w = FstWriter::create(&fst, FstOptions::default()).unwrap();
        w.push_scope("t", ScopeType::VcdModule);
        let d = w.create_var("d", 8, VarType::VcdWire);
        let clk = w.create_var("clk", 1, VarType::VcdWire);
        let vec = w.create_var("vec", 4, VarType::VcdWire);
        w.pop_scope();
        let dv: [&[u8]; 5] = [b"00000010", b"00000011", b"00001111", b"11111111", b"00000010"];
        let cv: [&[u8]; 5] = [b"0", b"1", b"0", b"1", b"0"];
        let vv: [&[u8]; 5] = [b"00x1", b"0101", b"0000", b"0011", b"0101"];
        for i in 0..5u64 {
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
    ];
    for q in queries {
        let a = ev(&vcd, q);
        let b = ev(&fst, q);
        assert_eq!(a, b, "VCD/FST divergence on {}", q);
    }
    let _ = std::fs::remove_file(&fst);
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
    let first = eval_with(&p, q);          // 未命中 → 加载 + 写缓存
    let second = eval_with(&p, q);         // 命中 → 读缓存
    assert_eq!(first, second, "cache hit must match the cold result");
    assert!(std::fs::read_dir(&dir).map(|d| d.count() > 0).unwrap_or(false),
        "cache file should exist after a load");
    // 波形内容变化 → mtime/指纹变化 → 失效重建, 结果更新
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(&p, "$timescale 1ns $end\n$scope module t $end\n$var wire 8 ! d $end\n$enddefinitions $end\n\
#0\nb00000100 !\n#10\nb00000101 !\n#20\nb00000110 !\n").unwrap();
    let third = eval_with(&p, q);
    assert_eq!(third, Value::Int(3), "after invalidation the new waveform must be read");
    std::env::remove_var("WAL_CACHE_DIR");
    let _ = std::fs::remove_dir_all(&dir);
}
