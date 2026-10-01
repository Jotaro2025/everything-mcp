# v1.3.0 更新说明 / Release Notes

## 中文

### 新增

- **`server_diagnostics` 自诊断工具**：零参数、只读。返回服务实际监听状态与配置值、全局搜索档位、journal 日志目录是否存在（`index_changes` 空结果的第一排查项）、诊断日志路径（`%LOCALAPPDATA%\everything-mcp\plugin.log`）以及每工具的调用/成功/出错统计——LLM 在工具行为异常时可先自查，再决定重试、换工具还是请用户处理。
- 选项对话框里的 **MCP Server Statistics 统计页**：按工具列出调用次数、成功/出错、平均耗时与输出量，每秒刷新（只重绘有变化的单元格），并提供两步确认的「清空统计」。

### 改进

- **传输层加固**：并发连接上限 64（超出直接 503）、停机时排空在途请求、单连接读写总时限（防慢速客户端永久占线）、统计刷盘移出请求路径。
- **`exclude` 参数**：schema 收紧为纯字符串数组；同时保留对「字符串化数组」（客户端按纯文本转发参数块的实锤形态）的就地展开兼容，形似数组却解析失败时显式报错而非静默失效；重复反斜杠折叠（`\\target\\` → `\target\`）。
- 统计页渲染改为逐项 diff，空闲时不做无谓的界面刷新。

### 工程化（不影响使用）

- GitHub Actions CI（x86/x64 双架构 build/test/clippy 门禁）与 tag 触发的自动发布流水线。

### 安装

运行对应架构的 `everything-mcp-1.3.0-{x64,x86}-setup.exe`，它会找到 Everything.exe 并以 `-setup-plugin` 参数重新拉起主程序完成安装；卸载在 Everything 选项 → 插件 里一键完成。

---

## English

### Added

- **`server_diagnostics` self-diagnosis tool**: argument-free, read-only. Reports whether the HTTP server is actually listening vs. configured, the current global-search policy, whether the Everything journal log directory exists (first thing to check when `index_changes` returns nothing), the diagnostic log path (`%LOCALAPPDATA%\everything-mcp\plugin.log`), and per-tool call/ok/err statistics — the LLM can triage misbehaving tools on its own before retrying, switching tools, or escalating to the user.
- **MCP Server Statistics page** in the Everything options dialog: per-tool calls, ok/err, average latency and output bytes, refreshed every second (only changed cells are redrawn), with a two-step-confirm "Clear Statistics" button.

### Improved

- **Transport hardening**: 64-connection concurrency cap (excess gets 503), graceful drain of in-flight requests on shutdown, per-connection read/write deadline against slow-loris-style clients, and stats flushing moved out of the request path.
- **`exclude` argument**: schema tightened to a pure string array; stringified-array inputs (the real-world artifact of clients forwarding parameter blocks as plain text) are still expanded in place, while bracket-shaped-but-unparseable input now errors explicitly instead of silently matching nothing; doubled backslashes are collapsed (`\\target\\` → `\target\`).
- Statistics page now redraws via per-item diffing — no busy-loop UI churn when idle.

### Engineering (no user-facing impact)

- GitHub Actions CI (x86/x64 build, test and clippy gate) plus a tag-triggered release pipeline.

### Install

Run the setup exe for your architecture (`everything-mcp-1.3.0-{x64,x86}-setup.exe`); it locates Everything.exe and relaunches it with `-setup-plugin` to complete the installation. Uninstall anytime via Everything → Options → Plugins.
