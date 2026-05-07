//! wal - WAL: Waveform Analysis Language
//!
//! High-performance command-line tool for WAL parsing, REPL, and waveform tools.

mod cli;
mod fst;
mod vcd;
pub mod wal;
pub mod trace;

use crate::cli::{Args, ExecMode};
use clap::Parser;
use std::path::{Path, PathBuf};
use std::process;

fn main() {
    let args = Args::parse();

    match args.resolve() {
        ExecMode::RunScript { path, load, code } => {
            if let Err(e) = run_wal_file(&path, &load, code.as_deref()) {
                eprintln!("error: {}", e);
                process::exit(1);
            }
        }
        ExecMode::EvalExpr { code, load } => {
            if let Err(e) = eval_wal_expr(&code, &load) {
                eprintln!("error: {}", e);
                process::exit(1);
            }
        }
        ExecMode::Repl => {
            run_repl();
        }
    }
}

fn init_eval_with_load(load: &[PathBuf]) -> Result<wal::eval::Evaluator, String> {
    let mut eval = wal::eval::Evaluator::new();
    for path in load {
        let trace_count = eval.traces.read().map_err(|e| format!("{}", e))?.trace_ids().len();
        let id = format!("t{}", trace_count);
        let path_str = path.to_string_lossy().to_string();
        eval.load_trace(&path_str, &id)?;
    }
    Ok(eval)
}

fn eval_wal_expr(code: &str, load: &[PathBuf]) -> Result<(), String> {
    let mut eval = init_eval_with_load(load)?;
    let result = eval.eval(code)?;
    println!("=> {}", result);
    Ok(())
}

fn run_wal_file(path: &Path, load: &[PathBuf], code: Option<&str>) -> Result<(), String> {
    use wal::eval::Evaluator;

    let mut eval = init_eval_with_load(load)?;

    // Execute code expression if provided (overrides file)
    if let Some(code) = code {
        let result = eval.eval(code)?;
        println!("=> {}", result);
        return Ok(());
    }

    // Execute the script file
    let source = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read file: {}", e))?;

    println!("Evaluating: {}", path.display());

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
