// wal_fsdb_shim —— 把 Verdi 的 FFR(C++)读者接口收成**稳定 C ABI**, 供 wal-rust 使用。
//
// 设计要点
//   * FFR 是 C++ 类接口, 跨编译器/版本 ABI 敏感 → Rust 侧只认下面的 C 函数,
//     垫片单独编成 libwal_fsdb.so, 运行时 dlopen(主二进制保持零专有库)。
//   * **按信号懒建列**: 第一次用到某信号时, 用 FFR 的值变化迭代器抽出它的完整
//     变更列并缓存。这与 wal-rust "首次触碰抽列 → signal_cache/旁挂缓存" 的模型一致。
//   * **同一 XTag 多次写入(glitch/delta)折叠为最后一次** —— 就是我们的
//     "某索引处的值 = 该时间戳内最后一次写入"。
//   * **口径 A**: t0 的整片初值 dump 是初值快照, 不是 INDEX。判据: 若所有信号的首个
//     XTag 都是 0, 则该 XTag 是 dump → 取它的值当 initial, 时间轴从下一个 XTag 开始;
//     否则(只有部分信号在 t0 变化)t0 是普通时间步, 无 initial。
//
// 编译见 build_shim.sh(宿主 Verdi 的 FFR 头/库, 免 license)。
#include "ffrAPI.h"
#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <map>
#include <string>
#include <vector>

#ifndef TRUE
#define TRUE 1
#endif
#ifndef FALSE
#define FALSE 0
#endif

struct WalFsdb {
    ffrObject *obj = nullptr;
    std::string path;

    struct Sig {
        std::string name;
        long long idcode = 0;
        int left = 0, right = 0;
        uint32_t bpb = 0;
        std::string type;
    };
    std::vector<Sig> sigs;
    std::map<long long, size_t> by_id;

    // 该信号的一组值变化: 同 XTag 折叠后的 (时间, 4 态位串)
    struct Col {
        std::vector<unsigned long long> times;
        std::vector<std::string> vals;
    };
    std::map<long long, Col> cols;                 // 原始列缓存(含 t0 dump)
    std::map<long long, size_t> skip;              // 规则 A: 该信号列要跳过的前缀条目数(0/1)
    std::map<long long, std::string> initial;      // 规则 A 的 t0 快照
    std::vector<unsigned long long> times;         // 时间轴(规则 A 之后)

    const Sig *find_sig(long long idcode) const {
        auto it = by_id.find(idcode);
        return it == by_id.end() ? nullptr : &sigs[it->second];
    }

    // 抽出(并折叠)某信号的全部值变化 —— **原始列**, 含 t0; 规则 A 的裁剪由 skip 表示。
    const Col &column(long long idcode) {
        auto it = cols.find(idcode);
        if (it != cols.end()) return it->second;
        Col c;
        const Sig *s = find_sig(idcode);
        ffrVCTrvsHdl vh = obj->ffrCreateVCTrvsHdl(static_cast<fsdbVarIdcode>(idcode));
        if (vh) {
            fsdbTag64 t;
            if (vh->ffrGetMinXTag(&t) == FSDB_RC_SUCCESS && vh->ffrGotoXTag(&t) == FSDB_RC_SUCCESS) {
                while (true) {
                    fsdbTag64 cur;
                    byte_T *vc = nullptr;
                    if (vh->ffrGetXTag(&cur) != FSDB_RC_SUCCESS) break;
                    if (vh->ffrGetVC(&vc) != FSDB_RC_SUCCESS || !vc) break;
                    unsigned long long tv = tag_to_u64(&cur);
                    std::string bits = decode(vc, vh->ffrGetBitSize(), s ? s->bpb : 0);
                    if (!c.times.empty() && c.times.back() == tv) {
                        c.vals.back() = bits;              // 同 XTag: 最后一次写入胜出
                    } else {
                        c.times.push_back(tv);
                        c.vals.push_back(bits);
                    }
                    if (vh->ffrGotoNextVC() != FSDB_RC_SUCCESS) break;
                }
            }
            vh->ffrFree();
        }
        return cols.emplace(idcode, std::move(c)).first->second;
    }

    static uint64_t tag_to_u64(const fsdbTag64 *t) {
        return (static_cast<uint64_t>(t->H) << 32) | static_cast<uint64_t>(t->L);
    }
    static void u64_to_tag(unsigned long long v, fsdbTag64 *t) {
        t->H = static_cast<uint_T>(v >> 32);
        t->L = static_cast<uint_T>(v & 0xffffffffu);
    }
    // 4 态位串: bpb=1B(或 0, 枚举 0 = 1B)时每位一字节 → 0/1/x/z; 其它编码走十六进制
    static std::string decode(const byte_T *vc, uint_T bits, uint32_t bpb) {
        std::string out;
        out.reserve(bits);
        if (bpb == FSDB_BYTES_PER_BIT_1B || bpb == 0) {
            for (uint_T i = 0; i < bits; i++) {
                switch (vc[i]) {
                    case FSDB_BT_VCD_0: out.push_back('0'); break;
                    case FSDB_BT_VCD_1: out.push_back('1'); break;
                    case FSDB_BT_VCD_X: out.push_back('x'); break;
                    case FSDB_BT_VCD_Z: out.push_back('z'); break;
                    default:            out.push_back('u'); break;
                }
            }
        } else {
            char b[8];
            for (uint_T i = 0; i < bits && out.size() < 128; i++) {
                snprintf(b, sizeof b, "%02x", vc[static_cast<size_t>(i) * bpb]);
                out += b;
            }
        }
        return out;
    }
};

