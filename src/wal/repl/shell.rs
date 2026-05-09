//! REPL Shell implementation using rustyline

use rustyline::completion::{Completer, Pair};
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::{Validator, ValidationResult, ValidationContext};
use rustyline::{Context, Helper, Editor};
use crate::wal::eval::Evaluator;
use crate::wal::ast::Value;


pub struct Repl {
    editor: Editor<WalHelper, rustyline::history::DefaultHistory>,
    eval: Evaluator,
}

struct WalHelper {
    signals: Vec<String>,
}

impl WalHelper {
    fn new() -> Self { WalHelper { signals: Vec::new() } }
    fn refresh_signals(&mut self, eval: &Evaluator) {
        if let Some(traces) = eval.env.get_traces() {
            if let Ok(t) = traces.read() {
                self.signals = t.all_signals();
            }
        }
    }
}

impl Completer for WalHelper {
    type Candidate = Pair;
    fn complete(&self, line: &str, pos: usize, _ctx: &Context<'_>) -> rustyline::Result<(usize, Vec<Pair>)> {
        let prefix = &line[..pos];
        // Extract last word
        let start = prefix.rfind(|c: char| c == ' ' || c == '(' || c == '"').map(|i| i+1).unwrap_or(0);
        let word = &prefix[start..];
        if word.is_empty() || word.starts_with('(') {
            return Ok((pos, Vec::new()));
        }
        let matches: Vec<Pair> = self.signals.iter()
            .filter(|s| s.contains(word))
            .map(|s| Pair { display: s.clone(), replacement: s.clone() })
            .take(20)
            .collect();
        Ok((start, matches))
    }
}

impl Hinter for WalHelper {
    type Hint = String;
    fn hint(&self, _line: &str, _pos: usize, _ctx: &Context<'_>) -> Option<String> { None }
}

impl Highlighter for WalHelper {}
impl Validator for WalHelper {
    fn validate(&self, _ctx: &mut ValidationContext) -> rustyline::Result<ValidationResult> {
        Ok(ValidationResult::Valid(None))
    }
}
impl Helper for WalHelper {}

impl Repl {
    pub fn new() -> Self {
        let mut helper = WalHelper::new();
        let eval = Evaluator::new();
        helper.refresh_signals(&eval);
        let mut editor = Editor::new().expect("Failed to create editor");
        editor.set_helper(Some(helper));
        Repl { editor, eval }
    }

    pub fn run(&mut self) {
        println!("WAL REPL v0.2.0 - Type '(exit)' to quit");
        println!("Examples: (+ 1 2), (define x 42), (load \"test.vcd\")");
        println!("Tab-completion: signal names, keywords");
        println!();

        loop {
            let readline = self.editor.readline("wal> ");
            match readline {
                Ok(line) => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let _ = self.editor.add_history_entry(&line);

                    match self.eval_line(&line) {
                        Ok(Some(value)) => println!("=> {}", value),
                        Ok(None) => {}
                        Err(e) => println!("Error: {}", e),
                    }
                    // Refresh signals for completion after load
                    if let Some(h) = self.editor.helper_mut() {
                        h.refresh_signals(&self.eval);
                    }
                }
                Err(rustyline::error::ReadlineError::Eof) => {
                    println!("\nGoodbye!");
                    break;
                }
                Err(rustyline::error::ReadlineError::Interrupted) => {
                    println!("\nInterrupted. Ctrl+D to exit.");
                }
                Err(e) => {
                    println!("Error: {:?}", e);
                    break;
                }
            }
        }
    }

    fn eval_line(&mut self, line: &str) -> Result<Option<Value>, String> {
        let trimmed = line.trim();

        if trimmed == "(exit)" || trimmed == "exit" {
            return Err("exit".to_string());
        }

        match self.eval.eval(trimmed) {
            Ok(v) => {
                if matches!(v, Value::Nil) {
                    Ok(None)
                } else {
                    Ok(Some(v))
                }
            }
            Err(e) => {
                if e.starts_with("exit:") {
                    Err("exit".to_string())
                } else {
                    Err(e)
                }
            }
        }
    }
}

impl Default for Repl {
    fn default() -> Self {
        Self::new()
    }
}

pub fn run_repl() {
    let mut repl = Repl::new();
    repl.run();
}