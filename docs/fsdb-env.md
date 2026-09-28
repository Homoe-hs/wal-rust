# FSDB 环境与许可(读/写波形的前置条件)

> ✅ 现行 —— 本机(Arch 宿主机 + NixOS 客机 VM)上"读 FSDB / 写 FSDB"的完整清单与踩坑。
> 换机器照第 2、4 节核对; **拿到别人的波形先跑 `scripts/fsdb_env_check.sh`**(第 7 节)。

## 1 一句话:FSDB 是**写者产物**

能不能读一份 FSDB, 取决于三件事同时成立:

1. **reader 版本 ≥ 写者版本**(写者版本串就写在文件头里, 不占许可);
2. **许可能 checkout**(NPI 在 `npi_fsdb_open` 时才 checkout);
3. 库路径/环境变量对(`$VERDI_HOME` 或 `$WAL_NPI_LIB`)。

三者缺一, 报错长得都不一样 —— 先看第 5 节的判读表, 别猜。

## 2 本机现状矩阵(2026-09)

| 角色 | 版本 | 位置 | 配哪个许可 |
|---|---|---|---|
| **reader**(宿主机, 推荐) | Verdi **X-2025.06-SP1** | `~/eda_tools/synopsys/verdi/X-2025.06-SP1` | 25A daemon(27080) |
| **reader**(客机 VM) | 同上(拷了 NPI+etc+bin 到共享盘) | `/mnt/wal/verdi25` | 25A daemon |
| reader(客机 VM, 老) | Verdi O-2018.09-SP2 | `/mnt/eda/verdi_home2018` | 2018 daemon(27051) |
| **writer** | VCS **X-2025.06-SP1** | `~/Projects/fsdb-parser/eda_tools/synopsys/vcs/X-2025.06-SP1` | 25A daemon(27080) |
| writer(老) | VCS O-2018.09-SP2 | 同上目录 `vcs/O-2018.09-SP2` | 2018 daemon(27051) |
| 许可文件 | 25A: `scl/2025.03/admin/license/synopsys.lic`; 2018: `scl/2018.06/.../eetop2018.lic` | 共享盘 | — |

> 实测: VCS 25A **编译通过**(`VCS_RC=0`)、25A NPI **能读 2018 写的 FSDB**(只打一条
> "generated using a previous version" 警告)。

## 3 两套许可是**身份互斥**的

| | 主机名 | 网卡 MAC(hostid) | 端口 |
|---|---|---|---|
| 25A(新) | `ic_designer` | `d0:50:99:d4:16:0b` | 27080 |
| 2018(旧) | `rone` | `00:0c:29:04:00:65` | 27051 |

所以 VM **一次只能扮一套**: `.tools/vm/run.sh` = 2018 身份, `.tools/vm/run25.sh` = 25A 身份
(后者已经把 `hostfwd=tcp::27080-:27080` 与 `27081` 一起配好)。

## 4 让宿主机**原生**读 FSDB(不经过 VM, 快一个量级)

TCG 客机里读 FSDB 慢一个数量级, 所以能在宿主跑就在宿主跑。**注意 VM 在这里只当"许可服务器"**
(现场许可绑的是内网身份 `ic_designer` + MAC, 宿主冒充不了), 解析本身完全在宿主原生跑。
两条命令:

```bash
# ① 只在**会话内**用托管作业把 VM 起起来(它同时提供 27080/27081 的 hostfwd)
#    → 作为后台作业跑: sh .tools/vm/run25.sh
# ② 拉许可通道(幂等, 重复跑安全: 客机 hostname=/mnt/wal 挂载/防火墙/daemon 一次做完)
sh .tools/vm/license_up.sh
#    结尾必须看到 "✅ 通道可用: => N"; 看到 ❌ 就别往下测性能
# ③ 导出宿主环境 + 读
. .tools/vm/host_fsdb_env.sh
target/release/wal-rust '(length (SIGNALS))' -l x.fsdb
```

`.tools/vm/license_up.sh` 在客机里做的事**每一步都是踩出来的**:

* **客机 hostname 必须是 `ic_designer`**: 许可 `SERVER` 行按主机名+hostid 匹配; 重启 VM 后
  hostname 会回到 `nixos`(运行期设置不持久), 所以脚本每次都兜一遍。
* **`/mnt/wal` 每次开机都要重挂**: 它不在客机 `/etc/fstab` 里 → 开机后 `/mnt/wal` 是空的,
  于是 `sh /mnt/wal/lic25_hostfwd.sh` 报 "No such file or directory"。脚本里 `mount -t 9p …`。
