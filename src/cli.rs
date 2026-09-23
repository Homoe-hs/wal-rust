//! CLI argument parsing and logging

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "wal-rust")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(
    about = "WAL: Waveform Analysis Language CLI",
    long_about = "High-performance WAL script runner and REPL for VCD/FST/FSDB waveform analysis.\n\n\
                  Auto-detection:\n  \
                  input starts with '(' → evaluated as WAL expression\n  \
                  input is an existing file → executed as WAL script\n  \
                  no input → shows help\n\n\
                  Features:\n  \
                  - WAL language: 146 named operators, macros, @/#/~ syntax, scripts + REPL\n  \
                  - mmap-based on-demand VCD loading (two-pass scan + sparse index + LRU cache)\n  \
                  - Handles 150GB+ dumps; process HEAP is O(signals + queried columns)\n  \
                    (RSS additionally counts mmap'd file pages — see docs/waveform-io-plan.md)\n  \
                  - FST read support (wellen); FST **write** is not supported (dump-trace only\n  \
                    writes VCD — use an external converter for FST export)\n  \
                  - FSDB read via Verdi's NPI (runtime-discovered libNPI.so, pure Rust FFI,\n  \
                    no C++ shim): set $VERDI_HOME or $WAL_NPI_LIB; needs a Verdi license\n  \
                  - Interactive REPL with rustyline, plus --stdin session mode\n  \
                    (one load, many probes: wal-rust --stdin -l big.vcd < probes.txt)",
    after_help = "QUICK START (waveform analysis):\n  \
                  wal-rust -l trace.vcd '(SIGNALS)'                          # list signals\n  \
                  wal-rust -l trace.vcd '(count (= (get \"clk\") 1))'          # count high cycles\n  \
                  wal-rust -l trace.vcd '(find (= (get \"clk\") 1))'           # find indices\n  \
                  wal-rust -l trace.vcd '(find (rising \"clk\"))'             # rising edges\n  \
                  wal-rust -l trace.vcd '(count (is-x \"sig\"))'              # unknown bits\n  \
                  wal-rust -l trace.vcd '(whenever (rising \"clk\") (printf \"%d\\n\" INDEX))'\n  \
                  wal-rust repl                                            # interactive\n\n\
                  QUICK START (scripts):\n  \
                  (load \"trace.vcd\")\n  \
                  (count (&& (= (get \"awvalid\") 1) (= (get \"awready\") 1)))  # handshakes\n  \
                  (whenever (&& (= (get \"req\") 1) (= (get \"gnt\") 1))\n  \
                    (printf \"grant at %0d\\n\" INDEX))\n\n\
                  QUICK REFERENCE:\n  \
                  count / find / whenever — condition queries (fast paths for (= sig N))\n  \
                  rising / falling / changes — edge detection\n  \
                  is-x / is-z — unknown / high-impedance detection\n  \
                  get — signal value at current INDEX; sample-at — at given index\n  \
                  SIGNALS / INDEX / TS / MAX-INDEX — special variables\n  \
                  step — advance trace index; + - * / if do define set! — language\n\n\
                  x/z semantics (authoritative): docs/4-state-semantics.md —\n  \
                  x is NOT 0; x→1 is a change but NOT a rising edge; (get s) reads at CURRENT INDEX.\n\n\
                  See https://wal-lang.org for WAL language documentation."
)]
#[command(subcommand_required = false)]
#[command(args_conflicts_with_subcommands = true)]
pub struct Args {
    /// WAL expression (starts with '(') or script file path.
    /// Auto-detected: expression → evaluate, file → execute.
    #[arg(help = "WAL expression or script file to execute")]
    pub input: Option<String>,

    /// Pre-load waveform file(s) before execution
    #[arg(
        short = 'l',
        long = "load",
        help = "VCD or FST waveform file(s) to load before running.\nCan be specified multiple times."
    )]
    pub load: Vec<PathBuf>,

    /// Stop at the first script error instead of continuing (CI-friendly)
    #[arg(long = "halt-on-error", global = true, help = "Stop at the first script error instead of continuing")]
    pub halt_on_error: bool,

    /// 会话模式: 从 stdin 逐行读 WAL 表达式, 单进程内复用加载/缓存
    /// (大波形上每个探针不再重新加载 + 全扫)
    #[arg(
        long = "stdin",
        help = "Read WAL expressions line by line from stdin and evaluate them in ONE process.\n\
Waveforms given with -l are loaded once; every subsequent probe reuses the in-memory\n\
index and per-signal column cache, so probing a 17GB dump interactively is fast.\n\
Example: wal-rust --stdin -l big.vcd < probes.txt"
    )]
    pub stdin: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Parser, Debug)]
pub enum Command {
    /// Run a WAL script file (default when file provided)
    #[command(
        about = "Execute a WAL script file",
        long_about = "Parse and evaluate a WAL script file.\n\
                      Supports multi-line expressions, waveform loading,\n\
                      and inline code execution."
    )]
    Run(RunArgs),

    /// Start an interactive WAL REPL
    #[command(
        about = "Start interactive REPL",
        long_about = "Launch an interactive Read-Eval-Print Loop for WAL.\n\
                      Features line editing, history, and tab completion."
    )]
    Repl,

    /// Count timestamps where a signal equals a value (default 1)
    #[command(about = "Count timestamps where signal == VALUE (default 1).\n\
