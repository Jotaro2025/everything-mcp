# v1.5.0 更新说明 / Release Notes

## 中文

1.5.0 是一个功能特性与体验增强版本，重点提升大模型检索与阅读效率、强化敏感凭据防护、优化文本解码性能，并增加运行时并发观测能力。完全向后兼容 1.4.x 的所有配置、端口及协议特性。

### 新增特性与优化

- **`grep` 支持返回匹配行上下文（`context_lines`）。** 新增可选参数 `context_lines`（范围 `0..=5`，默认为 `0`）。开启后将在命中结果中返回匹配行前后的文本列表（`before` 与 `after`）。上下文行严格计入 128 KiB 输出预算和 16384 字符行截断规则，避免额外检索往返。
- **`read_file` 支持行号前缀格式化（`line_numbers`）。** 新增可选布尔参数 `line_numbers`（默认为 `false`）。开启后各行统一以 `行号 | ` 前缀格式化（例如 `  42 | fn example() {`），便于大模型直接对齐行号进行精准代码分析与编辑。
- **大模型搜索别名智能归一化。** 在 `pattern` 参数解析前增加自动语法转换，容忍并转换 LLM 常见的非 Everything 语法习惯：
  - `extension:<ext>` 自动转换为 `ext:<ext>`
  - `modified:<time>` 自动转换为 `dm:<time>`
  - `created:<time>` 自动转换为 `dc:<time>`
  - `filename:<name>` 自动转换为 `name:<name>`
  - 类似 `*.{rs,toml}` 的大括号扩展模式转换为 `ext:rs;toml`
- **UTF-16 文本零堆分配解码优化。** 重构 `read.rs` 中的 UTF-16LE / UTF-16BE 解码逻辑与二进制嗅探检查，基于 `as_chunks::<2>()` 和 `char::decode_utf16` 迭代器直接流式解码，省去分配临时 `Vec<u16>` 堆内存的开销。
- **敏感凭据与文件拦截扩展。** 进一步收紧文件读取安全防线，在敏感文件黑名单中补充拦截：
  - 凭据与配置文件：`credentials`、`service-account.json`、`service_account.json`、`client_secret.json` 及其 `.old` / `.bak` 备份文件
  - 硬件认证私钥与钥匙串：FIDO 硬件秘钥 `id_ecdsa_sk`、`id_ed25519_sk`、KeePass 密码库（`.kdbx`）、macOS 钥匙串（`.keychain`）及证书私钥（`.p8`、`.pkcs12`）
- **服务器并发连接与峰值监控指标。** 在 `server_diagnostics` 工具的返回结果中新增 `active_connections`（当前在途活跃请求数）与 `peak_connections`（启动以来的历史最高并发连接数），便于监控与定位服务吞吐。
- **协议清单精简与契约保持。** 扩展工具参数定义的同时维持 `tools/list` 报文体积在 19.6 KiB（严格低于 20 KiB 上下文预算上限），保留完整的 UNC 共享路径指引与 Everything 索引前提说明。

### 验证与交付

- 全量单元测试（143 项）与 MCP 集成测试（91 项）全部通过（共 234 项）。
- `cargo clippy --locked --all-targets -- -D warnings` 零告警通过。
- `cargo fmt --all -- --check` 格式检查通过。

### 安装

运行对应架构的 `everything-mcp-1.5.0-{x64,x86}-setup.exe`。安装器通过 Everything 官方 `-setup-plugin` 入口完成安装；卸载位于 Everything → 选项 → 插件。

---

## English

1.5.0 is a feature and developer experience release focusing on LLM retrieval efficiency, sensitive credential protection, zero-allocation text decoding, and runtime concurrency observability. It is fully backward-compatible with 1.4.x configuration and protocol semantics.

### Features and Improvements

- **Context lines support in `grep` (`context_lines`).** Added an optional `context_lines` parameter (range `0..=5`, default `0`). Matching results include `before` and `after` line arrays around each hit, bounded by the shared 128 KiB output budget and 16384-character line clipping.
- **Line numbering option in `read_file` (`line_numbers`).** Added an optional boolean parameter `line_numbers` (default `false`). When enabled, returned lines are formatted with `{line:4} | {text}` prefixes (e.g. `  42 | fn example() {`) for direct line alignment by LLMs.
- **Query syntax alias normalization for LLMs.** Automatically normalizes frequent LLM search patterns into Everything syntax before query validation:
  - `extension:<ext>` -> `ext:<ext>`
  - `modified:<time>` -> `dm:<time>`
  - `created:<time>` -> `dc:<time>`
  - `filename:<name>` -> `name:<name>`
  - Brace expansions like `*.{rs,toml}` -> `ext:rs;toml`
- **Zero-allocation UTF-16 decoding.** Optimized UTF-16LE and UTF-16BE file decoding and binary sniffing in `read.rs` to stream via `as_chunks::<2>()` and `char::decode_utf16`, eliminating intermediate `Vec<u16>` allocations.
- **Sensitive file protection expansion.** Extended the security denylist in `sensitive.rs` to deny:
  - Credentials and service account configs: `credentials`, `service-account.json`, `service_account.json`, `client_secret.json` and backup suffixes.
  - Hardware keys and vaults: FIDO security keys `id_ecdsa_sk`, `id_ed25519_sk`, KeePass databases (`.kdbx`), macOS keychains (`.keychain`), and PKCS keys (`.p8`, `.pkcs12`).
- **Server concurrency metrics in `server_diagnostics`.** Exposes `active_connections` (current in-flight requests) and `peak_connections` (highest in-flight request count since start) for runtime visibility.
- **Compact schema footprint.** All new tool schemas fit within 19.6 KiB (strictly under the 20 KiB payload limit), preserving full documentation for UNC paths, index prerequisites, and paging contracts.

### Validation and Delivery

- All 234 automated tests (143 library unit tests and 91 integration tests) passed.
- Formatting check (`cargo fmt --all -- --check`) and Clippy (`-D warnings`) clean.

### Install

Run `everything-mcp-1.5.0-{x64,x86}-setup.exe` for your architecture. It uses Everything's official `-setup-plugin` entry point. Uninstall via Everything → Options → Plugins.

**Full Changelog**: https://github.com/Jotaro2025/everything-mcp/compare/v1.4.2...v1.5.0
