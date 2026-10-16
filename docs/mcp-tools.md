# MCP 工具与协议详解

本文从主 README 拆出，便于仓库首页保持简洁。返回：[README](../README.md)

## 工具一览（详解）

接入后 LLM 会自动发现以下**八个**工具：

#### 1. `search_in_folder`

递归搜索指定文件夹下的文件 / 文件夹（Everything 搜索语法）。

```json
{
  "folder": "D:\\source\\repos\\my-project",
  "pattern": "ext:rs;toml",
  "sort": "modified",
  "descending": true,
  "max_results": 30,
  "timeout_ms": 10000
}
```

- `pattern` 支持完整 Everything 语法：`*.rs`、`"readme"`、`ext:md;txt`、
  `dm:lastweek`、`size:>1mb`、`content:"fn main"` 等。
- **不是 shell glob**：写 `*.rs`，不要写 `**/*.rs` —— 文件夹范围本身
  已经限定了目录树。开头的 `**/`、`**\`、`./` 会被自动剥掉（评测里
  `**/Program.cs` 得到 0 条就是踩这个坑），中间的 globstar 无法忠实
  翻译，会原样传给 Everything。
- 排除项用 `!` 前缀（如 `ext:rs !test`），或用 `exclude` 参数
  （见下）。
- **不知道文件夹在哪？** 先用 `search_everywhere`（见第 7 节，需在设置里开启）
  按名字全局定位，拿到完整路径再回来做限定范围的搜索。
- `content:` 正文检索**开箱可用** —— 不需要先在 Everything 里建内容索引。
  但候选集不收窄会慢到超时，语法与提速办法见下面的
  [正文检索与提速](#正文检索与提速)。
- 空字符串 `""` 表示列出该文件夹下所有文件。
- **每条结果带 `modified` / `created`**（修改 / 创建时间，ISO 8601 UTC，
  形如 `2026-09-23T11:18:31Z`；索引项没有该时间时为 `null`。Everything
  默认不索引创建时间，所以 `created` 常见为 `null`，`modified` 一般都有）。
- **排序**：`sort` 取 `name`（默认）/ `path` / `size` / `modified` /
  `created`，`descending: true` 反序。找「最近改动的文件」用
  `"sort": "modified", "descending": true`，「最大的文件」用
  `"sort": "size", "descending": true`。
- **匹配开关**：`match_case`（区分大小写）、`match_whole_word`（全字）、
  `match_regex`（正则，内部用 Everything 的 `regex:` 搜索函数实现），
  默认都是 `false`。也可以直接在 pattern 里写 `case:` / `ww:` / `regex:`，
  但 **`case:` 后面不能有空格**：写 `case:content:"x"` 生效，写
  `case: content:"x"` 会被静默忽略、按不区分大小写返回结果。
- **翻页**：`offset` 指定从第几条开始返回（默认 0），`max_results` 是
  1..2000（默认 50，`0` 是写错而不是「不限」）。响应里 `count` 是实际返回
  条数，`total` 是命中总数，`truncated` 直接给出「还有更多」这个判断 —— 把
  `offset` 递增 `count` 再查一批即可翻页。**但优先收窄 `pattern` / `exclude`**：
  每一页都会重新执行一次查询，而单次调用有约 50 ms 的固定成本，翻页等于把这份
  成本乘以页数（实测 4419 条按 500 一页翻约 450 ms，一次拿完只要 88 ms）。
- **响应体另有一道 512 KiB 字节预算**：条数之外还按字节卡，路径特别长时返回
  条数会少于 `max_results` —— 这种情况 `truncated` 同样是 `true`。


`exclude` 参数（`search_in_folder` / `list_folder` / `count` / `grep` / `search_everywhere` 支持）：字符串或字符串数组，每项作为一条
Everything NOT 项拼进搜索词。仓库噪声默认会进结果（Everything 不知道
gitignore），常见做法：

```json
{
  "folder": "D:\\source\\repos\\my-project",
  "pattern": "*.cs",
  "exclude": ["\\obj\\", "\\.git\\", "\\node_modules\\"]
}
```

排除项按字面匹配路径片段，带首尾反斜杠（`\obj\`）才不会误伤名字里
含 `obj` 的文件。等效的 pattern 写法是 `*.cs !\obj\ !\.git\`。

##### 正文检索与提速

`content:` 是 Everything 的正文检索函数。插件已把它需要的
`allow_read_access` 权限位放开，**不需要先在 Everything 里建内容索引**就能
用 —— 没建索引时 Everything 按需打开候选文件读内容，结果是对的，只是慢。
慢的根源是候选集大小，所以提速按这个顺序做。

**1. 先收窄范围（最有效，零配置）。** 用 `ext:` 限后缀、把 `folder` 指到
子目录、用 `exclude` 排掉 `\target\` `\.git\` `\node_modules\`，或用
`dm:lastweek` 只搜最近改过的文件。实测同一个仓库：

| pattern | 结果 |
| --- | --- |
| `content:"db_query_search2"`（仓库根目录，含 `target/`） | 默认 10s 超时 |
| `ext:rs content:"db_query_search2"` | 立刻返回 9 个文件 |

**2. 调大 `timeout_ms`。** 收窄后仍超时就加大（默认 10000，必须是
1..4294967295 的整数 —— 0 和越界值都会被拒，不再静默变形）。

**3. 开内容索引（一劳永逸，但只对建了索引的目录有效）。** 在 Everything 里
走 工具 → 选项 → 索引 → 文件夹，选中目录并勾上「索引文件内容」（1.5 的较新
构建把这些选项挪到了「高级」页，找不到就在选项对话框里搜 `content`）。
对应的 ini 键是 `content_indexing_enabled`、`content_indexing_include_only_files`
和 `content_indexing_max_size`。两个坑：

- **务必把源码后缀加进 `content_indexing_include_only_files`。** 它的默认值
  只有 `*.doc;*.docx;*.pdf;*.txt;*.xls;*.xlsx`，不含 `.rs` / `.cs` / `.py` /
  `.md` —— 不加的话这些文件仍走按需读取，索引白开。
- 建索引时 Everything 要读一遍所有候选文件，**首次会明显变慢**；改完 ini
  要**重启 Everything** 才生效（运行中的实例会在退出时用内存里的旧值把
  ini 盖回去）。

正文检索的可用写法（均在 1.5.0.1422b 实测）：

| 写法 | 含义 |
| --- | --- |
| `content:"fn main"` | 正文含该文本（不区分大小写） |
| `case:content:"Fn Main"` | 区分大小写的正文检索 —— `case:` 后不能有空格 |
| `utf8content:"fn main"` | 按 UTF-8 读正文（代码 / 脚本） |
| `ansicontent:"系统日志"` | 按 ANSI/GBK 读正文（老中文文档） |
| `regex:content:"\d{3}-\d{2}-\d{4}"` | 正则匹配正文 |

`casecontent:"…"` 这种连写形式在 1.5.0.1422b 里**不生效**（恒返回 0 条），
要区分大小写请用 `case:content:`，或直接用工具参数 `match_case: true`。

#### 2. `list_folder`

列出某个文件夹的直接子项（非递归），返回名字、类型、大小、修改 / 创建时间。

```json
{ "folder": "D:\\source\\repos\\my-project", "exclude": ["\\.git\\"] }
```

- 文件夹条目的 `size` 恒为 0 —— Everything 不计算目录大小，这是口径
  而非缺失。要整个目录树的内容，用 `search_in_folder` 递归搜索。
- 每条同样带 `modified` / `created`（ISO 8601 UTC，无该时间时为 `null`）。
- 响应同样带 `total`（该目录直接子项总数，排除后的数量）。
- **大目录要翻页**：`max_results` 默认 500、上限 2000，`offset` 往后走。
  `count` < `total` 时说明还有下一页，`truncated` 直接给出这个判断。繁忙目录
  可以有几千个直接子项（`C:\Windows\System32` 实测约 4900 个），旧版写死
  500 条且没有 `offset`，第 501 项之后拿不到。
- `timeout_ms` 默认 10000，与另外三个搜索类工具一致（旧版这里写死 10 秒，
  挂着一个慢共享时没法放宽）。

#### 3. `count`

统计匹配文件数量，不返回名字列表。只取命中计数，不逐条读名字和路径。
支持与 `search_in_folder` 相同的 `pattern` 与 `exclude`。

```json
{ "folder": "D:\\source\\repos\\my-project", "pattern": "ext:rs" }
```

#### 4. `index_changes`

查询 Everything 的索引日志：哪些文件 / 文件夹被创建、修改、删除、重命名、
移动，**新的在前**。它回答的是「最近变了什么」，而不是「现在有什么」——
要知道当前状态用 `search_in_folder`。

```json
{ "action": "created", "path": "D:\\source\\repos\\my-project", "max_results": 50 }
```

- **需要 Everything 端开启日志记录**：选项 →「索引 → 日志 → 记录变更」
  （或 `%APPDATA%\Everything\Everything.ini` 的 `[Everything]` 段里
  `journal_log=1`，然后重启 Everything）。**没开时这个工具返回带确切
  开关位置的友好错误提示**，而不是静默返回空结果 —— 所以第一次调用前
  要先确认开关已打开。
- **日志写入有延迟，实测约 8–70 秒**：Everything 不是实时把变更写进文本
  日志的，刚刚发生的改动可能还查不到。**空结果不等于「什么都没发生」** ——
  拿到空结果时请稍等一分钟再查一次，或先用 `search_in_folder` /
  `count` 确认文件的当前状态。
- `action` 取 `created` / `modified` / `deleted` / `renamed` / `moved` /
  `any`（默认 `any`）。Everything 区分「重命名」（同一文件夹内）与
  「移动」（换文件夹）；每条结果另带 `action_text` 保存 Everything 原始
  输出的本地化动作串，界面语言看不懂的 locale 也能读懂。
- `path` 是不区分大小写的路径前缀，`name` 是文件 / 文件夹名子串（重命名同样
  匹配新名字）。`since` / `until` 给出时间窗（含端点），接受
  `2026-09-23`、`2026-09-23 11:18`、`2026-09-23 11:18:31`（日期与时间之间
  空格或 `T` 都行），裸数字按 Unix 秒处理。
- 响应带 `count`、`truncated`（还有更多匹配的历史时说明被 `max_results`
  截断了）、`days_searched`（回溯了几天的日志）、`skipped_lines`
  （解析失败的行数，通常就是正在写入的半行）和 `log_directory`（实际读的
  日志目录）。日志按天倒序分块读，「取最近 N 条」只读文件尾部一小段，
  不会把整份日志穿完。
- 深度受 Everything 侧的日志保留策略限制：日志按天一个文件，本工具最多
  回溯 92 天、单文件最多 64 MiB。

#### 5. `read_file`

读**一个**文本文件的内容，可选只返回其中一段行窗口。它是 `search_in_folder`
的搭档：先搜出文件，再读它的内容 —— 检索本身只返回路径与元数据，不返回正文。

```json
{ "path": "D:\\source\\repos\\my-project\\README.md", "start_line": 1, "max_lines": 2000 }
```

- `path` 必须是**绝对路径**且指向**单个已存在的文件**。通配符（`*` / `?`）
  会被拒绝并提示改用 `search_in_folder`，目录会被拒绝并提示改用
  `list_folder`，相对路径直接报错。这些纯入参问题都在读盘之前返回
  `-32602 INVALID_PARAMS`；运行期问题（文件不存在、是二进制、超过大小上限、
  命中黑名单）作为工具错误返回（`isError: true`），错误体是结构化 JSON：
  `{ "error": "…", "code": "NOT_FOUND" }`，**按 `code` 分支**而不是解析文案。
  目录那条还会带 `suggested_tool: "list_folder"` 与 `suggested_args`，省掉
  一轮试错。错误码：`NOT_FOUND` / `PATH_IS_DIRECTORY` / `PATH_DENIED` /
  `BINARY_CONTENT` / `TOO_LARGE` / `READ_FAILED`。
- 🔒 **敏感路径黑名单**：私钥与凭据文件**永不返回** ——
  `id_rsa`/`id_dsa`/`id_ecdsa`/`id_ed25519`、`.env`（`.env.example` /
  `.sample` / `.template` / `.dist` 除外）、`.netrc`、`.git-credentials`、
  `credentials.json`、`*.pem` / `*.key` / `*.p12` / `*.pfx` / `*.jks` /
  `*.keystore` / `*.ppk`，以及 `.git/objects` 整棵树。命中返回
  `PATH_DENIED`，且**在读盘之前就拒**（连文件是否存在都不去碰）。
  **这份名单不可配置** —— 它是这个面向 LLM 的读文件工具里唯一能挡住
  「提示注入 → 读走 `~/.ssh/id_rsa`」的机制。名单只作用于读正文，
  搜索结果不隐藏这些文件：文件名不是秘密，正文才是。
- **单文件上限 8 MiB**，超过直接报错（`TOO_LARGE`）—— 不会把大文件读进内存。
  返回内容另有 **128 KiB 字节预算**兜底，**按整行累加**：预算拦下时只会少返回
  整行，不会从一行中间切开。
- **超长行会被裁短**：超过 16384 字符的行裁到上限，裁了几行由 `clipped_lines`
  上报（压缩过的单行 js / 单行 JSON 因此不会一口吃掉整个窗口）。裁过的行仍
  算一行，`lines_returned` 照常计数。
- **分页**：`start_line` 是 1 起的起始行号（默认 1），`max_lines` 是返回行数
  （默认 **2000**、上限 **4000**）。行数是粗筛，**真正的闸门是那 128 KiB 预算** ——
  两者谁先到就停在谁那里，且永远停在整行边界上。响应直接给 `next_start_line`，
  把上次的 `next_start_line` 原样传回 `start_line` 就是下一页，它是 `null`
  表示读完了，不用自己算。`start_line` 超出末尾返回空窗口而不是报错。
- **响应是两段 content**：第一段是元信息 JSON（`path` / `size` /
  `total_lines` / `start_line` / `lines_returned` / `next_start_line` /
  `truncated` / `clipped_lines` / `encoding`），第二段是正文原文。正文单独
  成项是为了保持原样 —— 塞进 JSON 会把换行转义成 `\n`，读代码时既难读又容易
  在后续引用时出错。`truncated` 只表示「窗口外还有内容」，与 `clipped_lines`
  是两件事。
- **`encoding` 说明正文是怎么解出来的**：`utf-8` / `utf-16le` / `utf-16be`
  是确定的（合法 UTF-8 或带 BOM）；`ansi` 表示既不是合法 UTF-8 也没有 BOM，
  于是按**本机 ANSI 代码页**解（中文 Windows 即 GBK —— 老文档、老日志常见）；
  `utf-8-lossy` 表示有字节解不出来、已替换成 `\uFFFD`。看到 `ansi` 或
  `utf-8-lossy` 时正文可能是猜的，引用前留意一下。
- **二进制文件会被拒绝**，两道独立检查：**扩展名**（文档、压缩包、可执行、媒体、
  字体、数据库 —— `.pdf`/`.docx`/`.xlsx`/`.zip`/`.exe`/`.dll`/`.png`/`.mp4`/`.ttf`
  /`.sqlite` 等）与**内容嗅探**（前 4 KiB 内有 NUL，或非可打印字节超过 30%）。
  文本格式（`.json`/`.xml`/`.svg`/`.log`/`.csv`/`.md`/`.dat`）一个都不在扩展名表里，
  不会被误拦；带 BOM 的 UTF-16 豁免内容嗅探（它的原始字节本来就全是 NUL）。
  这两道是补出来的：原先只有"含 NUL"一条，实测一个 37 字节、全 ASCII 的小 PDF
  会被当正文返回。
- `truncated` 表示**窗口外还有内容**（配套字段是 `next_start_line`），不是"被从中间
  截断"的信号 —— 后者看 `clipped_lines`。两者是独立的。
- 想按内容找文件请用 `search_in_folder` 的 `content:"…"`（见上面的
  [正文检索与提速](#正文检索与提速)）；`read_file` 只读你指定的那一个文件。
- ⚠️ 除上面那份黑名单外，本工具能读 Everything 进程有权限读的**任意**绝对
  路径，不限于已索引的文件。服务默认只监听 `127.0.0.1`（见「配置」），请勿
  把它暴露到网络上。

#### 6. `grep`

按正则逐行搜文件内容，返回**命中行与行号**。它是 `search_in_folder` 的行级
搭档：后者用 Everything 索引回答「哪些文件的正文里有这个词」（只到文件粒度，
给路径），`grep` 把候选文件读进来逐行匹配，告诉你在第几行。

```json
{
  "folder": "D:\\source\\repos\\my-project",
  "pattern": "fn\\s+main",
  "filter": "ext:rs",
  "output_mode": "content",
  "head_limit": 200
}
```

- **`pattern` 是正则，`filter` 是 Everything 语法** —— 这两个最容易混。
  `pattern` 逐行匹配（每行独立，所以 `^` / `$` 锚的是行首行尾；含字面换行的
  模式永远匹配不到）；`filter` 是交给 Everything 的候选筛选串，语法与
  `search_in_folder` 的 `pattern` 完全一样（`ext:rs;toml`、`dm:lastweek`、
  `!\target\`），**在任何文件被读取之前生效**。
- **一定要给 `filter`。** 不给的话 `folder` 下每个文件都会被读一遍 —— 这跟
  不限范围的 `content:` 检索是同一个坑。三道内部闸门兜底：候选文件上限
  2000 个、读取总量上限 64 MiB、**返回的命中正文累计 128 KiB**（与 `read_file`
  同一个常量）；触顶时 `truncated` 为 `true`，而 `candidates` / `files_scanned` /
  `bytes_scanned` 会告诉你卡在哪一道上。注意这 128 KiB 算的是**正文**，JSON
  信封（路径、行号、转义）还会再叠一层，所以实际响应会略大于它。
- **`output_mode`** 决定返回形状：
  - `content`（默认）：`matches` 数组，每项 `{ path, line, text }`；
  - `filesWithMatches`：`files` 数组，只给有命中的文件路径；
  - `count`：`counts` 数组，每项 `{ path, count }`。
- `head_limit` 限制命中条数（`content`）或文件数（其余模式），默认 200、
  上限 2000。命中行超过 16384 字符会被裁短，裁了几行由 `clipped_lines` 上报。
- 结果是**按文件修改时间倒序**的：最近改过的文件先出 —— 刚动过的代码最可能
  是你要找的。
- **二进制文件、超过 8 MiB 的文件、命中敏感路径黑名单的文件会被静默跳过**
  （不报错、也不中断整次搜索）。黑名单在 grep 里同样生效：`.env` / 私钥的
  正文绝不会出现在结果里。
- `case_insensitive` 默认 `false`（区分大小写）；`exclude` 与其它工具同义。

#### 7. `search_everywhere`

按**文件名**在整个 Everything 索引里搜索 —— 所有本地磁盘与已索引的网络共享，
不需要指定文件夹。它是「知道文件叫什么、但不知道它在哪个共享里」这个场景的
答案（否则 agent 只能 `net view \\NAS` 列共享、逐个试到命中）。返回**完整
路径**，可以接着交给 `search_in_folder` / `list_folder` 做限定范围的后续操作。

```json
{
  "pattern": "*.vhd",
  "exclude": ["\\\\old\\\\"],
  "sort": "modified",
  "descending": true,
  "max_results": 50
}
```

- **默认关闭，按三档全局搜索开关管控**（Everything 选项 → 插件 → MCP →
  全局搜索）：
  - **拒绝**（默认）：调用一律返回 `GLOBAL_SEARCH_DISABLED`（服务端硬闸门，
    立即生效），其余七个工具照常（文件夹范围搜索 / 列表 / 计数 / 变更 / 读文件 / grep / 自诊断）。错误载荷里带开启方法，
    LLM 可以直接转告用户。
  - **审核**：调用放行，但工具被标注为「非只读 / 破坏性」（`destructiveHint:
    true`），客户端据此先弹权限确认框。注意注解在规范里是**不可信的提示** ——
    2026-07-28 规范原文：*clients MUST consider tool annotations to be untrusted
    unless they come from trusted servers*。所以认真读注解的客户端会先问一句，
    忽略注解的客户端会直接放行：**服务端真正强制的只有「拒绝」这一档**。无论
    是否放行，磁盘都不会被改动 —— 这个标注只用来驱动确认弹窗。
  - **允许**：标注为只读（`readOnlyHint: true`），调用直接放行，不再打扰用户。
  三档只改变工具注解与描述里的 POLICY 段，工具清单形状不变。注意 `tools/list`
  结果带 5 分钟缓存，切档后注解最多滞后一个 TTL，但「拒绝」档的硬闸在调用时
  检查，立即生效。
- **两道护栏**：`pattern` 至少 2 个字符（全局空 / 单字符 pattern 的命中量是
  灾难级的），且 **`pattern` 与 `exclude` 都禁止 `content:`** —— 否定的正文项
  照样要 Everything 逐文件评估内容，所以两边都得查；正文检索永远留给带 folder
  的 `search_in_folder`。两道都在触达 Everything 之前返回 `-32602
  INVALID_PARAMS`。注意「≥2 字符」只挡空 / 单字符，**不是范围控制**：`*.`、
  `a*`、`dm:thisyear` 都合法且命中量巨大 —— 真正的范围控制是档位闸门与
  `max_results` 上限（2000），`offset` 可以继续往后翻。
- 入参与 `search_in_folder` 同构：`exclude` / `sort` / `descending` /
  `match_case` / `match_whole_word` / `match_regex` / `offset` / `timeout_ms`
  语义一致；`max_results` 也与另外两个搜索工具同一套硬窗口（1..2000），
  `0` 或越界直接 `-32602`，不是「不限」。
- 结果每条带 `name` / `path` / `kind` / `size` / `modified` / `created`
  （时间戳为 ISO 8601 UTC），同样有 `count` / `total` 配对，用 `offset` 翻页。

#### 8. `server_diagnostics`

服务自诊断：零参数、只读，不触达 Everything 数据库。聚合当前配置与状态，让
LLM 在工具行为异常时先自查一轮，再决定是重试、换工具还是请用户去设置页处理：

- `server`：插件版本（与 serverInfo 同源）、支持的协议版本、**服务是否真的在
  监听**（`listening` / `bound_port`）与配置值（`enabled` / `bind` /
  `configured_port`）——`listening: false` 说明服务从未启用或 Apply 失败
  （比如端口被占）。
- `global_search`：`search_everywhere` 的当前档位与一句话说明（拒绝档带开启
  指引）。
- `index_changes_prerequisite`：journal 日志目录路径与**是否存在** —— 不存在
  基本就是 `journal_log` 没开，这正是 `index_changes` 空结果最常见的原因。
- `diagnostic_log`：`%LOCALAPPDATA%\everything-mcp\plugin.log` 的路径，排查
  「插件为什么没起来」的第一入口（见「调试」一节）。
- `stats`：终身计数 —— 总调用、请求数、连接数，以及每个工具的
  `calls` / `ok` / `err` / `avg_ms`。某个工具一直 `err` 是很强的线索。

带 `folder` 参数的工具共用的入参规则：`folder` 必须是绝对路径（`C:\…` 或 `\\server\share\…`），
UNC 网络共享与本地盘同权。搜索范围以 Everything 的索引为准 —— 本地盘自动全收，
网络共享要先在 Everything 的「工具 → 选项 → 索引 → 文件夹」里添加；没加进索引的
共享不会出现在任何结果里（返回 0 条结果，而不是报错）。
带引号、正斜杠、重复反斜杠、尾斜杠的写法会被自动规范化；通配符属于 `pattern`
而不属于 `folder`。规范化失败时返回 `-32602 INVALID_PARAMS`，消息里带期望格式
的范例与收到的原值 —— LLM 据此一次改对，不会带着坏参数反复重试。

`exclude` 的每一项同样会折叠重复反斜杠（`\\target\\` → `\target\`）。这不是
洁癖：1.1.4 之前该写法会**静默失效** —— 实测同一查询不排除 30 条、
`\target\` 22 条、`\\target\\` 又是 30 条，一条都没排除且不报错。折叠对 UNC
排除项也安全（`!\\NAS\old\`），因为 Everything 对完整路径做子串匹配，
`\NAS\old\` 与 `\\NAS\old\` 命中同一批文件。`pattern` 不做这个折叠：它在
`match_regex` 下可能是正则，把 `\\.` 折成 `\.` 会改语义。

`index_changes` 的入参全部可选，校验规则同上：坏 `action`、越界的
`max_results`（1–2000）、格式不对的时间戳、`since` 晚于 `until`，都在
触达文件系统之前返回 `-32602`。所有数值窗口的边界都写进了 JSON Schema 的
`minimum` / `maximum`，客户端据此提前拦下越界值，不必等打到服务端才报错。

**空结果会被解释**：`search_in_folder` / `list_folder` / `count` / `grep` 在
「一条都没匹配」时会顺手探一下目录，路径不存在、不可访问、或根本不是目录时，
响应里多一个 `folder_warning` 说明原因 —— 于是「目录确实是空的」与「路径写错了」
不再长得一模一样。**这是软信号而不是报错**，这点很关键：网络共享没加进索引、
或共享当前离线时本来就该返回 0 条，那是合法用法（NAS 场景正是它），改成报错会把
它们误判成参数错误。探测只在结果为空时才做，正常路径上不付任何代价；反过来，
对离线共享的那次 stat 可能阻塞到 SMB 超时，所以代价只落在「反正什么都没搜到」
的调用上。

#### 手动验证（curl）

不接客户端，先用 curl 确认服务活着。两种时代的请求各发一次：

```powershell
# legacy：initialize 握手（老客户端流程）
curl -X POST http://127.0.0.1:8285/ -H "Content-Type: application/json" -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}"

