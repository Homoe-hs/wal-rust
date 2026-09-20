use std::fs::File;
use std::io::{BufWriter, Write};
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: gen_vcd <size_mb> <output_file>");
        std::process::exit(1);
    }

    let target_mb: usize = args[1].parse().unwrap();
    let output = &args[2];

    let start = Instant::now();
    generate_vcd(target_mb, output);
    eprintln!("Generated in {:.1}s", start.elapsed().as_secs_f64());
}

fn generate_vcd(target_mb: usize, filename: &str) {
    let target_bytes = target_mb * 1024 * 1024;
    let num_signals = 100;
    let bytes_per_change = 5; // "0s1\n"
    let num_changes = target_bytes / bytes_per_change;
    let changes_per_timestamp = num_signals;
    let num_timestamps = num_changes / changes_per_timestamp;

    eprintln!("Generating VCD: {}MB, timestamps: {}", target_mb, num_timestamps);

    let file = File::create(filename).unwrap();
    let mut w = BufWriter::with_capacity(8 * 1024 * 1024, file);

    // Header
    writeln!(w, "$timescale 1ns $end").unwrap();
    writeln!(w, "$scope module test $end").unwrap();
    for i in 1..=num_signals {
        writeln!(w, "$var wire 1 s{} sig{} $end", i, i).unwrap();
    }
    writeln!(w, "$upscope $end").unwrap();
    writeln!(w, "$enddefinitions $end").unwrap();

    // Dumpvars
    writeln!(w, "$dumpvars").unwrap();
    for i in 1..=num_signals {
        writeln!(w, "0s{}", i).unwrap();
    }
    writeln!(w, "$end").unwrap();

    // Value changes
    let timestep = 1000u64;
    let chunk_size = 10000; // timestamps per chunk

    let mut buf = String::with_capacity(1024 * 1024);

    for t_start in (1..=num_timestamps).step_by(chunk_size) {
        buf.clear();
        let t_end = (t_start + chunk_size).min(num_timestamps + 1);

        for t in t_start..t_end {
            buf.push('#');
            buf.push_str(&(t as u64 * timestep).to_string());
            buf.push('\n');

            for i in 1..=num_signals {
                buf.push(if (t + i) % 2 == 0 { '0' } else { '1' });
                buf.push('s');
                buf.push_str(&i.to_string());
                buf.push('\n');
            }
        }

        w.write_all(buf.as_bytes()).unwrap();
    }

    writeln!(w, "#END").unwrap();
    w.flush().unwrap();
}