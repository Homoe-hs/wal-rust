//! CLI argument parsing and logging

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "wal-rust")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(
    about = "WAL: Waveform Analysis Language CLI",
    long_about = "High-performance WAL script runner and REPL for VCD/FST waveform analysis.\n\n\
                  Features:\n  \
                  - Full WAL language support (82 operators, macros, @/#/~ syntax)\n  \
                  - mmap-based on-demand VCD loading (two-pass scan + sparse index + LRU cache)\n  \
                  - Supports files up to 150GB+ with <2GB memory footprint\n  \
                  - FST format read/write support\n  \
                  - Interactive REPL with rustyline",
    after_help = "EXAMPLES:\n  \
                  wal-rust repl\n  \
                  wal-rust run script.wal\n  \
                  wal-rust run -l trace.vcd script.wal\n  \
                  wal-rust run -c '(+ 1 2)' script.wal\n  \
                  wal-rust run -c '(signals)' -l dump.vcd script.wal\n\n\
                  See https://wal-lang.org for WAL language documentation."
)]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Parser, Debug)]
pub enum Command {
    /// Run a WAL script file (.wal)
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

impl Args {
    #[allow(dead_code)]
    pub fn log_level(&self) -> LogLevel {
        match &self.command {
            Command::Run(_) | Command::Repl => LogLevel::Normal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum LogLevel {
    Quiet,
    Normal,
    Verbose,
}
