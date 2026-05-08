//! Convert builtin operators
//!
//! convert - VCD to FST conversion

use crate::wal::ast::Operator;
use crate::wal::eval::Dispatcher;

pub fn register_convert(disp: &mut Dispatcher) {
    disp.register(Operator::Convert, |_args, _env, _eval| {
        Err("convert: not yet implemented".to_string())
    });
}