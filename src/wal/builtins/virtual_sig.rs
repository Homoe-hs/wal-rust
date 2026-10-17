//! Virtual signal builtin operators
//!
//! `defsig`(虚拟信号定义)由求值器的特殊形式处理, 本模块只保留注册入口。

use crate::wal::eval::Dispatcher;

pub fn register_virtual(_disp: &mut Dispatcher) {}