static std::vector<std::string> g_scope_stack;

static bool_T tree_cb(fsdbTreeCBType cb_type, void *client_data, void *tree_cb_data) {
    auto *self = static_cast<WalFsdb *>(client_data);
    switch (cb_type) {
        case FSDB_TREE_CBT_SCOPE: {
            auto *s = static_cast<fsdbTreeCBDataScope *>(tree_cb_data);
            g_scope_stack.push_back(s->name);
            break;
        }
        case FSDB_TREE_CBT_UPSCOPE:
            if (!g_scope_stack.empty()) g_scope_stack.pop_back();
            break;
        case FSDB_TREE_CBT_VAR: {
            auto *v = static_cast<fsdbTreeCBDataVar *>(tree_cb_data);
            WalFsdb::Sig sig;
            for (auto &sc : g_scope_stack) { sig.name += sc; sig.name += "."; }
            sig.name += v->name;
            sig.idcode = v->u.idcode;
            sig.left = v->lbitnum;
            sig.right = v->rbitnum;
            sig.bpb = v->bytes_per_bit;
            switch (v->type) {
                case FSDB_VT_VCD_WIRE: sig.type = "wire"; break;
                case FSDB_VT_VCD_REG: sig.type = "reg"; break;
                case FSDB_VT_VCD_INTEGER: sig.type = "integer"; break;
                case FSDB_VT_VCD_PARAMETER: sig.type = "parameter"; break;
                case FSDB_VT_VCD_REAL: sig.type = "real"; break;
                case FSDB_VT_VCD_TIME: sig.type = "time"; break;
                default: sig.type = "other"; break;
            }
            if (self) self->sigs.push_back(sig);
            break;
        }
        default:
            break;
    }
    return TRUE;
}

