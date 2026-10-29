# v1.4.1 更新说明 / Release Notes

## 中文

1.4.1 是一次修复与打磨版本：修掉内容索引开关勾选后不生效的竞态，让 grep 在按文档格式过滤时不再给出无声的 0 结果，并把正文检索的三种写法（`content:` / `utf8content:` / `grep`）讲清楚。同时把 grep 的每文件句柄打开次数从 3 次降到 1 次、工具清单体积压掉 12%。无破坏性变更，现有设置与端口配置原样保留。

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

### 优化

- **grep 每个候选文件从 3 次句柄打开降到 1 次。** 旧实现为同一个文件依次做 `metadata`、`fs::read` 和黑名单复核，三次都会 open；冷缓存下每次 open 都可能触发安全软件扫描，实测是 grep 在大目录上变慢的主因（1165 个小文件 5.3 MB：热缓存 0.30 s，首次访问 13.1 s）。现在 metadata、句柄级黑名单复核和正文读取共用一个句柄，而且复核移到读取之前，顺带消掉了「复核完到读取之间被换成短名」的 TOCTOU 窗口。
- **`tools/list` 体积从 22.8 KB 压到 20.0 KB。** 八个工具的描述重新措辞，删掉重复叙述与参数级细节；同时加了一条回归测试，涨回 20 KiB 就会失败。
- **内容索引开关的单次等待上限 5 s → 1 s。** 整个应用过程跑在 Everything 的 UI 线程上，最坏冻结从约 16 s 降到约 3.8 s。
- **搜索超时的错误信息补上出路**：现在会提示「收窄范围或调大 timeout_ms」，并点明 `content:` / `utf8content:` 在大目录上远超 10 秒默认值。
- **`server_diagnostics` 的每工具统计增加 `total_ms`。** `avg_ms` 是整数，历史累计越大反推单次耗时越糊；两个一起给才能算出最近几次的真实耗时。
- **`installer/setup/version.h` 不再入库**（改由 `build-installers.ps1` 从 `Cargo.toml` 生成）。它以生成文件身份入库，1.4.0 发版时就漏更新、停在 1.3.1.0。
- **README 修正 `content:` 的说明**：三者分工（`content:` 查索引、`utf8content:`/`ansicontent:` 按需读、`grep` 自带行号）写清了，删掉「必须把源码后缀加进索引白名单」的绝对说法，并把「改完 ini 必须重启」改成「可用 `-add-config` 立即生效」。

### 安装

运行对应架构的 `everything-mcp-1.4.1-{x64,x86}-setup.exe`，它会找到 Everything.exe 并以 `-setup-plugin` 参数重新拉起主程序完成安装；卸载在 Everything 选项 → 插件 里一键完成。

---

## English

1.4.1 is a fix-and-polish release: it repairs the race that made the content-indexing switch silently fail, stops grep from returning an unexplained zero when filtering by document format, and documents the three content-search spellings (`content:` / `utf8content:` / `grep`) properly. It also cuts grep's per-file handle opens from three to one and trims the tool manifest by 12%. No breaking changes — existing settings and port configuration carry over.

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

### Performance and housekeeping

- **grep now opens each candidate file once instead of three times.** The old path did `metadata`, `fs::read` and the denylist recheck separately, each opening the file; on a cold cache every open can trigger a security-software scan, which measured as the dominant cost on large directories (1,165 small files / 5.3 MB: 0.30 s warm, 13.1 s on first access). Metadata, the handle-level denylist recheck and the read now share one handle, and the recheck moved before the read, which also removes the TOCTOU window where the file could be swapped for a short name between check and read.
- **`tools/list` shrank from 22.8 KB to 20.0 KB.** All eight descriptions were reworded to drop duplicated narrative and parameter-level detail; a regression test now fails if it grows past 20 KiB.
- **The content-indexing switch waits 1 s per attempt instead of 5 s.** The whole apply runs on Everything's UI thread, so the worst-case freeze drops from about 16 s to about 3.8 s.
- **The search-timeout error now says what to do**: narrow the scope or raise `timeout_ms`, noting that `content:` / `utf8content:` over a large tree need far more than the 10 s default.
- **`server_diagnostics` per-tool stats gained `total_ms`.** `avg_ms` is an integer, so the larger the history the blurrier the per-call estimate; both together let you measure recent calls accurately.
- **`installer/setup/version.h` is no longer tracked** (it is generated from `Cargo.toml` by `build-installers.ps1`). It was committed as a generated file and was missed in the 1.4.0 release, left at 1.3.1.0.
- **README corrected on `content:`**: the three spellings are now distinguished (`content:` reads the index, `utf8content:`/`ansicontent:` read on demand, `grep` gives line numbers), the absolute "you must add your source extensions to the index list" advice is gone, and "an ini edit needs a restart" now mentions `-add-config` for immediate effect.

### Install

Run the setup exe for your architecture (`everything-mcp-1.4.1-{x64,x86}-setup.exe`); it locates Everything.exe and relaunches it with `-setup-plugin` to complete the installation. Uninstall anytime via Everything → Options → Plugins.
