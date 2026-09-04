//! CLI argument parsing and logging

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "wal-rust")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(
    about = "WAL: Waveform Analysis Language CLI",
    long_about = "High-performance WAL script runner and REPL for VCD/FST waveform analysis.\n\n\
                  Auto-detection:\n  \
                  input starts with '(' → evaluated as WAL expression\n  \
                  input is an existing file → executed as WAL script\n  \
                  no input → shows help\n\n\
                  Features:\n  \
                  - Full WAL language support (82 operators, macros, @/#/~ syntax)\n  \
                  - mmap-based on-demand VCD loading (two-pass scan + sparse index + LRU cache)\n  \
                  - Supports files up to 150GB+ with <2GB memory footprint\n  \
                  - FST format read/write support\n  \
                  - Interactive REPL with rustyline",
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
    #[command(about = "Count signal==VALUE timestamps:\nwal-rust count <wave> <signal> [value]")]
    Count(CountArgs),

    /// List signal names containing a pattern
    #[command(about = "List signal names matching a substring:\nwal-rust sigs <wave> <pattern> [limit]")]
    Sigs(SigsArgs),

    /// Top signals by number of value changes
    #[command(about = "Most-active signals (by change count):\nwal-rust topsig <wave> [limit]")]
    Topsig(TopsigArgs),
}

#[derive(Parser, Debug)]
pub struct CountArgs {
    /// VCD/FST waveform path
    pub wave: PathBuf,
    /// Signal name (exact or unique substring)
    pub sig: String,
    /// Value to count (timestamps where signal == VALUE); default 1
    #[arg(default_value_t = 1)]
    pub value: i64,
}

#[derive(Parser, Debug)]
pub struct SigsArgs {
    /// VCD/FST waveform path
    pub wave: PathBuf,
    /// Substring to match against signal names
    pub pattern: String,
    /// Max names to print (default 50; 0 = all)
    #[arg(default_value_t = 50)]
    pub limit: usize,
}

#[derive(Parser, Debug)]
pub struct TopsigArgs {
    /// VCD/FST waveform path
    pub wave: PathBuf,
    /// Max top signals to show (default 10)
    #[arg(default_value_t = 10)]
    pub limit: usize,
}

#[derive(Parser, Debug)]
pub struct RunArgs {
    /// WAL script file to execute
    #[arg(help = "Path to the WAL script file (.wal)")]
    pub file: PathBuf,

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
    /// Start the interactive REPL
    Repl,
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
                Command::Run(r) => ExecMode::RunScript {
                    path: r.file,
                    load: r.load,
                    code: r.code,
                    halt_on_error: self.halt_on_error,
                },
                Command::Repl => ExecMode::Repl,
                Command::Count(c) => ExecMode::Count { wave: c.wave, sig: c.sig, value: c.value },
                Command::Sigs(s) => ExecMode::Sigs { wave: s.wave, pattern: s.pattern, limit: s.limit },
                Command::Topsig(t) => ExecMode::Topsig { wave: t.wave, limit: t.limit },
            };
        }

        // No subcommand — auto-detect
        let load = self.load;

        match self.input {
            None => ExecMode::Repl, // no input → help shown by clap
            Some(input) => {
                let trimmed = input.trim().to_string();
                if trimmed.starts_with('(') || trimmed.starts_with('\'') {
                    // Looks like a WAL expression
                    ExecMode::EvalExpr { code: trimmed, load }
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