# modern：不握手，直接 server/discover（版本同时写在请求头和 _meta 里）
curl -X POST http://127.0.0.1:8285/ -H "Content-Type: application/json" -H "MCP-Protocol-Version: 2026-07-28" -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"server/discover\",\"params\":{\"_meta\":{\"io.modelcontextprotocol/protocolVersion\":\"2026-07-28\"}}}"

# modern：tools/list（响应 result 里带 "resultType": "complete" 与缓存提示 ttlMs / cacheScope）
curl -X POST http://127.0.0.1:8285/ -H "Content-Type: application/json" -H "MCP-Protocol-Version: 2026-07-28" -d "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{\"_meta\":{\"io.modelcontextprotocol/protocolVersion\":\"2026-07-28\"}}}"
```

第二条返回的 `supportedVersions` 就是本端点支持的协议版本列表。

#### 接入排错

| HTTP 状态 | JSON-RPC 错误码 | 含义与处理 |
| --- | --- | --- |
| 200 | — | 正常响应 |
| 202 | — | modern 通知已接受（规范要求空响应体） |
| 400 | `-32020` | `MCP-Protocol-Version` 头与 `_meta` 里的版本不一致 —— 两侧改成同值 |
| 400 | `-32022` | 请求的协议版本不支持 —— 按 `data.supported` 列出的版本重试 |
| 403 | `-32600` | 请求带了非 localhost 的 `Origin` 头（浏览器页面被挡）—— 改用本机客户端访问 |
| 404 | `-32601` | modern 时代未知方法 —— 检查方法名拼写 |
| 200 | `-32602` | 工具参数不合法 —— 消息里带期望格式范例与收到的原值，按提示修正后重试 |
| 400/405 | — | 见「调试」一节：请求行不是 POST 到 `/` 或 `/mcp` 时返回 405 |

服务完全无响应时，先确认 `mcp_enabled=1` 且端口没被占用（见「调试」）。

### 协议版本（双时代）

同一个端点按请求声明的协议版本走两套语义，两代客户端都能用：

| 客户端形态             | 版本声明方式                                                       | 行为                                                                 |
| ---------------------- | ------------------------------------------------------------------ | -------------------------------------------------------------------- |
| legacy（2024-11-05）   | `initialize` 握手，或不带任何版本信息                              | 200 + JSON-RPC 响应；未知方法 200 + `-32601`；通知回 `id: null` 空响应 |
| modern（2026-07-28）   | `MCP-Protocol-Version` 请求头 + `_meta` 里的 `io.modelcontextprotocol/protocolVersion` | `server/discover` 可用；未知方法 404；通知 202 空体；头/体不一致 `-32020`；版本不支持 400 + `-32022`（`data.supported` 列出可用版本） |

**`resultType`（2026-07-28）**：modern 时代的每个响应 result 都带
`resultType: "complete"`（该修订版起规范 MUST，缺失会被客户端判为无效结果并
告警）；legacy 响应保持 2024-11-05 原形状，客户端按 absent-means-complete
规则把缺失当作 complete 处理。

**`ttlMs` / `cacheScope`（2026-07-28）**：`ListToolsResult` 与
`DiscoverResult` 继承 `CacheableResult`，modern 时代的这两个结果除
`resultType` 外还必须带 `ttlMs`（客户端可缓存的毫秒数，语义类比 HTTP
`Cache-Control: max-age`；tools/list 取 5 分钟，discover 取 1 小时）和
`cacheScope: "public"`（结果不含用户特定数据，可跨授权上下文缓存）；
legacy 响应保持原形状，不带这两个字段。

其他安全与兼容规则：带 `Origin` 且非 localhost 来源的请求一律 403（防 DNS
rebinding）；`Mcp-Method` / `Mcp-Name` 镜像头存在时校验与请求体一致，非 ASCII
值走 `=?base64?…?=` sentinel；`tools/call` 的参数校验（路径规范化）发生在触达
Everything 主程序之前，因此非法入参绝不会变成一次真实搜索。

**绑定与凭据边界（请如实评估）**：设置页允许把 `mcp_bind` 改成 `0.0.0.0`（监听所有网卡），但**当前没有鉴权**（无 token / 密码）——只有浏览器带的 `Origin` 校验能挡 DNS rebinding，局域网内的普通 HTTP 客户端仍可直接调用工具。默认请保持 `127.0.0.1`。`read_file` / `grep` 的敏感路径黑名单有意偏窄（拦私钥、`.env`、常见密钥库、`.git\objects`），**不**拦 `~/.aws/credentials`、`.npmrc`、`.docker/config.json` 这类「可能含 token 的配置文件」；除黑名单外，工具可读 Everything 进程有权读的任意路径。

### 已知限制

集中列出各工具文档里散落的行为边界，接入前值得一读：

- **搜索结果的上限是 Everything 的索引**。只搜已索引的位置：所有本地 NTFS
  盘 + 在 Everything 选项 → 索引 → 文件夹 里添加的网络共享。未索引的共享
  （或当前离线的共享）返回 0 条 —— 工具会在结果为空时附 `folder_warning`
  帮你区分「真的没有」与「没被索引」。
- **`index_changes` 依赖 Everything 的变更日志**。需要先在 Everything 选项里
  开启 journal_log（索引 → 变更日志），否则日志不存在，只能返回空结果。
- **`search_everywhere` 的 review 档位依赖客户端自觉**。服务端只下发工具注解
  （`readOnlyHint` / `destructiveHint`），confirm 弹窗由 MCP 客户端实现；只有
  deny 档在服务端硬拒。服务端无法强制客户端弹确认 —— 这是 MCP 注解机制的
  固有边界。
- **传输是简单的一次一请求 HTTP**：每连接单请求（`Connection: close`）、无
  SSE 长连接通道、无 GET 通知流（405）。并发上限 64，超出直接 503。单机
  回环 + 串行调用的 MCP 客户端足够；不要把它当高并发 Web 服务用。
- **HTTP 服务器默认只服务本机**。绑定地址默认回环；带非 localhost `Origin`
  头的请求一律 403（防 DNS rebinding）。若你主动绑到 `0.0.0.0`，局域网可达且
  **无鉴权**——见上文「绑定与凭据边界」。远程公开暴露不在设计目标内。
- **只处理文本**。`read_file` 拒绝二进制内容（按扩展名与内容双重嗅探），
  行长超 16 KiB 截断；`grep` 同样跳过二进制文件与超过 8 MiB 的文件。

### 调试

诊断信息有两条出路：

1. **主程序调试输出** —— 通过 host 的 `debug_printf` 输出，可用
   [DebugView](https://learn.microsoft.com/sysinternals/downloads/debugview) 实时查看。
2. **磁盘日志** —— `%LOCALAPPDATA%\everything-mcp\plugin.log`，记录 PM_* 生命周期、
   host 函数解析明细、每次搜索的提交/等待/读取过程。排查崩溃时这个文件最直接。

如果 MCP 服务无响应，先用 curl 验证：

```powershell
curl -X POST http://127.0.0.1:8285/ -H "Content-Type: application/json" -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}"
```
