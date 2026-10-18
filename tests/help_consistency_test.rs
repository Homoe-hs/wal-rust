//! help/(doc) 输出与当前算子集的一致性闸:
//! 0.14.63 清理后, 帮助里不得再出现被删/被更名算子的"现行用法", 且被删算子必须
//! **明确说明不再支持**(而不是含糊的 No help)。

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_wal-rust");

fn run(args: &[&str]) -> String {
    let o = Command::new(BIN).args(args).output().unwrap();
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

const REMOVED: &[&str] = &[
    "save", "dump-trace", "assert-eq", "count/step", "find/step", "fold/signal",
    "trim-trace", "new-trace", "timescale", "parse", "require", "import", "call",
    "repl", "set", "fmt-time", "rel_eval",
];

#[test]
fn help_overview_and_cli_help_have_no_removed_operator_usage() {
    let help = run(&["(help)"]);
    let cli = run(&["--help"]);
    // 只对"不可能作为普通英文单词出现"的旧算子名做 CLI 文案断言;
    // parse/call/set/repl 等是普通词, 只检查 (help) 总览。
    let strict = [
        "save", "dump-trace", "assert-eq", "count/step", "find/step", "fold/signal",
        "trim-trace", "new-trace", "fmt-time", "rel_eval",
    ];
    for name in strict {
        assert!(!cli.contains(name), "--help 里仍有旧算子 {name}:\n{cli}");
    }
    for name in strict {
        assert!(!help.contains(name), "(help) 里仍有旧算子 {name}:\n{help}");
    }
    assert!(cli.contains("140 named operators"), "算子计数应同步为 140:\n{cli}");
    assert!(help.contains("WAL_BEAT_CLK"), "(help) 应说明拍号模式");
}

#[test]
fn asking_help_for_removed_operator_says_not_supported_with_alternative() {
    for name in REMOVED {
        let out = run(&[&format!("(help \"{name}\")")]);
        assert!(
            out.contains("已移除") || out.contains("更名为"),
            "(help \"{name}\") 必须明确说不支持/已更名, 实际: {out}"
        );
        assert!(
            !out.contains(&format!("No help for '{name}'")),
            "(help \"{name}\") 不应再给含糊的 No help: {out}"
        );
    }
    // 被删算子的 (doc ...) 同样给明确提示
    for name in ["save", "dump-trace", "fmt-time", "rel_eval"] {
        let out = run(&[&format!("(doc \"{name}\")")]);
        assert!(out.contains("已移除") || out.contains("更名为"), "(doc \"{name}\"): {out}");
    }
    // 现存算子仍可查: beat/reval/set!/eval-file
    for (topic, needle) in [("beat", "拍号"), ("reval", "reval"), ("set!", "赋值"), ("eval-file", "eval-file")] {
        let out = run(&[&format!("(help \"{topic}\")")]);
        assert!(out.contains(needle) && !out.contains("No help"), "(help \"{topic}\"): {out}");
    }
}