extern "C" {

int wal_fsdb_is_fsdb(const char *path) {
    return (path && ffrObject::ffrIsFSDB(const_cast<char *>(path))) ? 1 : 0;
}

WalFsdb *wal_fsdb_open(const char *path) {
    if (!path || !ffrObject::ffrIsFSDB(const_cast<char *>(path))) return nullptr;
    auto *h = new WalFsdb();
    h->path = path;
    g_scope_stack.clear();
    h->obj = ffrObject::ffrOpen3(const_cast<char *>(path));
    if (!h->obj) { delete h; return nullptr; }
    h->obj->ffrSetTreeCBFunc(tree_cb, h);
    h->obj->ffrReadScopeVarTree();
    for (size_t i = 0; i < h->sigs.size(); i++) h->by_id[h->sigs[i].idcode] = i;
    for (auto &s : h->sigs) h->obj->ffrAddToSignalList(s.idcode);
    h->obj->ffrLoadSignals();

    // ---- 时间轴(所有信号 VC 时间的并集) + 规则 A 判定 ----
    std::vector<unsigned long long> all;
    bool all_at_zero = !h->sigs.empty();
    for (auto &s : h->sigs) {
        const auto &c = h->column(s.idcode);          // 原始列(含 t0)
        for (auto t : c.times) all.push_back(t);
        if (c.times.empty() || c.times.front() != 0) all_at_zero = false;
    }
    std::sort(all.begin(), all.end());
    all.erase(std::unique(all.begin(), all.end()), all.end());

    bool t0_is_dump = all_at_zero;                    // 所有信号的首个 XTag 都在 t0 → 整片 dump
    for (auto &s : h->sigs) {
        const auto &c = h->column(s.idcode);
        if (t0_is_dump && !c.times.empty() && c.times.front() == 0) {
            h->initial[s.idcode] = c.vals.front();    // t0 dump → 初值快照
            h->skip[s.idcode] = 1;                    // 且不进时间轴/变更列
        }
    }
    h->times.reserve(all.size());
    for (auto t : all) {
        if (t0_is_dump && t == 0) continue;
        h->times.push_back(t);
    }
    return h;
}

void wal_fsdb_close(WalFsdb *h) {
    if (!h) return;
    if (h->obj) h->obj->ffrClose();
    delete h;
}

int wal_fsdb_nsignals(WalFsdb *h) { return h ? static_cast<int>(h->sigs.size()) : 0; }

int wal_fsdb_signal(WalFsdb *h, int i, char *name, int name_cap,
                    long long *idcode, int *width, char *type, int type_cap) {
    if (!h || i < 0 || i >= static_cast<int>(h->sigs.size())) return -1;
    auto &s = h->sigs[i];
    if (name) {
        if (static_cast<int>(s.name.size()) + 1 > name_cap) return -1;
        memcpy(name, s.name.c_str(), s.name.size() + 1);
    }
    if (idcode) *idcode = s.idcode;
    if (width) *width = (s.left >= s.right) ? (s.left - s.right + 1) : 1;
    if (type) {
        if (static_cast<int>(s.type.size()) + 1 > type_cap) type[0] = 0;
        else memcpy(type, s.type.c_str(), s.type.size() + 1);
    }
    return 0;
}

// 按名字找 idcode(支持全名/叶子名); 找不到 → -1
long long wal_fsdb_find(const WalFsdb *h, const char *name) {
    if (!h || !name) return -1;
    for (auto &s : h->sigs) if (s.name == name) return s.idcode;
    size_t n = strlen(name);
    for (auto &s : h->sigs) {
        if (s.name.size() >= n && s.name.compare(s.name.size() - n, n, name) == 0
            && (s.name.size() == n || s.name[s.name.size() - n - 1] == '.'))
            return s.idcode;
    }
    return -1;
}

long long wal_fsdb_ntimes(WalFsdb *h) { return h ? static_cast<long long>(h->times.size()) : 0; }
long long wal_fsdb_time(WalFsdb *h, long long idx) {
    if (!h || idx < 0 || idx >= static_cast<long long>(h->times.size())) return -1;
    return static_cast<long long>(h->times[idx]);
}

// t0 快照(规则 A); 无 → -1
int wal_fsdb_initial(WalFsdb *h, long long idcode, char *buf, int cap) {
    if (!h) return -1;
    auto it = h->initial.find(idcode);
    if (it == h->initial.end()) return -1;
    if (static_cast<int>(it->second.size()) + 1 > cap) return -1;
    memcpy(buf, it->second.c_str(), it->second.size() + 1);
    return static_cast<int>(it->second.size());
}

long long wal_fsdb_nchanges(WalFsdb *h, long long idcode) {
    if (!h) return 0;
    const auto &c = h->column(idcode);
    size_t sk = h->skip.count(idcode) ? h->skip[idcode] : 0;
    return static_cast<long long>(c.times.size() - sk);
}

// 第 i 个变更: 时间 + 4 态位串(out_time 可空); 失败 → -1
int wal_fsdb_change(WalFsdb *h, long long idcode, long long i,
                    unsigned long long *out_time, char *buf, int cap) {
    if (!h || i < 0) return -1;
    const auto &c = h->column(idcode);
    size_t sk = h->skip.count(idcode) ? h->skip[idcode] : 0;
    size_t k = static_cast<size_t>(i) + sk;
    if (k >= c.times.size()) return -1;
    const std::string &v = c.vals[k];
    if (static_cast<int>(v.size()) + 1 > cap) return -1;
    memcpy(buf, v.c_str(), v.size() + 1);
    if (out_time) *out_time = c.times[k];
    return static_cast<int>(v.size());
}

// ≤time 的最近值(与 (at s T) 同口径); out_time 给命中的变更时间, out_is_dump 标初值
int wal_fsdb_value_at(WalFsdb *h, long long idcode, unsigned long long time,
                      char *buf, int cap, unsigned long long *out_time, int *out_is_dump) {
    if (!h) return -1;
    const auto &c = h->column(idcode);
    size_t sk = h->skip.count(idcode) ? h->skip[idcode] : 0;
    // 二分: 最后一个 time_i <= time
    size_t n = c.times.size(), lo = 0, hi = n;
    while (lo < hi) {
        size_t mid = lo + (hi - lo) / 2;
        if (c.times[mid] <= time) lo = mid + 1; else hi = mid;
    }
    if (lo <= sk) {
        // 早于首个**真实**变更(或命中的就是 t0 dump)→ 初值(规则 A)或 x
        auto it = h->initial.find(idcode);
        if (out_time) *out_time = 0;
        if (out_is_dump) *out_is_dump = it != h->initial.end() ? 1 : 0;
        const std::string v = (it != h->initial.end()) ? it->second : std::string("x");
        if (static_cast<int>(v.size()) + 1 > cap) return -1;
        memcpy(buf, v.c_str(), v.size() + 1);
        return static_cast<int>(v.size());
    }
    const std::string &v = c.vals[lo - 1];
    if (static_cast<int>(v.size()) + 1 > cap) return -1;
    memcpy(buf, v.c_str(), v.size() + 1);
    if (out_time) *out_time = c.times[lo - 1];
    if (out_is_dump) *out_is_dump = 0;
    return static_cast<int>(v.size());
}

// 时间轴是否与"所有 # 步"一致(诊断用): 返回轴元素个数
long long wal_fsdb_axis_check(WalFsdb *h) {
    return h ? static_cast<long long>(h->times.size()) : 0;
}

} // extern "C"