NOTE: this is NOT the change count — for that use the expression\n\
  wal-rust '(count (changes \"sig\"))' -l <wave>\n\
wal-rust count <wave> <signal> [value]")]
    Count(CountArgs),

    /// List signal names containing a pattern
    #[command(about = "List signal names matching a substring:\nwal-rust sigs <wave> <pattern> [limit]")]
    Sigs(SigsArgs),

    /// Top signals by number of value changes
    #[command(about = "Most-active signals (by change count):\nwal-rust topsig <wave> [limit]")]
    Topsig(TopsigArgs),

    /// FSDB 全局时间线: 集群/多进程分片预计算(第 shard 片)
    #[command(
        about = "Precompute a shard of the FSDB global timeline (index space).\n\
Use it to fan the one unavoidable full-file pass out over many cores / LSF nodes,\n\
then merge the shards with `fsdb-timeline-merge`:\n\
  wal-rust fsdb-timeline-map design.fsdb 0 8 tl.0.part\n\
  wal-rust fsdb-timeline-merge design.fsdb tl.*.part\n\
Shards are round-robin over the signal list (hot/cold mixed evenly).\n\
Each worker is a separate NPI session and takes one Verdi license."
    )]
    FsdbTimelineMap(FsdbTimelineMapArgs),

    /// FSDB 全局时间线: 归并分片 → 安装 .ftl 缓存
    #[command(
        about = "Merge shards from `fsdb-timeline-map` and install the .ftl timeline cache\n\
(byte-identical to the single-process cache):\n\
  wal-rust fsdb-timeline-merge design.fsdb tl.*.part\n\
Cache goes to $WAL_CACHE_DIR (default: ./.wal-rust-cache); run it from the directory\n\
you query from, or point WAL_CACHE_DIR at a shared one."
    )]
    FsdbTimelineMerge(FsdbTimelineMergeArgs),
}

/// 时间线分片预计算(集群/多进程)。
#[derive(clap::Args, Debug)]
pub struct FsdbTimelineMapArgs {
    /// 波形文件(FSDB)
    pub file: PathBuf,
    /// 本片编号(0 ≤ shard < shards)
    pub shard: usize,
    /// 分片总数(通常 = 并发 worker 数)
    pub shards: usize,
    /// 输出分片文件(该片内所有变更时间, 升序去重, delta-varint)
    pub out: PathBuf,
}

/// 归并分片并安装 `.ftl` 缓存。
#[derive(clap::Args, Debug)]
pub struct FsdbTimelineMergeArgs {
    /// 波形文件(必须与 map 阶段同一个文件)
    pub file: PathBuf,
    /// 分片文件(可多个)
    #[arg(required = true)]
    pub parts: Vec<PathBuf>,
    /// 缓存根目录(默认: $WAL_CACHE_DIR 或 ./.wal-rust-cache)
    #[arg(long)]
    pub cache_dir: Option<PathBuf>,
}

#[derive(Parser, Debug)]
pub struct CountArgs {
    /// VCD/FST/FSDB waveform path
    pub wave: PathBuf,
    /// Signal name (exact or unique substring)
    pub sig: String,
    /// Value to count (timestamps where signal == VALUE); default 1
    #[arg(default_value_t = 1)]
    pub value: i64,
}

#[derive(Parser, Debug)]
pub struct SigsArgs {
    /// VCD/FST/FSDB waveform path
    pub wave: PathBuf,
    /// Substring to match against signal names
    pub pattern: String,
    /// Max names to print (default 50; 0 = all)
    #[arg(default_value_t = 50)]
    pub limit: usize,
}

#[derive(Parser, Debug)]
pub struct TopsigArgs {
    /// VCD/FST/FSDB waveform path
    pub wave: PathBuf,
    /// Max top signals to show (default 10)
    #[arg(default_value_t = 10)]
    pub limit: usize,
}

#[derive(Parser, Debug)]
pub struct RunArgs {
    /// WAL script file to execute (省略时用 --code)
    #[arg(help = "Path to the WAL script file (.wal).\n可省略 —— 配合 --code 直接求值一段 WAL;两者都给时只跑 --code。")]
    pub file: Option<PathBuf>,

    /// Pre-load waveform file(s) before script execution
    #[arg(
        short = 'l',
        long = "load",
        help = "VCD or FST waveform file to load before running the script.\nCan be specified multiple times for multiple traces."
    )]
    pub load: Vec<PathBuf>,

    /// Execute a single WAL expression (overrides file execution)
    #[arg(
        short = 'c',
        long = "code",
        help = "WAL expression to evaluate directly.\nWhen specified, the script file is not executed."
    )]
    pub code: Option<String>,
}

