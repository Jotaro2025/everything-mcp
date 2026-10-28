# v1.4.0 更新说明 / Release Notes

## 中文

### 安全（宿主崩溃与泄密修复）

- **敏感路径黑名单加固**：匹配前按 Win32 语义规范化路径（按组件剥掉尾随点/空格），补上 8.3 短名（`CREDEN~1.JSO`）与 `.bak/.old/~` 副本；`read_file`/`grep` 对已打开句柄用 `GetFinalPathNameByHandleW` 取真实路径二次复核，取不到时按 fail-closed 拒绝。实测 `id_rsa`、`id_rsa.`、`CREDEN~1.JSO` 均返回 `PATH_DENIED`。
- **修复搜索超时 use-after-free**：放弃的查询不再释放后台线程仍可能读取的搜索串缓冲（`db_query_cancel` 可能不存在或未生效），改为刻意泄漏，避免宿主进程崩溃。
- **连接线程不再可能活过 DLL 卸载**：连接线程句柄注册并在 PM_STOP 时 join；所有等待循环停机感知，停机秒级收敛（旧实现可能冻结 UI 15 秒并放任线程执行已卸载代码）。
- **HTTP 防线收口**：`Content-Length > 1 MiB` 在读 body 前拒绝；方法/Origin 校验前置于读 body；`Origin: null` 拒绝；移除通配 `Access-Control-Allow-Origin: *`；OPTIONS 预检返回 204 并精确回显白名单来源。
- **索引日志**：单日文件超过 64 MiB 时改扫最新窗口（旧实现扫最旧 64 MiB，最新变更永远看不到）；跨块多字节 UTF-8 不再损坏；极端时间戳不再溢出回绕。

### 协议与正确性

- 未知工具错误码 `-32601` → `-32602`（MCP 规范）；JSON 合法但信封非法回 `-32600`（不再误报 `-32700`）；通知统一 `202` 空体。
- `search_everywhere` 响应补上 schema 承诺的 `truncated` 字段；`server_diagnostics` 统计补第 9 个未知工具兜底槽。
- x86 下 `offset/start_line/max_lines/head_limit` 的 usize 截断修复。
- `timeout_ms` 增加服务端上限 300000 毫秒，防止单次搜索长期占住共享查询锁（默认仍为 10 秒）。
- 查询完成事件埋点：`db_query_create` 传入归属标记 `user_data`，事件写入 `plugin.log`，为后续按归属过滤提供数据。

### 新功能

- 设置页新增「启用内容索引」开关：直接读写 Everything 核心设置 `content_indexing_enabled`（新版 Everything 已移到「高级」页），点应用后通过官方 `-add-config` 立即生效；`server_diagnostics` 增加只读回显。

### 其他修复

- `Plugins.ini` 解析支持 UTF-16/ANSI；`os_event_create` 从强制依赖降为可选；统计页最小高度修正；统计测试串行化；CI 缓存 `restore-keys`、release tag 与版本一致性校验、DLL 导出表检查。

### 安装

运行对应架构的 `everything-mcp-1.4.0-{x64,x86}-setup.exe`，它会找到 Everything.exe 并以 `-setup-plugin` 参数重新拉起主程序完成安装；卸载在 Everything 选项 → 插件 里一键完成。

---

## English

### Security (host-crash and data-leak fixes)

- **Sensitive-path denylist hardening**: paths are normalized with Win32 semantics before matching (per-component trailing dots/spaces stripped), plus 8.3 short names (`CREDEN~1.JSO`) and `.bak/.old/~` copies; `read_file`/`grep` re-check the final path via `GetFinalPathNameByHandleW` and fail closed when it cannot be resolved. Verified live: `id_rsa`, `id_rsa.`, `CREDEN~1.JSO` all return `PATH_DENIED`.
- **Fixed a search-timeout use-after-free**: an abandoned query no longer frees the search-string buffer while the background thread may still read it (`db_query_cancel` may be missing or ineffective); the buffer is deliberately leaked instead.
- **Connection threads can no longer outlive DLL unload**: handles are registered and joined on PM_STOP; every wait loop is shutdown-aware, so stop() converges in seconds (the old code could freeze the UI for 15 s and leave threads running unloaded code).
- **HTTP hardening**: `Content-Length > 1 MiB` is rejected before the body is read; method/Origin checks now run before the body is read; `Origin: null` is rejected; the wildcard `Access-Control-Allow-Origin: *` is gone; OPTIONS preflight returns 204 and echoes the allowlisted origin.
- **Index journal**: day files larger than 64 MiB now scan the newest window (the old code scanned the oldest 64 MiB, so the latest changes were never visible); multi-byte UTF-8 split across chunks is no longer corrupted; extreme timestamps no longer overflow.

### Protocol and correctness

- Unknown tool now returns `-32602` instead of `-32601`; valid JSON with an invalid JSON-RPC envelope returns `-32600` (not `-32700`); notifications always get `202` with an empty body.
- `search_everywhere` now includes the `truncated` field its schema promised; `server_diagnostics` lists the ninth (unknown) stats slot.
- Fixed usize truncation on x86 for `offset/start_line/max_lines/head_limit`.
- `timeout_ms` now has a server-side cap of 300000 ms so one search cannot hold the shared query lock indefinitely (default remains 10 s).
- Query-completion instrumentation: `db_query_create` now receives a `user_data` tag and events are logged to `plugin.log`, providing data for future per-query filtering.

### New

- The options page now exposes an "Enable content indexing" switch for Everything's core `content_indexing_enabled` setting (moved to the Advanced page in newer builds). Applying it takes effect immediately via the official `-add-config` command; `server_diagnostics` reports the current value.

### Other fixes

- `Plugins.ini` parsing now handles UTF-16/ANSI; `os_event_create` is optional instead of required; stats page minimum height fixed; stats tests serialized; CI cache `restore-keys`, release tag/version check, and DLL export verification.

### Install

Run the setup exe for your architecture (`everything-mcp-1.4.0-{x64,x86}-setup.exe`); it locates Everything.exe and relaunches it with `-setup-plugin` to complete the installation. Uninstall anytime via Everything → Options → Plugins.

**Full Changelog**: https://github.com/Jotaro2025/everything-mcp/compare/v1.3.1...v1.4.0
