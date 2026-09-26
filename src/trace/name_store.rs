//! 波形后端的**名字存储**: 连续 arena + 开放寻址索引。
//!
//! 为什么单独一层: 百万~千万信号级波形上, "每个信号一个 `String`" 本身就吃掉
//! GB 级内存 —— 4M 信号实测: `Vec<String>` 表头 24B + 堆 ≥40B + `HashMap<String,_>`
//! 里再存一份 key, 三份加起来 ~290B/信号。而名字总字节其实是固定的(约 40B/信号),
//! 所以正确做法是:
//!
//! * **arena**: 所有名字连续放进一块 `Vec<u8>`, 每信号只留 `(u32 偏移, u32 长度)`;
//! * **开放寻址索引**: `keys[] = 64 位哈希, slots[] = 下标+1`, 无 per-entry 分配、
//!   无 SipHash, 约 12B/槽(负载 ≤0.7 → ~17B/条目)。
//!
//! VCD 后端早已这么做(`names_blob`/`name_meta`/`OpenIndex`); FSDB 后端此前仍是
//! 三份 `String`, 所以这两个类型被提到这里共用。

/// 名字 arena: 连续字节 + 每名字 `(偏移, 长度)`。
pub(crate) struct NameArena {
    blob: Vec<u8>,
    spans: Vec<(u32, u32)>,
}

impl NameArena {
    pub(crate) fn with_capacity(names: usize, bytes: usize) -> Self {
        NameArena { blob: Vec::with_capacity(bytes), spans: Vec::with_capacity(names) }
    }

    /// 追加一个名字, 返回它的下标(与 `spans` 位置一致)
    pub(crate) fn push(&mut self, s: &str) -> u32 {
        let off = self.blob.len() as u32;
        self.blob.extend_from_slice(s.as_bytes());
        self.spans.push((off, s.len() as u32));
        (self.spans.len() - 1) as u32
    }

    /// 第 i 个名字的绝对 (偏移, 长度) —— 叶子名排序要预计算它
    #[inline]
    pub(crate) fn span(&self, i: u32) -> (u32, u32) {
        self.spans[i as usize]
    }

    /// arena 里的一段字节(越界视为空)
    #[inline]
    pub(crate) fn bytes_at(&self, off: u32, len: u32) -> &[u8] {
        let a = off as usize;
        let b = a.saturating_add(len as usize);
        self.blob.get(a..b).unwrap_or(&[])
    }

    #[inline]
    pub(crate) fn get(&self, i: u32) -> &str {
        let (off, len) = self.spans[i as usize];
        // 只由 `push(&str)` 写入 → 必为合法 UTF-8
        std::str::from_utf8(&self.blob[off as usize..(off + len) as usize]).unwrap_or("")
    }

