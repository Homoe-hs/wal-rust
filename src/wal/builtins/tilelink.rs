//! TileLink analysis builtin operators
//!
//! tl-handshakes: Count TileLink handshakes (A->B transitions)
//! tl-latency: Calculate average latency between handshakes
//! tl-bandwidth: Calculate total bandwidth

use crate::wal::ast::{Value, Operator};
use crate::wal::eval::{Environment, Dispatcher, Evaluator};

fn op_tl_handshakes(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 2)?;
    let request_sig = extract_symbol(&args[0])?;
    let grant_sig = extract_symbol(&args[1])?;

    if let Some(traces) = env.get_traces() {
        let traces = traces.read().unwrap_or_else(|e| e.into_inner());
        if let Some(trace) = traces.first_trace() {
            let mut handshake_count = 0usize;
            let mut prev_request = None::<u8>;
            let mut prev_grant = None::<u8>;
            let max_idx = trace.max_index();

            for idx in 0..=max_idx {
                let req_val = trace.signal_value(&request_sig, idx)
                    .ok()
                    .and_then(|v| match v {
                        crate::trace::ScalarValue::Bit(b) => Some(b),
                        _ => None,
                    });
                let gnt_val = trace.signal_value(&grant_sig, idx)
                    .ok()
                    .and_then(|v| match v {
                        crate::trace::ScalarValue::Bit(b) => Some(b),
                        _ => None,
                    });

                if let (Some(r), Some(g)) = (req_val, gnt_val) {
                    if let (Some(pr), Some(pg)) = (prev_request, prev_grant) {
                        if pr == 0 && r == 1 && pg == 0 && g == 0 {
                            handshake_count += 1;
                        }
                    }
                    prev_request = Some(r);
                    prev_grant = Some(g);
                }
            }

            return Ok(Value::Int(handshake_count as i64));
        }
    }
    Err("tl-handshakes: trace not loaded or signals not found".to_string())
}

fn op_tl_latency(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 3)?;
    let request_sig = extract_symbol(&args[0])?;
    let grant_sig = extract_symbol(&args[1])?;
    let _data_sig = extract_symbol_or_empty(&args[2])?;

    if let Some(traces) = env.get_traces() {
        let traces = traces.read().unwrap_or_else(|e| e.into_inner());
        if let Some(trace) = traces.first_trace() {
            let mut latencies = Vec::new();
            let mut pending_request_idx = Option::<usize>::None;
            let max_idx = trace.max_index();

            for idx in 0..=max_idx {
                let req_val = trace.signal_value(&request_sig, idx)
                    .ok()
                    .and_then(|v| match v {
                        crate::trace::ScalarValue::Bit(b) => Some(b),
                        _ => None,
                    });
                let gnt_val = trace.signal_value(&grant_sig, idx)
                    .ok()
                    .and_then(|v| match v {
                        crate::trace::ScalarValue::Bit(b) => Some(b),
                        _ => None,
                    });

                if let Some(r) = req_val {
                    if r == 1 && pending_request_idx.is_none() {
                        pending_request_idx = Some(idx);
                    }
                }

                if let (Some(start_idx), Some(g)) = (pending_request_idx, gnt_val) {
                    if g == 1 {
                        let latency = idx - start_idx;
                        latencies.push(latency as i64);
                        pending_request_idx = None;
                    }
                }
            }

            if latencies.is_empty() {
                return Ok(Value::Float(f64::NAN));
            }

            let sum: i64 = latencies.iter().sum();
            let avg = sum as f64 / latencies.len() as f64;
            return Ok(Value::Float(avg));
        }
    }
    Err("tl-latency: trace not loaded or signals not found".to_string())
}

fn op_tl_bandwidth(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 3)?;
    let channel_sig = extract_symbol(&args[0])?;
    let valid_sig = extract_symbol(&args[1])?;
    let data_width = extract_int(&args[2])? as usize;

    if let Some(traces) = env.get_traces() {
        let traces = traces.read().unwrap_or_else(|e| e.into_inner());
        if let Some(trace) = traces.first_trace() {
            let mut transfer_count = 0usize;
            let max_idx = trace.max_index();

            for idx in 0..=max_idx {
                let ch_val = trace.signal_value(&channel_sig, idx)
                    .ok()
                    .and_then(|v| match v {
                        crate::trace::ScalarValue::Vector(bits) => Some(bits),
                        _ => None,
                    });
                let vld_val = trace.signal_value(&valid_sig, idx)
                    .ok()
                    .and_then(|v| match v {
                        crate::trace::ScalarValue::Bit(b) => Some(b),
                        _ => None,
                    });

                if let (Some(_ch), Some(v)) = (ch_val, vld_val) {
                    if v == 1 {
                        transfer_count += 1;
                    }
                }
            }

            let total_bits = transfer_count * data_width;
            let total_bytes = total_bits / 8;
            return Ok(Value::Int(total_bytes as i64));
        }
    }
    Err("tl-bandwidth: trace not loaded or signals not found".to_string())
}

fn ensure_arity_atleast(args: &[Value], min: usize) -> Result<(), String> {
    if args.len() < min {
        return Err(format!("Expected at least {} arguments, got {}", min, args.len()));
    }
    Ok(())
}

fn extract_symbol(v: &Value) -> Result<String, String> {
    match v {
        Value::Symbol(s) => Ok(s.name.clone()),
        _ => Err("Expected symbol".to_string()),
    }
}

fn extract_symbol_or_empty(v: &Value) -> Result<String, String> {
    match v {
        Value::Symbol(s) => Ok(s.name.clone()),
        Value::String(s) => Ok(s.clone()),
        Value::Nil => Ok(String::new()),
        _ => Err("Expected symbol or string".to_string()),
    }
}

fn extract_int(v: &Value) -> Result<i64, String> {
    match v {
        Value::Int(i) => Ok(*i),
        _ => Err("Expected int".to_string()),
    }
}

pub fn register_tilelink(disp: &mut Dispatcher) {
    disp.register(Operator::TileLinkHandshakes, op_tl_handshakes);
    disp.register(Operator::TileLinkLatency, op_tl_latency);
    disp.register(Operator::TileLinkBandwidth, op_tl_bandwidth);
}