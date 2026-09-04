//! wal - WAL: Waveform Analysis Language
//!
//! High-performance command-line tool for WAL parsing, REPL, and waveform tools.

use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

mod cli;
mod fst;
mod vcd;
pub mod wal;
pub mod trace;

use crate::cli::{Args, ExecMode};
use crate::trace::Trace;
use clap::Parser;
use std::path::{Path, PathBuf};
use std::process;

fn main() {
    // ---version: print build version plus install path (helps reconcile
    // package-manager module versions with the binary's own version).
    let argv: Vec<String> = std::env::args().collect();
    if argv.len() <= 2 && argv.iter().any(|a| a == "--version" || a == "-V") {
        let exe = std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "?".to_string());
        println!("wal-rust {} (from {})", env!("CARGO_PKG_VERSION"), exe);
        return;
    }

    let args = Args::parse();

    match args.resolve() {
        ExecMode::RunScript { path, load, code, halt_on_error } => {
            if let Err(e) = run_wal_file(&path, &load, code.as_deref(), halt_on_error) {
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
        ExecMode::Count { wave, sig, value } => {
            if let Err(e) = cmd_count(&wave, &sig, value) {
                eprintln!("error: {}", e);
                process::exit(1);
            }
        }
        ExecMode::Sigs { wave, pattern, limit } => {
            if let Err(e) = cmd_sigs(&wave, &pattern, limit) {
                eprintln!("error: {}", e);
                process::exit(1);
            }
        }
        ExecMode::Topsig { wave, limit } => {
            if let Err(e) = cmd_topsig(&wave, limit) {
                eprintln!("error: {}", e);
                process::exit(1);
            }
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
    let val = eval.eval(code)?;
    println!("=> {}", val);
    Ok(())
}

fn run_wal_file(path: &Path, load: &[PathBuf], code: Option<&str>, halt_on_error: bool) -> Result<(), String> {
    let mut eval = init_eval_with_load(load)?;

    // Report a script error (default: continue; --halt-on-error stops).
    let report = |line: usize, e: String| -> Result<(), String> {
        if !e.starts_with("exit:") {
            if halt_on_error {
                // stop: main() prints the final "error: ..." line
                return Err(format!("Error on line {}: {}", line, e));
            }
            eprintln!("Error on line {}: {}", line, e);
        }
        Ok(())
    };

    // Execute code expression if provided (overrides file)
    if let Some(code) = code {
        let result = eval.eval(code)?;
        println!("=> {}", result);
        return Ok(());
    }

    // Execute the script file
    let source = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read file: {}", e))?;

    // Handle multi-line expressions by accumulating them across lines.
    // Comments (; to end of line, outside strings) are stripped per line so
    // they never leak into the accumulated expression; lines are joined with
    // '\n' so a trailing ';' keeps its line-comment semantics.
    let mut expr = String::new();
    let mut paren_depth = 0;
    let mut line_number = 0;
    let mut in_string = false;

    for line in source.lines() {
        line_number += 1;

        // Strip trailing comment (outside strings)
        let mut in_str = false;
        let mut effective = String::with_capacity(line.len());
        for ch in line.chars() {
            if ch == '"' { in_str = !in_str; }
            if ch == ';' && !in_str { break; }
            effective.push(ch);
        }

        let trimmed = effective.trim();
        if trimmed.is_empty() || trimmed.starts_with(';') {
            continue;
        }

        for ch in effective.chars() {
            expr.push(ch);
            if ch == '"' { in_string = !in_string; }
            if !in_string {
                match ch {
                    '(' | '[' | '{' => paren_depth += 1,
                    ')' | ']' | '}' => paren_depth -= 1,
                    _ => {}
                }
            }
            // Evaluate complete expressions as soon as paren_depth reaches 0.
            // Only bracketed expressions are evaluated mid-line; bare atoms
            // (numbers/symbols like 0x1F) are evaluated at end of line so
            // they are not chopped up character by character.
            if paren_depth == 0 && !in_string && !expr.trim().is_empty() {
                let trimmed = expr.trim().to_string();
                if !trimmed.is_empty() && !trimmed.starts_with(';') && trimmed.starts_with('(') {
                    match eval.eval(&trimmed) {
                        Ok(v) => {
                            if !matches!(v, wal::ast::Value::Nil) {
                                println!("{}", v);
                            }
                        }
                        Err(e) => report(line_number, e)?,
                    }
                    expr.clear();
                }
            }
        }

        // A bare (unbracketed) expression on its own line is complete at
        // end of line: evaluate it now instead of merging with next lines.
        if paren_depth == 0 && !in_string && !expr.trim().is_empty()
            && !expr.trim_start().starts_with('(')
        {
            let bare = expr.trim().to_string();
            if !bare.is_empty() && !bare.starts_with(';') {
                match eval.eval(&bare) {
                    Ok(v) => {
                        if !matches!(v, wal::ast::Value::Nil) {
                            println!("{}", v);
                        }
                    }
                    Err(e) => report(line_number, e)?,
                }
            }
            expr.clear();
        }

        // Add a newline between lines (keeps ';' line-comment semantics)
        if !in_string && paren_depth != 0 {
            expr.push('\n');
        }
    }

    // Evaluate any remaining expression at EOF
    if !expr.trim().is_empty() {
        if let Err(e) = eval.eval(expr.trim()) {
            report(line_number, e)?;
        }
    }

    Ok(())
}

fn run_repl() {
    wal::repl::run_repl();
}

// ---------------------------------------------------------------------------
// One-shot query subcommands: count / sigs / topsig
// ---------------------------------------------------------------------------

fn load_one_wave(wave: &Path) -> Result<trace::TraceContainer, String> {
    let mut tc = trace::TraceContainer::new();
    tc.load(wave, "t".into()).map_err(|e| format!("{}: {}", wave.display(), e))?;
    Ok(tc)
}

/// resolve a signal name (exact / unique substring) and include nearest
/// name candidates in the error when not found.
fn resolve_sig_or_err<'a>(sigs: &'a [String], name: &str) -> Result<&'a str, String> {
    if let Some(s) = wal::eval::resolve_signal_name(name, sigs) {
        return Ok(sigs.iter().find(|x| **x == s).unwrap().as_str());
    }
    let cands: Vec<String> = sigs.iter().filter(|s| s.contains(name)).take(8).cloned().collect();
    let hint = if cands.is_empty() {
        String::new()
    } else {
        format!(" (names containing it: {:?})", cands)
    };
    Err(format!("signal '{}' not found in wave{}", name, hint))
}

fn cmd_count(wave: &Path, sig: &str, value: i64) -> Result<(), String> {
    let tc = load_one_wave(wave)?;
    let tr = tc.get(&"t".to_string()).ok_or("no trace loaded")?;
    let sigs = tr.signals();
    let resolved = resolve_sig_or_err(&sigs, sig)?;
    let n = tr.find_indices(resolved, trace::FindCondition::ValueI64(value))?.len();
    println!("{}", n);
    Ok(())
}

fn cmd_sigs(wave: &Path, pattern: &str, limit: usize) -> Result<(), String> {
    let tc = load_one_wave(wave)?;
    let tr = tc.get(&"t".to_string()).ok_or("no trace loaded")?;
    let all = tr.signals();
    let matched: Vec<&String> = all.iter().filter(|s| s.contains(pattern)).collect();
    println!("{} signal(s) containing '{}':", matched.len(), pattern);
    let shown = if limit == 0 { matched.len() } else { limit.min(matched.len()) };
    for m in matched.iter().take(shown) {
        println!("{}", m);
    }
    if shown < matched.len() {
        println!("... ({} more, use a larger limit)", matched.len() - shown);
    }
    Ok(())
}

fn cmd_topsig(wave: &Path, limit: usize) -> Result<(), String> {
    let tc = load_one_wave(wave)?;
    let tr = tc.get(&"t".to_string()).ok_or("no trace loaded")?;
    // VCD overrides this with a single-pass counter (fast for 90k signals).
    for (name, c) in tr.signal_change_counts_top(limit) {
        println!("{:<7} {}", c, name);
    }
    Ok(())
}
