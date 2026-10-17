//! `(beat ...)` 算子与 `WAL_BEAT_CLK` 拍号显示模式的回归测试。
//!
//! 参数语义(与手册 §2.8 一致):
//! - beat(t) 不带时钟 → 人类可读时间串; 带时钟 → 整数拍号
//! - 拍号 = 上升沿序列中 <= t 的个数 - 1 (首个上升沿 = 第 0 拍, 首沿前 = -1)
//! - WAL_BEAT_CLK 开启后 at/getwave/wave/edges/search 输出拍号;
//!   单时钟整数; 多时钟每个时钟一对 ("clk名" 拍号), 不合并
//! - 时钟无法解析/没有上升沿 → 明确报错并整体回退绝对时间
//! - -1 提示每个进程只打一次

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_wal-rust");

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("wal_beat_{}_{}", tag, std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// clk 上升沿: 10, 50 (下降 30, 70); d: 20→1, 40→0; slow: 60→1; const1 恒 1(无上升沿)
fn fixture(dir: &Path) -> PathBuf {
    let p = dir.join("beat_fix.vcd");
    std::fs::write(
        &p,
        "$timescale 1ns $end\n\
         $scope module top $end\n\
         $var wire 1 ! clk $end\n\
         $var wire 1 \" d $end\n\
         $var wire 1 # slow $end\n\
         $var wire 1 $ const1 $end\n\
         $upscope $end\n\
         $enddefinitions $end\n\
         #0\n0!\n0\"\n0#\n1$\n\
         #10\n1!\n#20\n1\"\n#30\n0!\n#40\n0\"\n#50\n1!\n#60\n1#\n#70\n0!\n",
    )
    .unwrap();
    p
}

struct Out {
    stdout: String,
    stderr: String,
    code: i32,
}

fn run(dir: &Path, wave: &Path, expr: &str, envs: &[(&str, &str)]) -> Out {
    let mut c = Command::new(BIN);
    c.arg(expr).arg("-l").arg(wave).current_dir(dir);
    c.env_remove("WAL_BEAT_CLK");
    for (k, v) in envs {
        c.env(k, v);
    }
    let o = c.output().unwrap();
    Out {
        stdout: String::from_utf8_lossy(&o.stdout).to_string(),
        stderr: String::from_utf8_lossy(&o.stderr).to_string(),
        code: o.status.code().unwrap_or(-1),
    }
}

fn line(o: &Out) -> String {
    o.stdout
        .lines()
        .find(|l| l.trim_start().starts_with("=>"))
        .map(|l| l.trim().to_string())
        .unwrap_or_else(|| format!("<no result>\nstdout={}\nstderr={}", o.stdout, o.stderr))
}

#[test]
fn beat_operator_integer_zero_based_and_negative() {
    let dir = tmpdir("op");
    let wave = fixture(&dir);
    // 首个沿 10 = 第 0 拍; 50 = 第 1 拍; 20/30 在两根沿之间仍第 0 拍
    let o = run(&dir, &wave, "(list (beat 20 \"top.clk\") (beat 50 \"top.clk\"))", &[]);
    assert_eq!(line(&o), "=> (0 1)", "stderr={}", o.stderr);
    let o = run(&dir, &wave, "(beat 5 \"top.clk\")", &[]);
    assert_eq!(line(&o), "=> -1");
    assert!(o.stderr.contains("拍号 -1"), "首沿前应有提示: {}", o.stderr);
    // 不带时钟 → 人类可读时间串
    let o = run(&dir, &wave, "(beat 20)", &[]);
    assert!(line(&o).contains("20.00ns"), "{}", line(&o));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn beat_env_single_clock_rewrites_time_outputs() {
    let dir = tmpdir("single");
    let wave = fixture(&dir);
    // d 的变更: t=20 写 1, t=40 写 0。t=0 的变更点(dumpvars)早于首个沿 → -1
    let cases = [
        ("(at \"top.d\" 30)", "=> (0 1)"),
        ("(getwave \"top.d\")", "=> ((-1 0) (0 1) (0 0))"),
        ("(edges \"top.clk\")", "=> (-1 0 0 1 1)"),
        ("(search \"top.d\" \"1\")", "=> (0)"),
    ];
    for (expr, want) in cases {
        let o = run(&dir, &wave, expr, &[("WAL_BEAT_CLK", "top.clk")]);
        assert_eq!(line(&o), want, "expr={expr} stderr={}", o.stderr);
        assert_eq!(o.code, 0, "expr={expr}");
    }
    // 未配置 → 保持原生时间(默认行为不变)
    let o = run(&dir, &wave, "(at \"top.d\" 30)", &[]);
    assert_eq!(line(&o), "=> (20 1)");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn beat_env_multi_clock_shows_each_clock_separately() {
    let dir = tmpdir("multi");
    let wave = fixture(&dir);
    // slow 的首个上升沿在 60, 所以 t=20/40/50 处 slow 拍号 = -1
    let o = run(
        &dir,
        &wave,
        "(at \"top.d\" 30)",
        &[("WAL_BEAT_CLK", "top.clk,top.slow")],
    );
    assert_eq!(
        line(&o),
        "=> ((\"top.clk\" 0) (\"top.slow\" -1) 1)",
        "stderr={}",
        o.stderr
    );
    // 每个点都是 (各时钟拍号对...) 列表, 而不是合并成一个数
    let o = run(
        &dir,
        &wave,
        "(getwave \"top.slow\")",
        &[("WAL_BEAT_CLK", "top.clk,top.slow")],
    );
    let l = line(&o);
    // getwave("top.slow"): t=0 → (clk -1, slow -1) 值0; t=60 → (clk 1, slow 0) 值1
    assert!(l.contains("(\"top.clk\" -1) (\"top.slow\" -1) 0"), "{l}");
    assert!(l.contains("(\"top.clk\" 1) (\"top.slow\" 0) 1"), "{l}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn beat_env_invalid_clock_errors_once_and_falls_back_to_absolute() {
    let dir = tmpdir("invalid");
    let wave = fixture(&dir);
    let o = run(&dir, &wave, "(at \"top.d\" 30)", &[("WAL_BEAT_CLK", "no.such.clk")]);
    assert_eq!(line(&o), "=> (20 1)", "应整体回退绝对时间; stderr={}", o.stderr);
    assert!(o.stderr.contains("WAL_BEAT_CLK"), "{}", o.stderr);
    assert!(o.stderr.contains("已整体回退"), "{}", o.stderr);
    assert_eq!(o.code, 0, "回退不应让进程失败");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn beat_env_clock_without_rising_edge_falls_back() {
    let dir = tmpdir("noedge");
    let wave = fixture(&dir);
    // const1 恒 1, 没有上升沿 → 无法定义拍号 → 报错 + 绝对时间
    let o = run(&dir, &wave, "(getwave \"top.d\")", &[("WAL_BEAT_CLK", "top.const1")]);
    assert_eq!(line(&o), "=> ((0 0) (20 1) (40 0))");
    assert!(o.stderr.contains("没有上升沿"), "{}", o.stderr);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn beat_negative_hint_printed_only_once() {
    let dir = tmpdir("hint");
    let wave = fixture(&dir);
    // 两个查询都含首沿前的时间点(-1): 提示只应出现一次
    let o = run(
        &dir,
        &wave,
        "(do (getwave \"top.d\") (at \"top.d\" 3))",
        &[("WAL_BEAT_CLK", "top.clk")],
    );
    let n = o.stderr.matches("拍号 -1").count();
    assert_eq!(n, 1, "提示应只打一次, 实际 {n} 次; stderr={}", o.stderr);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn beat_env_uses_clock_of_the_trace_that_answers() {
    let dir = tmpdir("multi_wave");
    let a = dir.join("a.vcd");
    std::fs::write(
        &a,
        "$timescale 1ns $end\n$scope module top $end\n$var wire 1 ! clk $end\n$var wire 1 \" only_a $end\n$upscope $end\n$enddefinitions $end\n#0\n0!\n0\"\n#10\n1!\n#20\n1\"\n#25\n0!\n#30\n1!\n",
    ).unwrap();
    let b = dir.join("b.vcd");
    std::fs::write(
        &b,
        "$timescale 1ns $end\n$scope module top $end\n$var wire 1 ! clk $end\n$var wire 1 \" d $end\n$upscope $end\n$enddefinitions $end\n#0\n0!\n0\"\n#100\n1!\n#120\n1\"\n#140\n0!\n",
    ).unwrap();
    // d 只在 b 里: 应使用 b 的时钟(上升沿 100) → t=120 处 beat = 0
    let mut c = Command::new(BIN);
    c.arg("(at \"top.d\" 130)")
        .arg("-l").arg(&a)
        .arg("-l").arg(&b)
        .current_dir(&dir)
        .env("WAL_BEAT_CLK", "top.clk");
    let o = c.output().unwrap();
    let stdout = String::from_utf8_lossy(&o.stdout);
    let l = stdout.lines().find(|l| l.trim_start().starts_with("=>")).unwrap().trim().to_string();
    assert_eq!(l, "=> (0 1)", "应按 b 的时钟算拍号; stdout={stdout}");
    let _ = std::fs::remove_dir_all(dir);
}