/// Represents the resolved execution mode after auto-detection
pub enum ExecMode {
    /// Run a script file (with optional pre-load waveforms)
    RunScript {
        path: PathBuf,
        load: Vec<PathBuf>,
        code: Option<String>,
        halt_on_error: bool,
    },
    /// Evaluate a WAL expression directly
    EvalExpr {
        code: String,
        load: Vec<PathBuf>,
    },
    /// 会话模式: 逐行读 stdin 的表达式, 单进程复用加载与缓存
    StdinSession {
        load: Vec<PathBuf>,
    },
    /// Start the interactive REPL
    Repl,
    /// FSDB 时间线分片预计算
    FsdbTimelineMap {
        file: PathBuf,
        shard: usize,
        shards: usize,
        out: PathBuf,
    },
    /// FSDB 时间线分片归并 → 安装 .ftl
    FsdbTimelineMerge {
        file: PathBuf,
        parts: Vec<PathBuf>,
        cache_dir: Option<PathBuf>,
    },
    /// count <wave> <sig> [value]
    Count {
        wave: PathBuf,
        sig: String,
        value: i64,
    },
    /// sigs <wave> <pattern> [limit]
    Sigs {
        wave: PathBuf,
        pattern: String,
        limit: usize,
    },
    /// topsig <wave> [limit]
    Topsig {
        wave: PathBuf,
        limit: usize,
    },
}

impl Args {
    pub fn resolve(self) -> ExecMode {
        // If a subcommand was given explicitly, use it
        if let Some(cmd) = self.command {
            return match cmd {
                Command::Run(r) => match (r.file, r.code) {
                    // `-c <code>`: 只求值代码, 不需要 FILE
                    (_, Some(code)) => ExecMode::EvalExpr { code, load: r.load },
                    (Some(path), None) => ExecMode::RunScript {
                        path,
                        load: r.load,
                        code: None,
                        halt_on_error: self.halt_on_error,
                    },
                    (None, None) => {
                        eprintln!("error: `run` 需要 <FILE>, 或者用 `-c/--code '<表达式>'`");
                        std::process::exit(2);
                    }
                },
                Command::Repl => ExecMode::Repl,
                Command::Count(c) => ExecMode::Count { wave: c.wave, sig: c.sig, value: c.value },
                Command::Sigs(s) => ExecMode::Sigs { wave: s.wave, pattern: s.pattern, limit: s.limit },
                Command::Topsig(t) => ExecMode::Topsig { wave: t.wave, limit: t.limit },
                Command::FsdbTimelineMap(m) => ExecMode::FsdbTimelineMap {
                    file: m.file,
                    shard: m.shard,
                    shards: m.shards,
                    out: m.out,
                },
                Command::FsdbTimelineMerge(m) => ExecMode::FsdbTimelineMerge {
                    file: m.file,
                    parts: m.parts,
                    cache_dir: m.cache_dir,
                },
            };
        }

        // No subcommand — auto-detect
        let load = self.load;
        if self.stdin {
            return ExecMode::StdinSession { load };
        }

        match self.input {
            None => ExecMode::Repl, // no input → help shown by clap
            Some(input) => {
                let trimmed = input.trim().to_string();
                if trimmed.starts_with('(') || trimmed.starts_with('\'') {
                    // Looks like a WAL expression
                    ExecMode::EvalExpr { code: trimmed, load }
                } else if !PathBuf::from(&trimmed).exists()
                    && !trimmed.starts_with('(')
                    && !trimmed.starts_with(';')
                    && !trimmed.contains('\n')
                {
                    // Not an expression and not an existing file: the most
                    // common misuse is passing a waveform (or a stray argument)
                    // where a script path is expected. Say so explicitly
                    // (feedback round: "No such file" was mistaken for a
                    // broken -l).
                    eprintln!(
                        "error: '{}' is neither a WAL expression (must start with '(') nor an existing script file.",
                        trimmed
                    );
                    eprintln!("hint:  expression:  wal-rust '(count (rising \"clk\"))' -l wave.vcd");
                    eprintln!("       script:      wal-rust run script.wal -l wave.vcd");
                    eprintln!("       waveform:    pass it with -l (not as the input argument)");
                    ExecMode::RunScript {
                        path: PathBuf::from(&trimmed),
                        load,
                        code: None,
                        halt_on_error: self.halt_on_error,
                    }
                } else {
                    // Treat as file path
                    ExecMode::RunScript {
                        path: PathBuf::from(&trimmed),
                        load,
                        code: None,
                        halt_on_error: self.halt_on_error,
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// 帮助文案里的"146 named operators"必须与**实际注册表**一致。
    /// 这条数字曾经长期停在 125(README 写 146), 而 check_docs.py 不扫 help 文本,
    /// 所以在这里守一道。
    #[test]
    fn help_operator_count_matches_registry() {
        let cmd = Args::command();
        let text = cmd
            .get_long_about()
            .map(|s| s.to_string())
            .or_else(|| cmd.get_about().map(|s| s.to_string()))
            .unwrap_or_default();
        let n = crate::wal::builtins::registered_operator_count();
        assert!(
            text.contains(&format!("{} named operators", n)),
            "帮助文案里的算子数应写成 `{} named operators`(当前文案: {:?})",
            n,
            text.lines().find(|l| l.contains("named operators")).unwrap_or("<未找到>")
        );
    }
}
