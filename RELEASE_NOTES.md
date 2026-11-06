# v1.4.2 更新说明 / Release Notes

## 中文

1.4.2 是基于 1.4.1 的兼容修复版本，重点改进插件停止、文件扫描预算和 HTTP/JSON-RPC 校验。保留 1.4.1 的内容索引设置修复、格式诊断、`binary_skipped` 和 `total_ms` 字段；现有设置与端口配置继续使用。

### 修复与增强

- **修复内容索引开关的应用竞态。** 实机复测发现旧路径会在主线程等待配置转发进程、过早删除临时文件，并把自行写入 INI 当成生效证据。现在保存后关闭选项窗口，在后台串行应用；先释放闲置查询引用，保留临时文件直到转发结束，只有稳定回读一致才确认成功。诊断新增 `pending_enabled` 和 `apply_error`，MCP 服务设置仍即时应用。
- **停止和卸载更可靠。** 监听线程使用有界的就绪等待；即使自连接唤醒失败也会回收线程。停止时主动关闭在途 socket，并等待所有工作线程结束，避免部分请求拖到读取超时或线程活过 DLL 卸载。
- **宿主调度减少空转。** 主线程任务等待改用条件变量；取消只移除尚未执行的任务，已开始的宿主回调保留上下文直到完成。锁等待、查询提交和结果读取共享请求截止时间。
- **文件读取受限且保持句柄一致。** `read_file` 与 grep 共用单次打开的句柄完成真实路径复核、元数据检查与读取。先嗅探前 4 KiB，最多读取 8 MiB + 1 字节用于检测增长，避免文件变化导致无界分配。
- **grep 按实际 I/O 计账。** 64 MiB 总预算包含被拒绝的二进制样本及失败前已读的字节，`timeout_ms` 覆盖候选查询与正文扫描。新增 `files_skipped`、`truncation_reason`；保留 `binary_skipped`，它是跳过总数中的二进制子集。仅列文件模式首次命中后停止该文件的行匹配。
- **日志扫描有明确边界。** 倒序读取先拼接原始字节再解码，修复跨块中文与 emoji 损坏；增加总读取预算、天数、时间和输出限制，通过 `truncation_reason` 报告未完成扫描。
- **协议校验更严格。** 非法 JSON-RPC 信封正确返回错误和 `id: null`，显式 null ID 与缺省 ID 通知分开处理。拒绝歧义 Content-Length、Transfer-Encoding、畸形请求行、不完整正文和无效 UTF-8；允许来源的实际响应也返回 CORS 头。
- **修复 x86 参数及设置恢复。** `max_results` 先按 u64 校验范围，再转换为平台整数；x86 正确读取 `everything_mcp32.dll` 配置段。
- **实机发现的日志与多实例修复。** 识别英文 `File Create/Modify/Delete/Rename/Move` 标签；配置转发保留 `-instance` 与 `-config`，读取对应实例配置及日志，避免串用默认实例。

### 验证与交付

- CI 增加格式检查、锁定依赖构建，以及 DLL 系统依赖白名单检查。
- 新增 `python tests/live_host.py` 和实机验收说明。脚本默认核对 `Cargo.toml` 中的版本，使用隔离合成文件验证搜索、读取、grep、并发、协议及已发布字段兼容性。
- x64、x86 各 231 项自动化测试通过，1 项宿主专用测试忽略；格式检查、Clippy、双架构构建通过。最终 DLL 在 Everything 1.5.0.1423b x64 隔离实例通过 16/16 验收和真实英文日志解析；设置开关的启用/禁用已通过 UI 验证。带未完成 HTTP 请求正常退出约 74 ms，连接约 72 ms 关闭，重启后查询成功（小型测试索引的单次测量）。
- 大型主实例上的候选构建通过一轮 16/16 验收，但内容索引更新期间仍观察到搜索超时。最终 DLL 的完整验收使用独立测试索引；不把这一结果描述为大型索引性能或持续可用性保证。文件夹索引可能把跨目录移动记录为删除加创建，插件保留宿主事件语义。
- 文件系统取消仍是协作式的：执行中的 Windows/SMB 调用和宿主 FFI 必须等待安全返回。宿主退出耗时还包含 Everything 保存索引的时间。

