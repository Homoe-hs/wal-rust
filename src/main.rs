//! wal - WAL: Waveform Analysis Language
//!
//! High-performance command-line tool for WAL parsing, REPL, and waveform tools.

mod cli;
mod fst;
mod vcd;
pub mod wal;
pub mod trace;

use crate::cli::{Args, Command};
use clap::Parser;
use std::process;

fn main() {
    let args = Args::parse();

    match args.command {
        Command::Run(run_args) => {
            if let Err(e) = run_wal_file(&run_args) {
                eprintln!("error: {}", e);
                process::exit(1);
            }
        }
        Command::Repl => {
            run_repl();
        }
    }
}

fn run_wal_file(args: &crate::cli::RunArgs) -> Result<(), String> {
    use wal::eval::Evaluator;
    
    use std::fs;

    let source = fs::read_to_string(&args.file)
        .map_err(|e| format!("Failed to read file: {}", e))?;

    let mut eval = Evaluator::new();

    // Pre-load waveforms if specified
    for path in &args.load {
        let trace_count = eval.traces.read().unwrap().trace_ids().len();
        let id = format!("t{}", trace_count);
        let path_str = path.to_string_lossy().to_string();
        eval.load_trace(&path_str, &id)?;
    }

    // Execute code expression if provided
    if let Some(ref code) = args.code {
        let result = eval.eval(code)?;
        println!("=> {}", result);
        return Ok(());
    }

    // Otherwise, execute the file
    println!("Evaluating: {}", args.file.display());

    // Handle multi-line expressions by accumulating them across lines
    let mut expr = String::new();
    let mut paren_depth = 0;
    let mut line_number = 0;

    for line in source.lines() {
        line_number += 1;
        let trimmed = line.trim();

        if trimmed.is_empty() || trimmed.starts_with(";;") {
            continue;
        }

        for ch in line.chars() {
            expr.push(ch);
            match ch {
                '(' | '[' | '{' => paren_depth += 1,
                ')' | ']' | '}' => paren_depth -= 1,
                _ => {}
            }
        }

        // Add space between lines for proper tokenization
        expr.push(' ');

        if paren_depth == 0 && !expr.trim().is_empty() {
            match eval.eval(expr.trim()) {
                Ok(v) => {
                    if !matches!(v, wal::ast::Value::Nil) {
                        println!("=> {}", v);
                    }
                }
                Err(e) => {
                    if !e.starts_with("exit:") {
                        eprintln!("Error on line {}: {}", line_number, e);
                    }
                }
            }
            expr.clear();
        }
    }

    Ok(())
}

fn run_repl() {
    wal::repl::run_repl();
}