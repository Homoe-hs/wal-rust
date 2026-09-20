# 安全策略

## 报告漏洞

**请不要开公开 issue。** 用 GitHub 的私密通道:

👉 https://github.com/Homoe-hs/wal-rust/security/advisories/new

或者发邮件到仓库 owner 的 GitHub 公开邮箱(见 <https://github.com/Homoe-hs>)。

请在报告里包含: 影响版本(`wal-rust --version`)、最小复现(波形片段或生成脚本)、
平台(`uname -m`、`ldd --version | head -1`)、以及你能判断出的影响面
(读到越界内存 / 任意文件写入 / 任意代码执行 / 仅仅崩溃)。

## 我们的处理方式

* 48 小时内确认收到; 修复版本发布后在此文件与本仓库 release notes 里致谢(除非你要求匿名)。
* 这是个人维护的项目, 没有赏金计划, 请谅解。

## 适用范围与已知边界

`wal-rust` 是一个**本地命令行工具**, 设计前提是"输入文件由使用者自己提供"。因此:

| 场景 | 我们的立场 |
|---|---|
| 解析**恶意构造**的 VCD/FST/FSDB 导致崩溃或越界 | **算漏洞**, 请报告 —— 解析路径大量使用 `unsafe`(mmap + 裸指针)与第三方解码库(wellen/lz4/flate2) |
| 用超大/畸形输入造成 OOM 或极慢(DoS) | 算 bug 但不一定算安全漏洞; 请附上样本规模, 我们会按性能问题处理 |
| 波形文件里携带"执行命令"的元数据导致代码执行 | **算漏洞**。WAL 脚本是显式执行的(`run script.wal`), 波形内容本身不应具有执行能力 |
| 依赖库的漏洞 | 请同时报告到上游; 我们会在下个补丁版本跟进(依赖清单见 `Cargo.lock`) |

## 供应链

* `Cargo.lock` **随仓库提交**, 发布二进制由 `make release` 在本机/CI 用固定工具链
  (`rust-toolchain.toml`)与 `cargo-zigbuild` 交叉构建。
* 发布物附 `sha256`(`.sha256` 文件 / release notes 里给出前缀), 请核对后再使用。
* 目前**未**做代码签名(计划中, 见 `CHANGELOG.md` 的 `[未发布]` 一节)。