### 安装

运行对应架构的 `everything-mcp-1.4.2-{x64,x86}-setup.exe`。安装器通过 Everything 官方 `-setup-plugin` 入口完成安装；卸载位于 Everything → 选项 → 插件。

---

## English

1.4.2 is a compatible patch release based on 1.4.1, focused on shutdown, scan budgets and HTTP/JSON-RPC validation. It retains the content-indexing settings fixes, format diagnostics, `binary_skipped` and `total_ms` from 1.4.1. Existing settings and port configuration carry over.

### Fixes and improvements

- **Fixed content-indexing application races.** Live retesting exposed UI-thread waiting on the configuration forwarder, premature temporary-file removal and false confirmation from the plugin's own INI writes. Saved changes now apply in the background after Options closes. Idle query references are released first; the input survives until the forwarder exits, and stable readback is required. Diagnostics expose `pending_enabled` and `apply_error`; MCP service settings still apply immediately.
- **Reliable shutdown and unload.** A bounded readiness wait lets the listener exit even if self-connect wakeup fails. Shutdown closes active sockets and joins every worker before DLL unload, including workers handling incomplete requests.
- **Host dispatch without busy waiting.** Queued main-thread tasks use a condition variable. Cancellation reclaims only tasks that have not started; active callbacks retain their context until completion. Lock acquisition, submission and result retrieval share the request deadline.
- **Bounded, single-handle reads.** `read_file` and grep validate the final path, inspect metadata and read through one handle. A 4 KiB sample rejects binary content early; reads are capped at 8 MiB + 1 byte to detect growth without unbounded allocation.
- **grep charges actual I/O.** The shared 64 MiB budget includes rejected binary samples and bytes read before an error. `timeout_ms` covers candidate discovery and scanning. New `files_skipped` and `truncation_reason` fields complement the existing `binary_skipped` counter. Files-only matching stops at the first matching line in each file.
- **Bounded journal scanning.** Reverse scans assemble raw bytes before decoding, preserving Chinese and emoji across block boundaries. Total read, day, time and output limits report incomplete work through `truncation_reason`.
- **Stricter protocol validation.** Invalid JSON-RPC envelopes return proper errors with `id: null`; explicit null IDs remain distinct from notifications. Ambiguous Content-Length, Transfer-Encoding, malformed request lines, incomplete bodies and invalid UTF-8 are rejected. Actual responses include CORS headers for allowed origins.
- **x86 validation and settings restore.** Validate `max_results` as u64 before narrowing and read the `everything_mcp32.dll` settings section on x86.
- **Journal and instance fixes found live.** Recognize English `File Create/Modify/Delete/Rename/Move` labels. Configuration forwarding preserves `-instance` and `-config`; configuration and journal reads select the current instance.

### Validation and delivery

- CI now checks formatting, builds with locked dependencies and validates DLL imports against a system-library allowlist.
- Added `python tests/live_host.py` and live-test documentation. The script checks the version from `Cargo.toml` and uses isolated synthetic fixtures for search, reads, grep, concurrency, protocol and prior-release response compatibility.
- x64 and x86 each passed 231 automated tests with one host-only test ignored; formatting, Clippy and both builds passed. The final DLL passed 16/16 live checks and real English journal parsing in an isolated Everything 1.5.0.1423b x64 instance. Enabling/disabling content indexing was verified through the UI. A partial HTTP connection closed in about 72 ms and the host exited in 74 ms; queries recovered after restart (one measurement with a small test index).
- A candidate build passed 16/16 checks on the large primary instance, but searches also timed out during content-index updates. Full final-DLL acceptance used a separate test index; it does not guarantee large-index performance or continuous availability. Folder indexes may represent a cross-directory move as delete plus create; the plugin preserves the host event semantics.
- Cancellation remains cooperative between filesystem calls. In-progress Windows/SMB calls and host FFI must return safely. Everything shutdown time also includes saving its index.

### Install

Run `everything-mcp-1.4.2-{x64,x86}-setup.exe` for your architecture. It uses Everything's official `-setup-plugin` entry point. Uninstall via Everything → Options → Plugins.

**Full Changelog**: https://github.com/Jotaro2025/everything-mcp/compare/v1.4.1...v1.4.2
