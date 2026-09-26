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

TCG 客机里读 FSDB 慢一个数量级, 所以能在宿主跑就在宿主跑。实测可用的四步:

```bash
# ① 起 VM(25A 身份)          : sh .tools/vm/run25.sh   (托管作业, 别用 wrapper 起)
# ② 客机里起许可 + 放行防火墙 : ssh … 'sh /mnt/wal/lic25_hostfwd.sh'
# ③ 宿主导出环境              : . .tools/vm/host_fsdb_env.sh
# ④ 直接读                    : target/release/wal-rust '(length (SIGNALS))' -l x.fsdb
```

第 ② 步做了三件**缺一不可**的事, 全是踩出来的:

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

> 备选: 直接在 VM 里跑 wal-rust(2018 或 25A 的 NPI 都能用)。正确性够用, 性能数字要打折扣。

## 5 报错 → 判读

| 现象 | 真因 | 处置 |
|---|---|---|
| `npi_fsdb_open 失败` + `NSIS` / "比本机 Verdi 的 reader 新" | reader 比写者老 | 换 ≥ 写者版本的 Verdi/NPI(第 6 节) |
| `Failed to check out license` / FlexLM `-16,287` | 许可没起来 / **被防火墙拒** / daemon 版本不匹配 | 先确认 daemon 与工具**同代**(25A 工具必须配 25A 许可), 再查第 4 节那三件事 |
| `Fatal License Error` + `SCL-602` | 用了**跨代**许可(如 25A 工具 + 2018 许可) | 换成同代 daemon |
| `Could not open string resource file …/etc/sdProd.res` | 只拷了 `share/NPI`, 缺 `etc/` | 把 `etc/` 一起拷(第 6 节最小集) |
| `dlopen libNPI.so 失败` | `$VERDI_HOME`/`$WAL_NPI_LIB` 不对 | `scripts/fsdb_env_check.sh` |

## 6 别人的波形比本机 reader 新(例如 X-2025.06-**SP3** 写的)

同 release 的不同 SP 也可能被拒(reader 只看版本串)。处置优先级:

1. **先试**: `scripts/fsdb_env_check.sh their.fsdb` —— 它会把写者版本串和真实 open 结果一起打出来;
2. 打不开就是 NSIS, 需要 **≥ SP3 的 Verdi/NPI**。最小可迁移集(不必搬整个 26GB):
   `share/NPI`(951MB) + `share/vcst` + `etc`(503MB) + `bin`,
   放进共享目录后用 `WAL_NPI_LIB=<…>/share/NPI/lib/linux64/libNPI.so` 指过去;
3. 只要**小样本**就能判定兼容性, 不必等大文件 —— 让写波形的人顺手导一份 1MB 的。

## 7 每轮开始的一键自检

```bash
./scripts/fsdb_env_check.sh [x.fsdb]   # 工具链 / 写者版本 / magic / 许可 / 真实 open 一次
make bench-fsdb FSDB=x.fsdb            # 冷建 vs 暖查询(数字进 bench/perf-history.csv)
```
