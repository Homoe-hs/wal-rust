#[test]
fn debug_vcd2fst() {
    let data = std::fs::read("/tmp/fresh_counter.fst").unwrap();
    let tail = &data[..];
    
    // Simulate parse_vcd2fst_inline_hier
    let mut pos = 0usize;
    while pos + 9 < tail.len() {
        if tail[pos] == 0x52 {
            eprintln!("0x52 at tail pos {}", pos);
            // Skip to first 0xFE
            let mut hier_start = pos + 9;
            while hier_start < tail.len() && tail[hier_start] != 0xFE {
                hier_start += 1;
            }
            eprintln!("0xFE at tail pos {}", hier_start);
            eprintln!("Data passed to parse_hier_data starts at byte {} (0x{:02x})", 
                hier_start, tail[hier_start]);
            
            // Now trace parse_hier_data
            let mut dp = hier_start;
            while dp < tail.len() {
                let code = tail[dp];
                eprintln!("  dp={}: code=0x{:02x}", dp, code);
                dp += 1;
                if code == 0xFE {
                    dp += 1; // scope type
                    while dp < tail.len() && tail[dp] != 0 { dp += 1; } // scope name
                    dp += 1; // skip null
                    while dp < tail.len() && tail[dp] != 0 { dp += 1; } // scope comp
                    dp += 1; // skip null
                } else if code == 0xFF {
                    // upscope
                } else if code <= 29 {
                    let dir = tail[dp]; dp += 1;
                    eprintln!("    dir=0x{:02x}", dir);
                    let name_start = dp;
                    while dp < tail.len() && tail[dp] != 0 { dp += 1; }
                    let name = String::from_utf8_lossy(&tail[name_start..dp]);
                    dp += 1; // skip null
                    eprintln!("    name='{}' ({} bytes)", name, dp - name_start);
                    // width varint
                    let mut w = 0u64; let mut sh = 0;
                    while dp < tail.len() {
                        let b = tail[dp]; dp += 1;
                        w |= ((b & 0x7f) as u64) << sh; sh += 7;
                        if b & 0x80 == 0 { break; }
                    }
                    // alias varint
                    let mut a = 0u64; let mut sh = 0;
                    while dp < tail.len() {
                        let b = tail[dp]; dp += 1;
                        a |= ((b & 0x7f) as u64) << sh; sh += 7;
                        if b & 0x80 == 0 { break; }
                    }
                    eprintln!("    width={}, alias={}", w, a);
                } else if code == 0xFC {
                    eprintln!("    FC attr start");
                    dp += 2; // type+subtype
                    while dp < tail.len() && tail[dp] != 0 { dp += 1; }
                    dp += 1;
                    while dp < tail.len() && tail[dp] != 0 { dp += 1; }
                    dp += 1;
                } else if code == 0xFD {
                    eprintln!("    FD attr end");
                } else {
                    eprintln!("    UNKNOWN, stopping");
                    break;
                }
                if dp > 700 { break; }
            }
            break;
        }
        pos += 1;
    }
}
TESTEOF
cargo test --test debug_vcd2fst_test -- --nocapture 2>&1 | grep "dp=\|0x52\|0xFE\|name="
rm -f tests/debug_vcd2fst_test.rs