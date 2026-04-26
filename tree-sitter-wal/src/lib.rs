mod bindings;

use tree_sitter::Language;

pub use bindings::tree_sitter_wal;

pub fn language() -> Language {
    unsafe { bindings::language() }
}