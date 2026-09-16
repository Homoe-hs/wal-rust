// 垫片 ABI 冒烟: 按 Rust 侧将来完全一样的调用序列
#include <stdio.h>
#include <stdint.h>
typedef struct WalFsdb WalFsdb;
extern WalFsdb *wal_fsdb_open(const char *);
extern void wal_fsdb_close(WalFsdb *);
extern int wal_fsdb_nsignals(WalFsdb *);
extern int wal_fsdb_signal(WalFsdb *, int, char *, int, long long *, int *, char *, int);
extern long long wal_fsdb_find(const WalFsdb *, const char *);
extern long long wal_fsdb_ntimes(WalFsdb *);
extern long long wal_fsdb_time(WalFsdb *, long long);
extern int wal_fsdb_initial(WalFsdb *, long long, char *, int);
extern long long wal_fsdb_nchanges(WalFsdb *, long long);
extern int wal_fsdb_change(WalFsdb *, long long, long long, unsigned long long *, char *, int);
extern int wal_fsdb_value_at(WalFsdb *, long long, unsigned long long, char *, int, unsigned long long *, int *);

int main(int argc, char **argv) {
    if (argc < 2) return 2;
    WalFsdb *h = wal_fsdb_open(argv[1]);
    if (!h) { fprintf(stderr, "open failed\n"); return 1; }
    printf("signals=%d times=%lld", wal_fsdb_nsignals(h), wal_fsdb_ntimes(h));
    if (wal_fsdb_ntimes(h) > 0)
        printf(" first_t=%lld last_t=%lld", wal_fsdb_time(h, 0), wal_fsdb_time(h, wal_fsdb_ntimes(h) - 1));
    printf("\n");
    int n = wal_fsdb_nsignals(h);
    for (int i = 0; i < n; i++) {
        char name[512], type[64]; long long id; int w;
        wal_fsdb_signal(h, i, name, sizeof name, &id, &w, type, sizeof type);
        printf("SIG %-28s id=%-4lld w=%-3d %s", name, id, w, type);
        char buf[4096];
        int il = wal_fsdb_initial(h, id, buf, sizeof buf);
        printf(" init=%s", il >= 0 ? buf : "-");
        long long nc = wal_fsdb_nchanges(h, id);
        printf(" nchg=%lld", nc);
        if (nc > 0) {
            unsigned long long t; wal_fsdb_change(h, id, 0, &t, buf, sizeof buf);
            printf(" first=(%llu %s)", t, buf);
            wal_fsdb_change(h, id, nc - 1, &t, buf, sizeof buf);
            printf(" last=(%llu %s)", t, buf);
        }
        printf("\n");
    }
    // 随机取值口径
    const char *probe = argc > 2 ? argv[2] : NULL;
    if (probe) {
        long long id = wal_fsdb_find(h, probe);
        char buf[4096]; unsigned long long t; int isd;
        for (int k = 0; k < 3; k++) {
            unsigned long long qs[3] = {0, 30, 100000};
            int r = wal_fsdb_value_at(h, id, qs[k], buf, sizeof buf, &t, &isd);
            printf("AT %s t<=%llu -> %s (hit_t=%llu dump=%d)\n", probe, qs[k], r >= 0 ? buf : "?", t, isd);
        }
    }
    wal_fsdb_close(h);
    return 0;
}
