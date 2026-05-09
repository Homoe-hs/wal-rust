//! Convert builtin operators
//!
//! convert - VCD to FST conversion

use crate::fst::{FstWriter, FstOptions, VarType};
use crate::trace::{Trace, VcdTrace, ScalarValue};
use crate::wal::ast::{Value, Operator};
use crate::wal::eval::Dispatcher;
use std::path::Path;

fn scalar_to_bytes(sv: &ScalarValue) -> Vec<u8> {
    match sv {
        ScalarValue::Bit(b) => vec![*b],
        ScalarValue::Vector(v) => {
            let mut bits = Vec::with_capacity(v.len());
            for &b in v {
                bits.push(match b {
                    b'1' => 1u8,
                    b'x' | b'X' => 2u8,
                    b'z' | b'Z' => 3u8,
                    _ => 0u8,
                });
            }
            bits
        }
        ScalarValue::Real(r) => format!("{}", r).into_bytes(),
    }
}

fn convert_vcd_to_fst(input: &str, output: &str, compression: &str) -> Result<(), String> {
    let vcd = VcdTrace::load(Path::new(input), "src".to_string())
        .map_err(|e| format!("convert: failed to load VCD '{}': {}", input, e))?;

    let comp = match compression {
        "zlib" => crate::fst::types::Compression::Zlib,
        _ => crate::fst::types::Compression::Lz4,
    };
    let opts = FstOptions { compression: comp, ..Default::default() };
    let mut fst = FstWriter::create(Path::new(output), opts)
        .map_err(|e| format!("convert: failed to create FST '{}': {}", output, e))?;

    let sig_list: Vec<String> = vcd.signals();
    let mut sig_handles = Vec::new();
    for sig_name in &sig_list {
        let width = vcd.signal_width(sig_name).unwrap_or(1) as u32;
        let handle = fst.create_var(sig_name, width, VarType::VcdWire);
        sig_handles.push((sig_name.clone(), handle));
    }

    let max_idx = vcd.max_index();
    let mut prev_values: Vec<Vec<u8>> = vec![Vec::new(); sig_handles.len()];

    for idx in 0..=max_idx {
        fst.emit_time_change(idx as u64);
        for (i, (sig_name, handle)) in sig_handles.iter().enumerate() {
            if let Ok(sv) = vcd.signal_value(sig_name, idx) {
                let val_bytes = scalar_to_bytes(&sv);
                if prev_values[i] != val_bytes {
                    fst.emit_value_change(*handle, &val_bytes);
                    prev_values[i] = val_bytes;
                }
            }
        }
    }

    fst.close()
        .map_err(|e| format!("convert: failed to finalize FST '{}': {}", output, e))?;

    Ok(())
}

pub fn register_convert(disp: &mut Dispatcher) {
    disp.register(Operator::Convert, |args, _env, _eval| {
        if args.len() < 2 {
            return Err("convert: expected at least 2 arguments (input, output)".to_string());
        }
        let input = match &args[0] {
            Value::String(s) => s.clone(),
            _ => return Err("convert: first argument must be a string (input path)".to_string()),
        };
        let output = match &args[1] {
            Value::String(s) => s.clone(),
            _ => return Err("convert: second argument must be a string (output path)".to_string()),
        };
        let compression = if args.len() > 2 {
            match &args[2] {
                Value::String(s) => s.clone(),
                _ => return Err("convert: third argument must be a string (compression)".to_string()),
            }
        } else {
            "lz4".to_string()
        };

        convert_vcd_to_fst(&input, &output, &compression)?;
        Ok(Value::String(format!("converted {} -> {} ({})", input, output, compression)))
    });
}