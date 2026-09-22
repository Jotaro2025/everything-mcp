# reference/ — 参考代码与 SDK

[中文](#中文) | [English](#english)

---

## 中文

本目录存放 **voidtools 官方发布的第三方材料**，仅供本项目的 Rust 插件开发参考，
不属于本项目代码，也不参与 `everything-mcp` 的编译。修改它们不会影响插件产物。

| 目录 | 内容 | 许可证 |
| ---- | ---- | ------ |
| `etp_server-1.0.2.5/` | 官方 ETP/FTP 服务器插件（C 语言）。本插件的**主线程 marshaling 方案、`db_query_search2` 参数表、PM_* 消息处理**均对照它的源码实现 | MIT（Copyright © 2025 voidtools / David Carpenter） |
| `http_server-1.0.5.6/` | 官方 HTTP 服务器插件（C 语言）。参考其基于 `everything_plugin.h` 的 host 函数解析与设置读写方式 | MIT（Copyright © 2025 voidtools / David Carpenter） |

各目录自带的 `LICENSE`、`README.md`、`Changes.txt` 均为原始文件，未作改动。

> 本项目自身的代码位于仓库根目录（Rust crate，`Cargo.toml` / `src/`），
> 采用 MIT 许可证，见 [`../LICENSE`](../LICENSE) 与 [`../README.md`](../README.md)。

## English

This directory contains **official third-party material published by voidtools**,
kept for reference while developing the Rust plugin in this repository. It is not
part of this project's code and is never compiled into `everything-mcp`.

| Folder | Contents | License |
| ------ | -------- | ------- |
| `etp_server-1.0.2.5/` | Official ETP/FTP server plugin (C). Source of truth for the main-thread marshaling approach, the `db_query_search2` parameter list, and PM_* message handling used by this plugin | MIT (Copyright © 2025 voidtools / David Carpenter) |
| `http_server-1.0.5.6/` | Official HTTP server plugin (C). Reference for resolving host functions via `everything_plugin.h` and reading/writing settings | MIT (Copyright © 2025 voidtools / David Carpenter) |

The `LICENSE`, `README.md` and `Changes.txt` files inside each folder are the
originals, unmodified.

> This project's own code is the Rust crate at the repository root
> (`Cargo.toml` / `src/`), MIT licensed — see [`../LICENSE`](../LICENSE) and
> [`../README.md`](../README.md).