* **许可副本改成 `SERVER 127.0.0.1`**: 客户端连的是 `27080@127.0.0.1`, 但 lmgrd 会告诉它
  "vendor daemon 在 <SERVER 行里的主机名>:<随机端口>" —— 主机名不换成 127.0.0.1, 宿主机
  既解析不了 `ic_designer`, 也连不上那个随机端口。
* **钉住 vendor daemon 端口**: `VENDOR snpslmd PORT=27081`(许可副本里改, 签名不受影响),
  这样 QEMU 的 `hostfwd` 才能把它一起转出去。
* **放行 27080/27081**: ★真正的坑★ — 客机 NixOS 的 INPUT 链会 **reject** 经 hostfwd 进来的
  许可流量(`dmesg` 里是 `refused connection: … DST=10.0.2.15 … DPT=27080`), 宿主侧看到的
  却是 FlexLM 的 `-16,287 Cannot read data from license server system`, 看起来像"协议不通",
  其实是**被防火墙拒了**。`iptables -I INPUT 1 -p tcp -m multiport --dports 27080,27081 -j ACCEPT`
  (运行期规则, 重启 VM 后要重加, 所以脚本里每次都会兜一遍)。
* **起 daemon 前要等旧进程真的死透**: 旧 lmgrd 还占着 27080 时, 新 lmgrd 只会
  `Retrying for about 5 more minutes` —— 脚本"成功"返回但许可 1~2 分钟内不可用, 宿主侧报的是
  误导性的 "需要 NPI 读库"。所以 `/mnt/wal/lic25_hostfwd.sh` 现在是"等进程消失 → 起 → 等端口真在听"。

> 备选: 直接在 VM 里跑 wal-rust(2018 或 25A 的 NPI 都能用)。正确性够用, 性能数字要打折扣。

## 5 报错 → 判读

| 现象 | 真因 | 处置 |
|---|---|---|
| `npi_fsdb_open 失败` + `NSIS` / "比本机 Verdi 的 reader 新" | reader 比写者老 | 换 ≥ 写者版本的 Verdi/NPI(第 6 节) |
| `Failed to check out license` / FlexLM `-16,287` | 许可没起来 / **被防火墙拒** / daemon 版本不匹配 | 先确认 daemon 与工具**同代**(25A 工具必须配 25A 许可), 再查第 4 节那几件事 |
| `FSDB 需要 Verdi 的 NPI **许可**(库已找到, 是 NPI 初始化没过)` | `npi_init` 时 checkout Verdi 席位失败 | 同上。**注意**: 0.14.36 以前的文案是"需要 NPI 读库(libNPI.so)", 会把人引去翻安装目录 —— 库好得很(实测 `dlopen` 13ms 成功, 只是 `npi_init` 返回 0) |
| 端口 `27080` "通" 但宿主侧仍取不到许可 | QEMU `hostfwd` 的监听**一直存在**: 客机里 daemon 死了它也照收连接 | 别只看端口。看客机 `pgrep -c snpslmd` 与 `lmstat -a`;`.tools/vm/license_up.sh` 结尾的"真打开一次"才是判据 |
| `Fatal License Error` + `SCL-602` | 用了**跨代**许可(如 25A 工具 + 2018 许可) | 换成同代 daemon |
| `Could not open string resource file …/etc/sdProd.res` | 只拷了 `share/NPI`, 缺 `etc/` | 把 `etc/` 一起拷(第 6 节最小集) |
| `dlopen libNPI.so 失败` | `$VERDI_HOME`/`$WAL_NPI_LIB` 不对 | `scripts/fsdb_env_check.sh` |
| lmgrd 日志 `Retrying for about 5 more minutes` | 旧 lmgrd/snpslmd 还占着 27080 | 等它们死透再起(`/mnt/wal/lic25_hostfwd.sh` 已内置等待) |
| `.tools/vm/license_up.sh` 报 "✅ 通道可用", **一分钟后又不可用**(端口 CLOSED / 报"需要 NPI 许可") | 客机 daemon 起来后又掉了(2026-09-28 本机实测到;宿主侧 `hostfwd` 监听一直在, 所以"端口通"不等于可用) | 在**同一次调用里**"license_up → 立刻跑一次真打开"(`. .tools/vm/host_fsdb_env.sh` + 任一查询)确认;持久问题看客机 `pgrep -c snpslmd` / `lmstat -a` / lmgrd 日志 |

## 6 别人的波形比本机 reader 新(例如 X-2025.06-**SP3** 写的)

