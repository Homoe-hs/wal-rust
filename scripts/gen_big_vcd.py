#!/usr/bin/env python3
"""Generate a synthetic VCD matching the real 152GB workload's structure:
   3.5M signals, ~1100 value changes per timestamp (sparse: each signal
   changes only ~7k times in total), 32-bit vectors + some 1-bit signals.
   Usage: gen_big_vcd.py <out.vcd> [signals] [timestamps] [changes_per_ts] [seed]
   Sizes: 10GB ≈ signals=3500000, timestamps=1500000, changes_per_ts=1100.
"""
import random
import sys

ID_CHARS = "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ!#$%&'()*+,-./:;<=>?@[]^_`{|}~"

def id_of(n: int) -> str:
    s = ""
    n += 1
    while n:
        n, r = divmod(n, len(ID_CHARS))
        s = ID_CHARS[r] + s
    return s or ID_CHARS[0]

def main():
    out = sys.argv[1]
    n_sig = int(sys.argv[2]) if len(sys.argv) > 2 else 3_500_000
    n_ts = int(sys.argv[3]) if len(sys.argv) > 3 else 1_500_000
    per_ts = int(sys.argv[4]) if len(sys.argv) > 4 else 1100
    seed = int(sys.argv[5]) if len(sys.argv) > 5 else 42
    rng = random.Random(seed)

    ids = [id_of(i) for i in range(n_sig)]

    with open(out, "w", buffering=1 << 20) as f:
        f.write("$timescale 1ns $end\n$scope module big $end\n")
        for i in range(n_sig):
            w = 32 if i % 10 != 0 else 1
            f.write(f"$var wire {w} {ids[i]} s{i} $end\n")
        f.write("$enddefinitions $end\n$dumpvars\n")
        ts = 0
        for t in range(n_ts):
            ts += 10
            f.write(f"#{ts}\n")
            # guarantee the first signal changes at least once per ~1000 ts
            if t % 1000 == 0:
                f.write(f"b{rng.getrandbits(32):032b} {ids[0]}\n")
            for _ in range(per_ts):
                si = rng.randrange(n_sig)
                if si % 10 != 0:
                    # 32-bit vector
                    f.write((f"b{rng.getrandbits(32):032b} {ids[si]}\n"))
                else:
                    # 1-bit
                    f.write(f"{rng.choice('01')}{ids[si]}\n")
    print(f"wrote {out}: {n_sig} signals, {n_ts} timestamps, ~{per_ts} changes/ts")

if __name__ == "__main__":
    main()
