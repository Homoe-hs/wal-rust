# WAL 脚本示例

| 脚本 | 用途 | 跑法 |
|---|---|---|
| `tilelink.wal` | TileLink/通用总线协议性能分析器(演示宏、列表、时间窗统计) | `wal-rust run -l design.vcd examples/wal/tilelink.wal` |

约定:
* 示例必须**可独立运行**(只依赖仓库里的脚本 + 使用者自带的波形), 脚本头部写清用途与跑法;
* 面向语言特性的最小示例请放 `test_samples/`(那是测试用例的家), 这里放"像真实工具一样"的脚本。
