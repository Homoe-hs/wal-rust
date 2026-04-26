#![allow(non_camel_case_types)]

use tree_sitter::Language;

extern "C" {
    pub fn tree_sitter_wal() -> *const tree_sitter::ffi::TSLanguage;
}

pub unsafe fn language() -> Language {
    Language::from_raw(tree_sitter_wal())
}