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
