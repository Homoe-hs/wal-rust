//! REPL Shell implementation using rustyline

use rustyline::Editor;
use crate::wal::eval::Evaluator;
use crate::wal::ast::Value;

pub struct Repl {
    editor: Editor<(), rustyline::history::DefaultHistory>,
    eval: Evaluator,
}

impl Repl {
    pub fn new() -> Self {
        Repl {
            editor: Editor::new().expect("Failed to create editor"),
            eval: Evaluator::new(),
        }
    }

    pub fn run(&mut self) {
        println!("WAL REPL v0.2.0 - Type '(exit)' to quit");
        println!("Examples: (+ 1 2), (define x 42), (load \"test.vcd\")");
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