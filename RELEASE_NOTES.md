# v1.3.1 更新说明 / Release Notes

## 中文

### 修复

- **修复 Codex（rmcp）旧协议握手失败**：legacy（2024-11-05 或未声明版本）时代收到 `notifications/initialized` 等通知时，原先返回 `200 + {"jsonrpc":"2.0","id":null}`——这不是合法的 JSON-RPC 响应，Codex/rmcp 在 initialized 阶段反序列化失败后直接断开。现在两个协议时代接受通知时统一返回 `202` 空体，modern 时代行为不变；补充回归测试与真实 TCP 端到端握手测试（initialize → initialized → tools/list）。by @yz0812 in [#7](https://github.com/Jotaro2025/everything-mcp/pull/7)
- **修复安装程序手动选择 Everything 目录时崩溃**：注册表和运行中实例都找不到 Everything 时会弹出目录选择框，原先把只读的空字符串常量当作显示名缓冲区传给 `SHBrowseForFolderW`，选完目录即崩溃；且选出的是目录而非可执行文件，`CreateProcessW` 无法启动。现在改用可写缓冲区、只允许选择文件系统目录，自动拼接 `Everything.exe` 并校验路径长度与文件存在性，失败时给出明确报错。by @yz0812 in [#6](https://github.com/Jotaro2025/everything-mcp/pull/6)

### 安装

运行对应架构的 `everything-mcp-1.3.1-{x64,x86}-setup.exe`，它会找到 Everything.exe 并以 `-setup-plugin` 参数重新拉起主程序完成安装；卸载在 Everything 选项 → 插件 里一键完成。

---

## English

### Fixed

- **Codex (rmcp) legacy handshake failure**: in the legacy era (2024-11-05 or no declared version), notifications such as `notifications/initialized` used to get `200 + {"jsonrpc":"2.0","id":null}` — not a valid JSON-RPC response, so Codex/rmcp failed to deserialize it at the initialized step and dropped the connection. Accepted notifications now get a `202` with an empty body in both protocol eras (modern behavior unchanged), covered by a regression test and a real-TCP end-to-end handshake test (initialize → initialized → tools/list). by @yz0812 in [#7](https://github.com/Jotaro2025/everything-mcp/pull/7)
- **Installer crash when manually choosing the Everything folder**: when Everything is found neither in the registry nor as a running instance, setup falls back to a folder picker. It used to hand `SHBrowseForFolderW` a read-only empty string literal as the display-name buffer, crashing right after a folder was picked; and the picked path was a directory, not an executable, so `CreateProcessW` could not launch it. Setup now uses a writable buffer, restricts the picker to file-system folders, appends `Everything.exe`, validates path length and file existence, and reports a clear error on failure. by @yz0812 in [#6](https://github.com/Jotaro2025/everything-mcp/pull/6)

### Install

Run the setup exe for your architecture (`everything-mcp-1.3.1-{x64,x86}-setup.exe`); it locates Everything.exe and relaunches it with `-setup-plugin` to complete the installation. Uninstall anytime via Everything → Options → Plugins.

**Full Changelog**: https://github.com/Jotaro2025/everything-mcp/compare/v1.3.0...v1.3.1
