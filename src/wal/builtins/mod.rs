//! Builtins module for WAL
//!
//! All 76 operators implementation.

pub mod core;
pub mod math;
pub mod list;
pub mod signal;
pub mod convert;
pub mod types;
pub mod bitwise;
pub mod array;
pub mod scope;
pub mod virtual_sig;
pub mod special;
pub mod tilelink;

pub use core::register_core;
pub use math::register_math;
pub use list::register_list;
pub use signal::register_signal;
pub use convert::register_convert;
pub use types::register_types;
pub use bitwise::register_bitwise;
pub use array::register_array;
pub use scope::register_scope;
pub use virtual_sig::register_virtual;
pub use special::register_special;
pub use tilelink::register_tilelink;

use super::eval::Dispatcher;

pub fn register_all(disp: &mut Dispatcher) {
    register_core(disp);
    register_math(disp);
    register_list(disp);
    register_signal(disp);
    register_convert(disp);
    register_types(disp);
    register_bitwise(disp);
    register_array(disp);
    register_scope(disp);
    register_virtual(disp);
    register_special(disp);
    register_tilelink(disp);
}