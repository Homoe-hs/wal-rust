//! 规范小波黄金断言(tiny2 / tiny_x)。
//!
//! 目的: 给"语义口径"一个**手算黄金值**的锚点,覆盖表达式与 CLI 两条路径
//! (矩阵对拍的是"实现 vs 实现",这里对拍的是"实现 vs 手算")。
//! 数值全部由 IEEE 1364 四值语义 + 本项目 docs/4-state-semantics.md 的口径手算得出。

use wal_rust::wal::ast::Value;
use wal_rust::wal::eval::Evaluator;

/// tiny2: dumpvars 初值 + 时钟/握手/8bit 数据, 11 个索引(ts 0,5,…,50)
const TINY2: &str = "\
$date today $end
$version golden tiny2 $end
$timescale 1ns $end
$scope module tb $end
$var wire 1 ! clk $end
$var wire 1 \" rst_n $end
$var wire 8 # data [7:0] $end
$var wire 1 $ valid $end
$upscope $end
$enddefinitions $end
$dumpvars
0!
0\"
b00000000 #
0$
$end
#5
1!
#10
0!
b00000001 #
1$
#15
1\"
#20
1!
b00000010 #
#25
0!
#30
1!
b00000101 #
#35
0!
#40
1!
b11111111 #
0$
#45
0!
#50
1!
b00000000 #
";

/// tiny_x: 无 dumpvars(初值 x)+ 含 x/z 的 4bit 向量, 4 个索引
const TINY_X: &str = "\
$timescale 1ns $end
$scope module t $end
$var wire 1 ! a $end
$var wire 4 \" b $end
$upscope $end
$enddefinitions $end
#0
x!
b00x1 \"
#10
1!
b0101 \"
#20
0!
b00z1 \"
#30
1!
b0101 \"
";

fn write(name: &str, body: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("wal_golden_{}_{}.vcd", name, std::process::id()));
    std::fs::write(&p, body).unwrap();
    p
}

fn eval(vcd: &std::path::Path, code: &str) -> Value {
    let mut e = Evaluator::new();
    e.load_trace(&vcd.to_string_lossy(), "t").unwrap();
    e.eval(code).unwrap()
}

fn ints(v: Vec<i64>) -> Value {
    Value::List(wal_rust::wal::ast::WList::from_vec(v.into_iter().map(Value::Int).collect()))
}

