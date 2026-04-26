# WAL-Rust

A high-performance waveform analysis toolkit in Rust.

## 项目状态

**开发中** - 当前专注于 `walconv` 工具。

## 组件

### walconv - VCD转FST转换器

将VCD波形文件转换为FST二进制格式，支持超大文件（10GB+）和高性能转换。

**特性：**
- 10GB VCD < 10秒转换（高端NVMe）
- 内存占用 ≤200MB
- 支持压缩VCD输入 (.gz, .bz2, .xz)
- 精确错误报告（行号+列号）
- 多线程自动检测
- 实时进度显示

**性能目标：**

| 文件大小 | 硬件 | 目标时间 |
|----------|------|----------|
| 1GB | NVMe | 1-2秒 |
| 10GB | 高端NVMe | 8-10秒 |
| 10GB | 普通NVMe | 12-15秒 |

**CLI：**
```bash
walconv convert input.vcd output.fst
```

**选项：**
- `-v, --verbose` - 详细输出 (-vv 完整调试)
- `-t, --threads <N>` - 线程数 (0=自动)
- `-c, --compression <ALG>` - lz4|zlib|fastlz (默认: lz4)
- `-s, --skip-errors` - 跳过错误行继续

## 构建

```bash
# Debug构建
cargo build

# Release构建
cargo build --release

# 静态链接构建（推荐用于分发）
RUSTFLAGS="-C target-feature=+crt-static" cargo build --release --target x86_64-unknown-linux-musl
```

## 架构

```
walconv/
├── src/
│   ├── main.rs              # CLI入口+进度条
│   ├── lib.rs               # 库接口
│   ├── cli.rs              # CLI解析+日志
│   ├── fst/                # FST格式库
│   │   ├── types.rs        # BlockType, FstHeader
│   │   ├── varint.rs       # 变长编码
│   │   ├── blocks.rs       # 块序列化
│   │   ├── writer.rs       # FstWriter
│   │   └── compress.rs     # LZ4/zlib压缩
│   ├── vcd/                # VCD解析器
│   │   ├── types.rs        # VcdEvent, VcdError
│   │   ├── reader.rs       # 压缩感知读取
│   │   └── parser.rs       # 状态机解析器
│   └── convert/
│       └── pipeline.rs      # 转换管道
└── tests/
```

## 参考文档

详细规格说明请见 [CONSTRUCTION.md](./CONSTRUCTION.md)。

## 依赖

- Rust 1.70+
- LZ4, libdeflate, fastlz（压缩）
- flate2, bzip2, xz2（输入解压）
- clap, indicatif（CLI/进度条）

## 致谢

FST格式参考自 [GTKWave](https://github.com/gtkwave/gtkwave) 项目。
