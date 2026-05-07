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
    after_help = "EXAMPLES:\n  \
                  wal-rust repl\n  \
                  wal-rust '(+ 1 2)'\n  \
                  wal-rust '(step 100)'\n  \
                  wal-rust script.wal\n  \
                  wal-rust run -l trace.vcd script.wal\n  \
                  wal-rust '(+ 1 2)' -l dump.vcd\n\n\
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
    },
    /// Evaluate a WAL expression directly
    EvalExpr {
        code: String,
        load: Vec<PathBuf>,
    },
    /// Start the interactive REPL
    Repl,
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
                },
                Command::Repl => ExecMode::Repl,
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
                    }
                }
            }
        }
    }
}
