<div align="center">

# ⚡ FastSync

**Rust 编写的高速单向目录同步工具。**

把源目录镜像到目标目录：速度快、可预览、覆盖已有文件时更稳妥。

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.85%2B-orange.svg)](https://www.rust-lang.org/)
[![Edition](https://img.shields.io/badge/Edition-2024-orange.svg)](https://doc.rust-lang.org/edition-guide/rust-2024/index.html)
[![BLAKE3](https://img.shields.io/badge/Compare-BLAKE3-brightgreen.svg)](https://github.com/BLAKE3-team/BLAKE3)
[![GitHub](https://img.shields.io/badge/GitHub-ShouChenICU%2FFastSync-black.svg)](https://github.com/ShouChenICU/FastSync)

[English](README.md) · [性能优先](#-性能优先) · [安全模型](#-默认安全模型) · [安装](#-安装) · [命令速查](#-命令速查)

</div>

| 快                                    | 可预期                     | 避免文件损坏                       |
| ------------------------------------- | -------------------------- | ---------------------------------- |
| Rust、元数据优先、BLAKE3、并发 worker | 先预览、显式删除、摘要清晰 | 降低意外中断后留下不完整文件的风险 |

## ✨ 为什么选择 FastSync？

FastSync 面向大目录、本地镜像和需要明确结果的同步场景：速度要快，但不能悄悄做危险的事。

- **Rust 编写**：原生二进制执行，资源占用更可控，部署也更简单。
- **为速度设计**：元数据感知比较、BLAKE3、并发 worker。
- **默认安全**：不隐式删除、支持预览、覆盖已有文件时使用临时文件。
- **结果清楚**：给人看的摘要，也支持给脚本用的 JSON。

```mermaid
flowchart LR
    A["源目录"] --> B["扫描"]
    C["目标目录"] --> B
    B --> D["比较"]
    D --> E["复制 / 更新"]
    D --> F["可选删除"]
    E --> G["可读摘要"]
    F --> G
```

## 🏎️ 性能优先

目录同步通常混合了文件系统延迟、元数据判断、哈希计算和真实复制。FastSync 将这些阶段保持得清晰、可控。

| 性能设计         | 作用                                                                     |
| ---------------- | ------------------------------------------------------------------------ |
| Rust 实现        | 原生执行性能，内存和 CPU 行为更可预期。                                  |
| 元数据感知比较   | 在大小、修改时间适合作为内容信号时用于快速判断，元数据同步保持独立配置。 |
| BLAKE3 哈希      | 在需要内容确认时，用高速现代哈希算法完成强比较。                         |
| 有界 worker 队列 | 并发复制，同时避免任务无限堆积导致内存失控。                             |
| 新文件直接复制   | 目标端不存在的新文件直接写入，避免不必要的临时文件重命名开销。           |

> [!NOTE]
> 快速比较是默认模式。需要在元数据一致时也用 BLAKE3 确认内容时，可以使用 `--strict`。

## 🚀 快速开始

预览同步计划：

```bash
fastsync -n ./source ./target
```

真正执行同步：

```bash
fastsync ./source ./target
```

同步并删除目标端陈旧文件：

```bash
fastsync -n -d ./source ./target
fastsync -d ./source ./target
```

> [!CAUTION]
> `--delete` 会删除目标端中“源目录不存在”的项目。首次使用删除模式时，请先用 `-n -d` 预览。

## 📦 安装

FastSync 使用 Rust 2024 edition，要求 Rust 1.85 或更新版本。如果使用 `rustup`，建议使用稳定工具链：

```bash
rustup default stable
rustup component add rust-src
```

### 从源码构建

```bash
git clone https://github.com/ShouChenICU/FastSync.git
cd FastSync
cargo build --release
./target/release/fastsync --help
```

### 使用 Cargo 从 Git 安装

```bash
cargo install --git https://github.com/ShouChenICU/FastSync
```

## 🧭 常见场景

| 目标                       | 命令                                  |
| -------------------------- | ------------------------------------- |
| 预览同步                   | `fastsync -n ./source ./target`       |
| 将一个目录同步到另一个目录 | `fastsync ./source ./target`          |
| 同步并删除目标端陈旧项目   | `fastsync -d ./source ./target`       |
| 使用严格比较模式           | `fastsync --strict ./source ./target` |
| 限制 worker 线程数         | `fastsync -t 4 ./source ./target`     |
| 为脚本输出 JSON            | `fastsync -o json ./source ./target`  |

<details>
<summary><strong>示例：安全地同步照片备份</strong></summary>

```bash
# 第一步：先查看会发生什么。
fastsync -n -d ~/Photos /mnt/backup/Photos

# 第二步：确认无误后执行。
fastsync -d ~/Photos /mnt/backup/Photos
```

</details>

<details>
<summary><strong>示例：快速同步构建缓存</strong></summary>

```bash
fastsync ./target/release ./cache/release
```

默认快速模式会信任完全一致的元数据；只有同大小文件的修改时间或支持的权限不一致时，才会用 BLAKE3 确认内容。

</details>

## 🛡️ 默认安全模型

| 默认行为         | 为什么重要                                                                             |
| ---------------- | -------------------------------------------------------------------------------------- |
| 单向同步         | 源目录是权威数据，目标目录跟随源目录。                                                 |
| 不隐式删除       | 不传 `--delete` 时，目标端额外文件会被保留。                                           |
| 快速内容比较     | 两端已有文件默认信任一致的元数据；同大小但元数据不一致时才用 BLAKE3 确认内容。         |
| 临时文件覆盖写入 | 覆盖已有文件时，默认先写入临时文件名，再重命名到目标路径，降低中断后留下半文件的风险。 |
| 新文件直接复制   | 目标端不存在的新文件直接复制，避免不必要的重命名开销。                                 |
| 支持预览模式     | 可以在修改目标目录前查看计划。                                                         |

## 🔍 选择比较模式

| 模式     | 行为                                                                                                           | 适合场景                                         |
| -------- | -------------------------------------------------------------------------------------------------------------- | ------------------------------------------------ |
| `fast`   | 元数据一致时认为文件一致；元数据不一致时，大小不同直接认为内容不一致，大小相同再用 BLAKE3 确认内容。默认模式。 | 希望保持速度，同时对同大小的可疑变化做内容确认。 |
| `strict` | 大小一致时始终使用 BLAKE3 确认内容，即使元数据也一致。                                                         | 重要数据，需要元数据一致时也做内容确认。         |

`--strict` 是 `--compare strict` 的快捷方式。

> [!IMPORTANT]
> 快速模式可能漏掉“内容变化但大小、修改时间和支持的权限都未变化”的文件。重要数据如果需要元数据一致时也确认内容，请使用 `strict`。

同名文件的元数据同步与内容比较策略相互独立，并且默认开启。可以用 `--no-sync-metadata` 跳过内容已相同文件的独立元数据更新，或用 `--preserve-times false` 和/或 `--preserve-permissions false` 缩小保留范围。

## ✅ 校验策略

使用 `--verify` 控制复制后的校验：

| 模式      | 行为                               |
| --------- | ---------------------------------- |
| `none`    | 复制后不校验。                     |
| `changed` | 校验发生覆盖的文件，默认模式。     |
| `all`     | 同步后校验源目录中的所有普通文件。 |

摘要会分开统计两类 BLAKE3 内容检查：`fast` 或 `strict` 在比较阶段执行的内容比较，以及由 `--verify` 控制的复制后校验。
目标端不存在的新文件会直接复制，不会计入复制后的 BLAKE3 校验次数。

## 🧾 命令速查

| 参数                                         | 含义                                         |
| -------------------------------------------- | -------------------------------------------- |
| `-n`, `--dry-run`                            | 只预览，不修改目标目录。                     |
| `-d`, `--delete`                             | 删除目标端中源目录不存在的项目。             |
| `--strict`                                   | 对同大小的已有文件使用严格 BLAKE3 内容确认。 |
| `-c`, `--compare <fast\|strict>`             | 选择比较策略。                               |
| `--no-sync-metadata`                         | 不更新内容已相同的同名文件元数据。           |
| `--preserve-times <auto\|true\|false>`       | 控制时间戳同步。                             |
| `--preserve-permissions <auto\|true\|false>` | 控制权限同步。                               |
| `--verify <none\|changed\|all>`              | 选择复制后校验策略。                         |
| `-t`, `--threads <N\|auto>`                  | 设置 worker 线程数。                         |
| `-q`, `--queue-size <N>`                     | 设置有界任务队列长度。                       |
| `--no-atomic-write`                          | 禁用覆盖时的临时文件写入。                   |
| `-o`, `--output <text\|json>`                | 设置摘要输出格式。                           |
| `-l`, `--log-level <level>`                  | 设置日志级别。                               |

查看完整帮助：

```bash
fastsync --help
```

不带任何参数运行 `fastsync` 时，也会输出帮助页面。

## 🧪 开发

`Cargo.toml` 中的 `edition = "2024"` 是 Rust edition 名称，不是当前日历年份。Rust edition 是可选择的语言兼容性里程碑；即使在 2026 年构建项目，Rust 2024 edition 仍然是正确写法。

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

维护者和代码 agent 请阅读 [AGENTS.md](AGENTS.md)。

## ❓ 常见问题

<details>
<summary><strong>FastSync 是双向同步吗？</strong></summary>

不是。FastSync 是单向同步：从源目录同步到目标目录。

</details>

<details>
<summary><strong>FastSync 默认会删除文件吗？</strong></summary>

不会。只有显式传入 `--delete` 或 `-d` 时才会删除目标端额外项目。

</details>

<details>
<summary><strong>我应该使用 <code>--strict</code> 吗？</strong></summary>

如果是重要个人数据或生产数据，且你希望元数据一致时也确认内容，可以使用。对于生成文件、缓存、构建产物，默认的 `fast` 模式通常是更合适的折中。

</details>

## 📄 开源协议

FastSync 使用 [MIT License](LICENSE) 开源。

作者：[ShouChen](https://github.com/ShouChenICU)

项目地址：[https://github.com/ShouChenICU/FastSync](https://github.com/ShouChenICU/FastSync)