fn run_cli(args: &[&str]) -> String {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_wal-rust"))
        .args(args).output().expect("spawn wal-rust");
    assert!(out.status.success(), "cli {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn golden_tiny2_values() {
    let p = write("tiny2", TINY2);
    // clk: 0,1,0,1,0,1,0,1,0,1,1 → 上升 5, 下降 4, 变化 9
    assert_eq!(eval(&p, "(count (rising \"clk\"))"), Value::Int(5));
    assert_eq!(eval(&p, "(count (falling \"clk\"))"), Value::Int(4));
    assert_eq!(eval(&p, "(count (changes \"clk\"))"), Value::Int(9));
    // 握手/复位: rst_n 初值 0(dumpvars)→ #15 上升 → 算一次上升沿
    assert_eq!(eval(&p, "(count (rising \"valid\"))"), Value::Int(1));
    assert_eq!(eval(&p, "(count (rising \"rst_n\"))"), Value::Int(1));
    assert_eq!(eval(&p, "(count (changes \"valid\"))"), Value::Int(2));
    // 数据: 5 次变化; =5 保持 2 拍(ts30/35); =255 保持 2 拍(ts40/45); >2 共 4 拍
    assert_eq!(eval(&p, "(count (changes \"data\"))"), Value::Int(5));
    assert_eq!(eval(&p, "(count (= (get \"data\") 5))"), Value::Int(2));
    assert_eq!(eval(&p, "(count (= (get \"data\") 255))"), Value::Int(2));
    assert_eq!(eval(&p, "(count (> (get \"data\") 2))"), Value::Int(4));
    // 变更点列表只含"变化", 不含初值快照
    assert_eq!(eval(&p, "(getwave \"data\")"), Value::List(wal_rust::wal::ast::WList::from_vec(vec![
        Value::List(wal_rust::wal::ast::WList::from_vec(vec![Value::Int(10), Value::Int(1)])),
        Value::List(wal_rust::wal::ast::WList::from_vec(vec![Value::Int(20), Value::Int(2)])),
        Value::List(wal_rust::wal::ast::WList::from_vec(vec![Value::Int(30), Value::Int(5)])),
        Value::List(wal_rust::wal::ast::WList::from_vec(vec![Value::Int(40), Value::Int(255)])),
        Value::List(wal_rust::wal::ast::WList::from_vec(vec![Value::Int(50), Value::Int(0)])),
    ])));
    assert_eq!(eval(&p, "(at \"data\" 30)"),
        Value::List(wal_rust::wal::ast::WList::from_vec(vec![Value::Int(30), Value::Int(5)])));
    // 引擎 == 逐拍(oracle)
    for q in ["(count (rising \"clk\"))", "(count (changes \"data\"))",
              "(count (&& (rising \"clk\") (= (get \"valid\") 1)))",
              "(find (&& (rising \"clk\") (> (get \"data\") 2)))"] {
        let eng = eval(&p, q);
        let step = eval(&p, &q.replacen("(count ", "(count/step ", 1).replacen("(find ", "(find/step ", 1));
        assert_eq!(eng, step, "引擎与逐拍不一致: {}", q);
    }
    let _ = std::fs::remove_file(&p);
}

#[test]
fn golden_tiny_x_four_state_rules() {
    let p = write("tinyx", TINY_X);
    // a: x,1,0,1 → 上升 1(#30 的 0→1), 下降 1(#20), 变化 3(x→1 算变化但不算上升沿)
    assert_eq!(eval(&p, "(count (rising \"a\"))"), Value::Int(1), "x→1 不是上升沿");
    assert_eq!(eval(&p, "(count (falling \"a\"))"), Value::Int(1));
    assert_eq!(eval(&p, "(count (changes \"a\"))"), Value::Int(3));
    // x 不等于 0, 但 != 0 为真
    assert_eq!(eval(&p, "(count (= (get \"a\") 0))"), Value::Int(1));
    assert_eq!(eval(&p, "(count (!= (get \"a\") 0))"), Value::Int(3));
    assert_eq!(eval(&p, "(count (is-x \"a\"))"), Value::Int(1));
    // 含 x/z 的向量: 位串比较, 只有纯 0101 的两拍等于 5
    assert_eq!(eval(&p, "(get \"b\")"), Value::String("00x1".to_string()));
    assert_eq!(eval(&p, "(count (= (get \"b\") 5))"), Value::Int(2));
    assert_eq!(eval(&p, "(count (is-z \"b\"))"), Value::Int(1));
    assert_eq!(eval(&p, "(count (changes \"b\"))"), Value::Int(3));
    assert_eq!(eval(&p, "(count/step (= (get \"b\") 5))"), Value::Int(2), "逐拍口径一致");
    let _ = std::fs::remove_file(&p);
}

/// CLI 子命令与表达式必须给出同一个答案(count 的值计数语义)。
#[test]
fn golden_cli_subcommands_match_expressions() {
    let p = write("tiny2cli", TINY2);
    let path = p.to_string_lossy().to_string();
    assert_eq!(run_cli(&["count", &path, "data", "255"]), "2");
    assert_eq!(run_cli(&["count", &path, "data", "5"]), "2");
    assert_eq!(run_cli(&["count", &path, "valid"]), "6", "默认 =1: valid 在 1 上保持 6 拍");
    let sigs = run_cli(&["sigs", &path, "data", "5"]);
    assert!(sigs.contains("tb.data"), "{}", sigs);
    // 会话模式(--stdin): 单进程内多探针, 结果与逐个表达式一致
    let session = {
        use std::io::Write;
        let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_wal-rust"))
            .arg("--stdin").arg("-l").arg(&path)
            .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped())
            .spawn().unwrap();
        c.stdin.take().unwrap().write_all(b"(count (rising \"clk\"))\n(count (changes \"data\"))\n").unwrap();
        let out = c.wait_with_output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    assert_eq!(session, "=> 5\n=> 5", "会话模式结果: {}", session);
    let _ = std::fs::remove_file(&p);
}