同 release 的不同 SP 也可能被拒(reader 只看版本串)。处置优先级:

1. **先试**: `scripts/fsdb_env_check.sh their.fsdb` —— 它会把写者版本串和真实 open 结果一起打出来;
2. 打不开就是 NSIS, 需要 **≥ SP3 的 Verdi/NPI**。最小可迁移集(不必搬整个 26GB):
   `share/NPI`(951MB) + `share/vcst` + `etc`(503MB) + `bin`,
   放进共享目录后用 `WAL_NPI_LIB=<…>/share/NPI/lib/linux64/libNPI.so` 指过去;
3. 只要**小样本**就能判定兼容性, 不必等大文件 —— 让写波形的人顺手导一份 1MB 的。

## 7 每轮开始的一键自检

```bash
sh .tools/vm/license_up.sh             # 许可通道(VM 只当许可服务器);结尾要看到 ✅
./scripts/fsdb_env_check.sh [x.fsdb]   # 工具链 / 写者版本 / magic / 许可(含端口探测) / 真实 open 一次
make bench-fsdb FSDB=x.fsdb            # 冷建 vs 暖查询(数字进 bench/perf-history.csv)
```

## 8 现场大波形就位后的接收流程(landing)

拿到别人的波形(如内网 300MB / 400 万信号的那份), **先入库再优化** —— 没有基线,
"比上一版更好"就无从谈起。一条命令做完:

```bash
sh .tools/vm/license_up.sh                                   # ① 先要许可通道
. .tools/vm/host_fsdb_env.sh
scripts/fsdb_landing_check.sh /path/to/wave.fsdb [信号全名]   # ② 接收体检(约十几分钟)
scripts/fsdb_landing_check.sh /path/to/wave.fsdb --quick      #    只要"能不能读 + 并行有没有收益"
```

它按顺序做六件事(每件都是为后面优化做准备的):

| # | 做什么 | 为什么要 |
|---|---|---|
| ① | 身份留档: 大小 / **sha256** / mtime / FSDB magic / **写者版本串** | 后面 A/B 的口径;写者版本决定 reader 够不够(第 6 节) |
| ② | 环境自检(`scripts/fsdb_env_check.sh`): 许可通道 + 真实 open 一次 | 打不开就别谈性能;错因在报告里分段可见 |
| ③ | 磁盘余量 | 缓存会占地(`.fnames` 名字树 / `.ftl` 时间线 / `.fcol` 变更列), 建议 ≥ 波形×3 |
| ④ | 基线电池(`scripts/bench_fsdb.sh`): load / edge / level / at × 冷/建/暖 | 三个区段各自一条基线, 数字自动追加 `bench/perf-history.csv` |
| ⑤ | 并行轴: 冷建时间线 `-j 1` / `-j auto` / `-j 8` | "**单个波形**多核加速"到底有没有收益、收益多少(现场主诉) |
| ⑥ | 一致性: `-j 1` 与 `-j auto` 产出的 `.ftl` **逐字节相同** + 索引空间取值逐字符相同 | 并行 map/reduce 的正确性闸;不一致 = 先修对再谈快(退出码 3) |

判读(拿到数字后怎么落动作):

* `load` 大 → 名字树遍历(NPI 逐 scope/sig 走树 + 名字存储);对应 `WAL_DEBUG_FSDB=1` 里那行
  名字存储占用(arena/spans/索引), 4M 信号的经验值是每个信号 ~73B。
* `cold-*` 大 → 冷建全局时间线 = **整文件过一遍**(FSDB 没有 VCD 那种内存映射索引), 唯一还在
  秒级以上的操作;并行就是冲它来的, 看 ⑤ 的 `-j 1 → -j auto` 差多少。
* `hit-*` 明显大于 `load` → 暖路径没吃到旁挂列缓存(`.fcol`);查 `WAL_CACHE` /
  `WAL_CACHE_MIN_MB` / 缓存目录是否可写(缓存落在**执行命令的路径**下)。
* 三个区段都小, 但"现场用起来慢" → 问题在**调用方式**(每个查询一个新进程 → 每次重付 load;
  用 `--stdin` 一次加载多探针, 或 LSF 侧 `scripts/lsf_wal_run.sh`)。

落地约定:

* 波形文件**不进仓库**: 放 `.tools/wave/` 或任意目录(仓库盘 158GB 空余够用);
  `bench/perf-history.csv` 里只留文件名与数字。
* 每次改完 FSDB 路径都重跑一次 landing, 让同一个 fixture 的数字连续可比。

