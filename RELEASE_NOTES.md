# v1.4.1 更新说明 / Release Notes

## 中文

### 修复

- **「启用内容索引」开关勾选后没生效。** 旧实现先把 ini 写成新值、再调官方 `-add-config` 合并进运行中实例；转发进程退出时会用它自己那份状态覆盖 ini，把刚写好的键改回去。实机复现：安装插件重启 Everything 后 23 秒内勾选，ini 先被写成 1、同一秒又被覆盖回 0，而插件只看到「命令发出去了」就报了成功。现在只发 `-add-config`（它自己会落盘），并按回读结果校验：不一致就补写 ini 兜底、退避重试，三次仍不生效则如实报错并保留 Apply 按钮供重试。
- **设置页里「需重启生效」的提示是错的。** `-add-config` 是即时生效的，两处提示已删除。
- **设置页工具提示写错了 grep 与内容索引的关系。** 原文称「grep 与 content: 搜索才可用」，实际 grep 自己读文件、完全不依赖内容索引（关闭索引后实测照样命中）。

### 新增

- **grep 新增 `binary_skipped` 计数。** 按文档格式过滤（如 `filter: ext:pdf`）时 grep 一个文件都读不了，旧版只返回一个没有任何线索的 0 结果。现在会回报因二进制（扩展名硬表或内容嗅探）被跳过的候选数，调用方据此能判断是「没有匹配」还是「这个格式搜不了」。
- **设置页只读展示 `content_indexing_include_only_files`。** 在内容索引开关下方直接显示 Everything 当前索引了哪些文件类型，让「content: 为什么搜不到我的代码」在页面上就能自答。该行**不提供编辑**：改动会影响全局内容索引的体积与 CPU 开销，请到 Everything 高级页修改。
- **`server_diagnostics` 回显索引覆盖范围。** `content_indexing` 块新增 `formats` / `formats_setting` 两个只读字段。

### 工具说明

- 讲清正文检索的四层分工：`content:"..."` 查内容索引（只覆盖白名单里的格式，是进入 PDF/DOCX 的唯一入口）；`utf8content:"..."` / `ansicontent:"..."` 按需读取，任何文本文件都能搜但只返回路径、明显更慢；`grep` 给行号与正则、不依赖索引，是代码搜索首选；`read_file` 读不了文档，已指向 `content:`。
- 设置页的格式行改为自动换行（原来用 `SS_LEFTNOWORDWRAP`，右边被裁掉），并预留两行高度。

### 安装

运行对应架构的 `everything-mcp-1.4.1-{x64,x86}-setup.exe`，它会找到 Everything.exe 并以 `-setup-plugin` 参数重新拉起主程序完成安装；卸载在 Everything 选项 → 插件 里一键完成。

---

## English

### Fixes

- **The "Enable content indexing" switch did not take effect.** The old code wrote the new value to the ini first and then called the official `-add-config` to merge it into the running instance; the forwarder process overwrote the ini from its own state on exit, reverting the key that had just been written. Reproduced live: ticking the box within 23 seconds of the post-install restart left the ini at 0 within the same second while the plugin reported success because it only checked that the command had been spawned. It now sends `-add-config` alone (which persists the value itself) and verifies by reading back: a mismatch falls back to writing the ini, then retries with backoff; after three attempts it reports the failure and leaves Apply enabled.
- **The "a restart may be required" hint was wrong.** `-add-config` applies immediately; both hints are gone.
- **The settings-page tooltip misstated how grep relates to content indexing.** It claimed grep needed the content index; grep reads the files itself and works with indexing off (verified).

### New

- **`binary_skipped` in the grep response.** When filtering by document format (e.g. `filter: ext:pdf`) grep cannot read a single file, and the old response was a bare zero with no explanation. The count of candidates dropped as binary (extension table or content sniff) now comes back with the result, so a caller can tell "no match" from "this format is unreadable".
- **Read-only display of `content_indexing_include_only_files`** on the settings page, right under the content-indexing switch, so "why can't content: find my code" answers itself. The line is deliberately **not editable** — changing it affects the size and CPU cost of the global content index; edit it in Everything's Advanced page.
- **`server_diagnostics` now echoes the indexed formats** via two new read-only fields, `formats` and `formats_setting`.

### Tool descriptions

- Spell out the four-way split: `content:"..."` queries the content index (only the extensions in the include list; the only way into PDF/DOCX); `utf8content:"..."` / `ansicontent:"..."` read on demand, work on any text file but return paths only and are much slower; `grep` gives line numbers and regex without needing an index and is the default for code; `read_file` cannot read documents and now points at `content:`.
- The formats line on the settings page now wraps (it used `SS_LEFTNOWORDWRAP` and was clipped on the right) and reserves two lines.

### Install

Run the setup exe for your architecture (`everything-mcp-1.4.1-{x64,x86}-setup.exe`); it locates Everything.exe and relaunches it with `-setup-plugin` to complete the installation. Uninstall anytime via Everything → Options → Plugins.