    /// 诊断用(越界查询): 生产路径用 `get`
    #[allow(dead_code)]
    #[inline]
    pub(crate) fn get_opt(&self, i: u32) -> Option<&str> {
        if (i as usize) < self.spans.len() {
            Some(self.get(i))
        } else {
            None
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.spans.len()
    }

    /// 名字本体占的字节(不含 spans) —— 诊断/基准用
    #[allow(dead_code)]
    pub(crate) fn blob_len(&self) -> usize {
        self.blob.len()
    }
}

/// 开放寻址索引(键 = 64 位哈希, 值 = 下标)。
#[derive(Clone)]
pub(crate) struct OpenIndex {
    keys: Vec<u64>,
    slots: Vec<u32>,
    mask: usize,
    len: usize,
}

/// 探针位置必须用"混合后"的哈希: FNV 的低位周期性很强, 直接拿低位做
/// 开放寻址会让插入退化成 O(n²)(实测 4M 信号 23s → 0.2s)。
#[inline]
pub(crate) fn mix64(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// FNV-1a(波形内部表用; 不需要 SipHash 的抗碰撞性)
#[inline]
pub(crate) fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

impl OpenIndex {
    pub(crate) fn new(cap_hint: usize) -> Self {
        // 1.5x 预留 → 负载 ≈0.66(增长阈值 0.7), 比 2x 省 ~25% 内存
        let cap = (cap_hint.saturating_mul(3) / 2 + 16).max(16).next_power_of_two();
        OpenIndex { keys: vec![0; cap], slots: vec![0; cap], mask: cap - 1, len: 0 }
    }

    #[inline]
    pub(crate) fn find(&self, h: u64) -> Option<u32> {
        let mut i = (mix64(h) as usize) & self.mask;
        loop {
            let s = self.slots[i];
            if s == 0 {
                return None;
            }
            if self.keys[i] == h {
                return Some(s - 1);
            }
            i = (i + 1) & self.mask;
        }
    }

    /// 命中哈希后还要比对真实字节的表(名字表: 处理哈希冲突)
    #[inline]
    pub(crate) fn find_verified(&self, h: u64, verify: impl Fn(u32) -> bool) -> Option<u32> {
        let mut i = (mix64(h) as usize) & self.mask;
        loop {
            let s = self.slots[i];
            if s == 0 {
                return None;
            }
            if self.keys[i] == h && verify(s - 1) {
                return Some(s - 1);
            }
            i = (i + 1) & self.mask;
        }
    }

    pub(crate) fn insert(&mut self, h: u64, idx: u32, rehash: impl Fn(u32) -> u64) {
        if (self.len + 1) * 10 >= self.slots.len() * 7 {
            self.grow(&rehash);
        }
        let mut i = (mix64(h) as usize) & self.mask;
        while self.slots[i] != 0 {
            i = (i + 1) & self.mask;
        }
        self.keys[i] = h;
        self.slots[i] = idx + 1;
        self.len += 1;
    }

    fn grow(&mut self, rehash: &impl Fn(u32) -> u64) {
        let old = std::mem::take(&mut self.slots);
        let new_cap = (old.len() * 4).max(16);
        self.slots = vec![0; new_cap];
        self.keys = vec![0; new_cap];
        self.mask = new_cap - 1;
        for s in old {
            if s == 0 {
                continue;
            }
            let h = rehash(s - 1);
            let mut i = (mix64(h) as usize) & self.mask;
            while self.slots[i] != 0 {
                i = (i + 1) & self.mask;
            }
            self.keys[i] = h;
            self.slots[i] = s;
        }
    }

    #[allow(dead_code)]
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// 槽位占的字节(诊断/基准用)
    #[allow(dead_code)]
    pub(crate) fn bytes(&self) -> usize {
        self.keys.len() * 8 + self.slots.len() * 4
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::fsdb::{decode_tree_file, decode_tree_into, encode_tree_arena};

    #[test]
    fn arena_round_trip() {
        let mut a = NameArena::with_capacity(3, 64);
        let i0 = a.push("tb.u_cpu.clk");
        let i1 = a.push("tb.u_cpu.rst_n");
        let i2 = a.push("短名");
        assert_eq!((i0, i1, i2), (0, 1, 2));
        assert_eq!(a.get(0), "tb.u_cpu.clk");
        assert_eq!(a.get(1), "tb.u_cpu.rst_n");
        assert_eq!(a.get(2), "短名");
        assert_eq!(a.len(), 3);
        assert_eq!(a.get_opt(3), None);
        assert_eq!(a.blob_len(), "tb.u_cpu.clk".len() + "tb.u_cpu.rst_n".len() + "短名".len());
    }

    #[test]
    fn open_index_hit_miss_and_grow() {
        // 塞到触发扩容(阈值 0.7), 再校验全部可查、且不会把不同名字混起来
        let mut ix = OpenIndex::new(4);
        let names: Vec<String> = (0..500).map(|i| format!("tb.sig_{}", i)).collect();
        for (i, n) in names.iter().enumerate() {
            let h = fnv1a(n.as_bytes());
            // rehash 回调: 用下标反查名字再算哈希
            let idx = i as u32;
            ix.insert(h, idx, |j| fnv1a(names[j as usize].as_bytes()));
        }
        assert_eq!(ix.len(), 500);
        for (i, n) in names.iter().enumerate() {
            let h = fnv1a(n.as_bytes());
            let got = ix.find_verified(h, |j| names[j as usize] == *n);
            assert_eq!(got, Some(i as u32), "lookup {}", n);
        }
        assert_eq!(ix.find(fnv1a(b"tb.not_there")), None);
    }

    /// 老的存法(`Vec<String>` + `HashMap<String, usize>`)每信号占多少字节 —— 只作对照,
    /// 用来固定"这次省了多少"的口径(命名: 名字 40B 左右, 但三份副本 + 表头就上去了)。
    fn old_style_bytes_per_signal(n: usize) -> usize {
        // 还原 FSDB 后端**改动前**的存法: 同一份名字存四遍 ——
        //   Sig.name(叶子名) / Sig.full(全名) / sig_names[](全名) / HashMap 的 String 键
        let full: Vec<String> = (0..n)
            .map(|i| format!("tb.u_subsys_{}.u_blk_{}.sig_{}", i % 1000, i % 97, i))
            .collect();
        let leaf: Vec<String> = full.iter().map(|s| s.rsplit('.').next().unwrap().to_string()).collect();
        let mut map: std::collections::HashMap<&str, usize> = std::collections::HashMap::with_capacity(n);
        for (i, name) in full.iter().enumerate() {
            map.entry(name.as_str()).or_insert(i);
        }
        let str_cost = |v: &Vec<String>| -> usize { v.iter().map(|s| s.capacity() + 24).sum() };
        let keys_heap: usize = full.iter().map(|s| s.capacity()).sum(); // map 里那份键的堆副本
        let map_buckets = map.capacity() * 48; // 开放寻址桶: hash + key(24) + value(8) + 控制位
        (str_cost(&full) * 2 + str_cost(&leaf) + keys_heap + map_buckets) / n
    }

    /// 进程 RSS(KB; 只读 /proc/self/statm 的常驻页数)
    fn rss_kb() -> usize {
        let s = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
        let pages: usize = s.split_whitespace().nth(1).and_then(|x| x.parse().ok()).unwrap_or(0);
        pages * 4
    }

    /// **4M 信号规模的离线上限基准**(默认 `#[ignore]`, 手动跑):
    ///
    /// ```bash
    /// cargo test --release --lib -- --ignored --nocapture fsdb_name_path_4m
    /// ```
    ///
    /// 量的是 FSDB **读 `.fnames` 缓存 → 建索引 → 建叶子名索引**这条路径(不需要 NPI,
    /// 也就等真波形来之前就能回答"加载这一段要多久/多少内存")。
    /// 现场形状: 300MB / 400 万信号。
    #[test]
    #[ignore = "4M 名字 ≈ 1GB 瞬时内存; 手动 --ignored 跑"]
    fn fsdb_name_path_4m() {
        use std::time::Instant;
        const N: usize = 4_000_000;
        let t0 = Instant::now();
        let rss0 = rss_kb();
        // 名字生成: 复用同一个 buffer 直接进 arena(不造 4M 个 String —— 那正是要避免的)
        let mut arena = NameArena::with_capacity(N, N * 34);
        let mut widths: Vec<usize> = Vec::with_capacity(N);
        let mut buf = String::with_capacity(64);
        for i in 0..N {
            buf.clear();
            use std::fmt::Write as _;
            let _ = write!(
                buf,
                "tb_top.u_subsys_{}.u_blk_{}.u_core_{}.sig_reg_{}",
                i % 64,
                (i / 64) % 64,
                (i / 4096) % 64,
                i
            );
            arena.push(&buf);
            widths.push((i % 33) + 1);
        }
        let t_gen = t0.elapsed();
        let scopes: Vec<String> = (0..4096).map(|i| format!("tb_top.u_subsys.u_blk_{}", i)).collect();

        // ① 写缓存(encode)
        let t = Instant::now();
        let blob = encode_tree_arena(&arena, &widths, &scopes);
        let t_enc = t.elapsed();

        // ② 读缓存(decode → 直接进 arena)
        let t = Instant::now();
        let (arena2, widths2, scopes2) = decode_tree_into(&blob, 0).expect("decode");
        let t_dec = t.elapsed();
        assert_eq!(arena2.len(), N);
        assert_eq!(widths2.len(), N);
        assert_eq!(scopes2.len(), scopes.len());
        assert_eq!(arena2.get(123_456), arena.get(123_456));

        // ②b 生产路径: 从**文件**流式解码(不把整份缓存搬进内存)
        let blob_mb = blob.len() / 1024 / 1024;
        let tmpdir = std::env::temp_dir().join(format!("wal-fnames-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmpdir);
        let tmpf = tmpdir.join("x.fnames");
        std::fs::write(&tmpf, &blob).expect("write tmp fnames");
        let rss_before_file = rss_kb();
        let t = Instant::now();
        let (arena_file, _w, _s) = decode_tree_file(&tmpf, 0).expect("decode_tree_file");
        let t_dec_file = t.elapsed();
        let rss_after_file = rss_kb();
        assert_eq!(arena_file.len(), N);
        assert_eq!(arena_file.get(123_456), arena.get(123_456));
        let _ = std::fs::remove_file(&tmpf);

        // ③ 建名字索引(哈希 → 下标)
        let t = Instant::now();
        let mut hashes: Vec<u64> = Vec::with_capacity(N);
        let mut ix = OpenIndex::new(N);
        for i in 0..N as u32 {
            let h = fnv1a(arena2.get(i).as_bytes());
            hashes.push(h);
            ix.insert(h, i, |j| hashes[j as usize]);
        }
        let t_ix = t.elapsed();
        assert_eq!(ix.find_verified(hashes[999_999], |j| arena2.get(j) == arena2.get(999_999)), Some(999_999));

        // ④ 叶子名排序索引(短名解析用) —— 与 fsdb.rs 的 `leaf_order_index` 同口径:
        //    先一遍预计算叶子 (偏移,长度), 然后只比字节切片
        let t = Instant::now();
        let mut leaf_keys: Vec<(u32, u32)> = Vec::with_capacity(N);
        for i in 0..N as u32 {
            let (off, len) = arena2.span(i);
            let s = arena2.bytes_at(off, len);
            let rel = s.iter().rposition(|&b| b == b'.').map(|p| p + 1).unwrap_or(0);
            leaf_keys.push((off + rel as u32, len - rel as u32));
        }
        let mut order: Vec<u32> = (0..N as u32).collect();
        order.sort_unstable_by(|a, b| {
            let (oa, la) = leaf_keys[*a as usize];
            let (ob, lb) = leaf_keys[*b as usize];
            arena2.bytes_at(oa, la).cmp(arena2.bytes_at(ob, lb)).then(a.cmp(b))
        });
        let t_leaf = t.elapsed();

        let rss1 = rss_kb();
        println!(
            "4M 信号名字路径: 生成 {}ms | encode {}ms({}MB) | 内存 decode {}ms | **文件流式 decode {}ms** | 建索引 {}ms | 叶子排序 {}ms",
            t_gen.as_millis(),
            t_enc.as_millis(),
            blob_mb,
            t_dec.as_millis(),
            t_dec_file.as_millis(),
            t_ix.as_millis(),
            t_leaf.as_millis()
        );
        println!(
            "  文件流式 decode 期间 RSS: {}MB → {}MB(+{}MB; 内存 decode 会多背一份 {}MB 缓存)",
            rss_before_file / 1024,
            rss_after_file / 1024,
            (rss_after_file - rss_before_file) / 1024,
            blob_mb
        );
        println!(
            "  内存: arena {}MB + spans {}MB + 索引 {}MB + 叶子序 {}MB ; RSS {}MB → {}MB(+{}MB)",
            arena2.blob_len() / 1024 / 1024,
            arena2.len() * 8 / 1024 / 1024,
            ix.bytes() / 1024 / 1024,
            order.len() * 4 / 1024 / 1024,
            rss0 / 1024,
            rss1 / 1024,
            (rss1 - rss0) / 1024
        );
        // 别让编译器把东西优化掉
        assert!(order[0] < N as u32 && arena2.len() == N);
    }

    /// 4M 信号下的**每信号字节数** —— 目标 1 的验收口径。
    /// 实测口径: arena + 索引 ≈ 100B/信号, 老的"三份 String + HashMap"≈ 300B/信号。
    #[test]
    fn per_signal_bytes_stay_small() {
        const N: usize = 200_000; // 真实 4M 太慢, 按每信号字节线性外推
        let mut arena = NameArena::with_capacity(N, N * 40);
        let mut ix = OpenIndex::new(N);
        let mut hashes: Vec<u64> = Vec::with_capacity(N);
        for i in 0..N {
            let name = format!("tb.u_subsys_{}.u_blk_{}.sig_{}", i % 1000, i % 97, i);
            let h = fnv1a(name.as_bytes());
            hashes.push(h);
            let idx = arena.push(&name);
            debug_assert_eq!(idx as usize, i);
            // rehash 必须按**下标**重算哈希(扩容时会对所有旧条目回调):
            // 传一个"当前名字"的闭包是错的 —— 扩容会把旧条目全指到新哈希上。
            ix.insert(h, idx, |j| hashes[j as usize]);
        }
        let per_sig = (arena.blob_len() + arena.len() * 8 + ix.bytes()) / N;
        let old = old_style_bytes_per_signal(N);
        println!(
            "名字存储: 新 {}B/信号(arena {} + spans 8 + 索引 {}) vs 老 {}B/信号 → 省 {}%",
            per_sig,
            arena.blob_len() / N,
            ix.bytes() / N,
            old,
            100 - per_sig * 100 / old
        );
        // 上限放到 200B/信号: 真正的门是"别回到三份 String"(~300B)
        assert!(per_sig < 200, "每信号 {}B 太大", per_sig);
        assert!(per_sig * 2 < old, "新存法必须不到老存法的一半(新 {} vs 老 {})", per_sig, old);
    }
}
