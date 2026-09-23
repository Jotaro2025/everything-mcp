# everything-mcp

[English](README.en.md)

---

一个 Everything 1.5 的进程内插件，把 Everything 的文件搜索能力以 **MCP**（Model
Context Protocol）接口暴露给 LLM（如 Claude Desktop、Cursor 等）使用。

### 特性

- **进程内插件** —— 编译为单个 `everything_mcp.dll`，被 Everything 1.5 通过
  `LoadLibrary` 直接加载，没有外部进程开销。
- **零第三方 DLL 依赖** —— 除 Windows 自带的 `kernel32` / `ws2_32` / `advapi32`
  等外不引入任何额外 DLL。Rust 标准库与 `serde` / `serde_json` / `windows-sys`
  全部静态链接（CRT 也静态链接，见 `.cargo/config.toml`）。
- **文件夹优先** —— LLM 通常在指定项目目录下工作，本插件提供的工具以「按文件夹
  搜索」为主，避免全局扫描带来的噪声。
- **图形设置页** —— Everything 选项对话框内直接配置启用开关、绑定地址、端口，
  改动点「应用」即时生效，无需重启 Everything。
- **MCP Streamable HTTP** —— 默认监听 `127.0.0.1:8285`，单条 JSON-RPC over HTTP。
- **双时代 MCP 协议** —— 同一端点同时服务两代客户端：2024-11-05（`initialize`
  握手）与 2026-07-28（逐请求 `_meta` 版本声明 + `server/discover`）。版本不支持
  时返回 `-32022` 并附可用版本列表，老客户端行为完全不变。
- **入参校验与路径规范化** —— `folder` 在触达 Everything 之前统一去包裹引号、
  正斜杠转反斜杠、折叠重复反斜杠、去尾斜杠；非法路径返回带范例的
  `INVALID_PARAMS`，而不是静默返回 0 结果。
- **索引变更查询** —— `index_changes` 回答「最近变了什么」：文件/文件夹的
  创建、修改、删除、重命名、移动，新的在前，支持按动作、路径前缀、名字
  子串和时间窗过滤。
- **可打包安装** —— 一条命令产出 x64 / x86 两个安装包；安装与卸载都由
  Everything 自己完成（与官方插件同一套机制）。

### 仓库结构

```
everything-mcp/
├── Cargo.toml                  Rust 包定义（cdylib + rlib，静态 CRT）
├── Cargo.lock                  锁定依赖版本（可执行产物，建议入库）
├── everything_mcp.def          DLL 模块定义，只导出 everything_plugin_proc
├── build.rs                    把 .def 传给 MSVC linker
├── .cargo/config.toml          target-feature=+crt-static（零第三方 DLL 的关键）
├── src/
│   ├── lib.rs                  插件入口 everything_plugin_proc 与 PM_* 分发
│   ├── options.rs              Everything 选项页（启用开关/绑定地址/端口/恢复默认）与设置状态机
│   ├── plugin/
│   │   ├── mod.rs              子模块汇总 + PM_* 常量
│   │   ├── diag.rs             磁盘诊断日志（%LOCALAPPDATA%\everything-mcp\plugin.log）
│   │   ├── ffi_types.rs        #[repr(C)] 类型（Utf8Buf/DbHandle/FileInfoFd...）
│   │   ├── host.rs             host 函数指针表 + HOST/HOST_LOCK
│   │   ├── journal.rs          索引日志（index-journal-*.txt）解析与查询
│   │   ├── search.rs           异步搜索 → 同步等待的高层封装（主线程 marshaling）
│   │   ├── main_thread.rs      主线程窗口 + PostMessage 任务分发
│   │   └── state.rs            运行期状态（懒创建的 db/query、关闭标志）
│   └── mcp/
│       ├── mod.rs
│       ├── protocol.rs         JSON-RPC 2.0 + MCP 双时代类型（2024-11-05 / 2026-07-28）
│       ├── server.rs           基于 std::net 的 HTTP 服务，按请求协商协议版本、校验 Origin
│       ├── tools.rs            工具实现（search_in_folder/list_folder/count/index_changes）
│       └── validate.rs         入参校验与路径规范化（folder 规范化、pattern 校验）
├── installer/
│   ├── build-installers.ps1    一键打包脚本（x64 / x86 两个安装包）
│   ├── setup/
│   │   ├── setup.c             安装启动器源码（照抄官方插件做法）
│   │   ├── setup.rc            版本信息 + 内嵌 bz2 插件 dll 的资源脚本
│   │   ├── resource.h          资源 ID（IDR_DLL_BZ2 = 107）
│   │   └── version.h           版本号（打包时从 Cargo.toml 自动生成）
│   └── client-config-example.json  MCP 客户端接入示例
├── tests/
│   └── mcp.rs                  MCP 协议层集成测试（cargo test）
├── docs/
│   └── PLUGIN_SDK_API_CN.md    插件 SDK 中文 API 参考（含实战踩坑记录）
├── reference/                  第三方参考材料（voidtools 官方 C 插件源码，
│                               不参与本 crate 编译，详见 reference/README.md）
└── LICENSE
```

### 构建前置

只需要 Rust 工具链，无需 Visual Studio（rustup 自带的 MSVC 工具链即可）：

```powershell
# 安装 Rust（仅一次）
Invoke-WebRequest https://win.rustup.com/x86_64 -OutFile rustup-init.exe
.\rustup-init.exe -y
# 默认安装 stable-x86_64-pc-windows-msvc 目标。

# 在仓库根目录编译
cargo build --release

# 运行测试（MCP 协议层集成测试，不依赖 Everything 主程序）
cargo test

# 产物位置
ls target\release\everything_mcp.dll
```

**关于「零第三方 DLL」**：

| 依赖              | 来源           | 是否引入 DLL |
| ----------------- | -------------- | ------------ |
| `serde` / `serde_json` | 纯 Rust | ❌ 静态链接进 DLL |
| `windows-sys`     | 仅 FFI 绑定    | ❌ 仅通过 `#[link]` 绑定系统 DLL |
| Rust std（含 alloc） | Rust 自带   | ❌ 静态链接进 DLL |
| MSVC CRT          | `/MT` + `libcmt.lib`（`crt-static`） | ❌ 静态链接进 DLL |

`panic = "abort"` 与 `lto = true` 进一步保证产物里只剩 Everything 主程序需要的
那一个导出符号。

### 部署

把编译出来的 `everything_mcp.dll` 复制为 **`everything_mcp64.dll`**，放进：

```
C:\Program Files\Everything\Plugins\everything_mcp64.dll
```

Everything 1.5 从 `Plugins\` 根目录按 `<name>64.dll` 约定加载 64 位插件（与官方
`etp_server64.dll` / `http_server64.dll` 一致）。下次启动 Everything 即自动加载，
无需注册表或额外配置。

#### 通过安装包部署

打包方式与官方插件一致：`setup.exe` 自身不拷贝任何文件，它只负责定位
Everything，然后用 `-setup-plugin` 把 Everything 拉起来，由 Everything 自己从
exe 资源里解出插件 dll、装进 `Plugins\` 并登记注册信息。

```powershell
cd installer
powershell -ExecutionPolicy Bypass -File build-installers.ps1
```

产物在 `installer\dist\`：

| 安装包                              | 架构 | 内嵌插件 dll           |
| ----------------------------------- | ---- | ---------------------- |
| `everything-mcp-1.1.2-x64-setup.exe` | x64  | `everything_mcp64.dll` |
| `everything-mcp-1.1.2-x86-setup.exe` | x86  | `everything_mcp32.dll` |

安装：运行对应架构的安装包 → Everything 弹出「设置插件」对话框 → 点「安装」。
两个安装包可以一起分发，`Plugins\` 下 `everything_mcp64.dll` 与
`everything_mcp32.dll` 可共存，Everything 按自身位数选用。卸载在 Everything
选项 → 插件 里操作，不会动 Plugins 目录本身。

打包除了 Rust 工具链外还需要（仅编译插件 dll 不需要这些）：

- Visual Studio「使用 C++ 的桌面开发」工作负载（提供 `cl.exe` 与 `rc.exe`，
  脚本通过 VsDevCmd 自动取环境变量）
- Windows SDK（`rc.exe` 编译资源脚本）
- `rustup target add i686-pc-windows-msvc`（脚本检测到缺失会自动添加）
- 7-Zip 或 Python 二选一（把插件 dll 压成 bz2 再嵌进 exe，官方用 7-Zip）

#### 官方插件（Everything 1.5）

Everything 官方插件页：<https://www.voidtools.com/support/everything/plugins/>

以下 voidtools 官方插件同样面向 Everything 1.5，安装方式与本插件相同（安装包
一键安装，或把插件 dll 放进 Plugins 目录后重启 Everything）：

| 插件              | 版本      | 说明                                                                 | 源码                                                |
| ----------------- | --------- | -------------------------------------------------------------------- | --------------------------------------------------- |
| HTTP Server       | 1.0.5.6   | 允许通过浏览器搜索与访问文件                                         | [voidtools/http_server](https://github.com/voidtools/http_server) |
| ETP/FTP Server    | 1.0.2.5   | 允许通过 Everything 或 FTP 客户端搜索与访问文件                       | [voidtools/etp_server](https://github.com/voidtools/etp_server)   |
| Everything Server | 1.0.4.5   | 允许其他 Everything 访问本机索引（需 1.5.0.1408 或更高版本，另需站点许可证） | —                                                   |

官方安装说明：

- **安装包方式**：下载插件安装包（`Setup.exe`）→ 运行 → 点击「Add」。
- **手动方式**：下载插件 zip 并解压出插件 dll → 把 dll 移动到
  `C:\Program Files\Everything\plugins`（即 Everything 安装目录下的 Plugins
  文件夹）→ 在 Everything 的 File 菜单点击 Exit → 重启 Everything。

### 配置

插件在 Everything 设置中读取以下项（缺省使用默认值）：

| 配置项        | 类型   | 默认值        | 说明                          |
| ------------- | ------ | ------------- | ----------------------------- |
| `mcp_enabled` | int    | `0`           | 0 关闭 MCP 服务（默认不启用，需勾选） |
| `mcp_port`    | int    | `8285`        | HTTP 监听端口                 |
| `mcp_bind`    | string | `127.0.0.1`   | 绑定地址（仅本机访问）        |

设置存放于 `%APPDATA%\Everything\Plugins.ini` 的本插件专属小节
`[everything_mcp64.dll]`（32 位系统上是 `[everything_mcp32.dll]`）——
不是 Everything 的 `Settings.ini`，也不与 http_server 等官方插件共用小节。
也可以直接在 Everything 选项对话框的「插件 → MCP」页修改（启用开关、绑定地址、
端口、恢复默认），点「应用」后立即生效，无需重启 Everything。

### MCP 客户端接入

把下面的 `mcpServers` 段合并到 Claude Desktop 的
`claude_desktop_config.json` 或类似配置文件：

```json
{
  "mcpServers": {
    "everything": {
      "url": "http://127.0.0.1:8285/"
    }
  }
}
```

**只需要一个 URL。** 插件端点是双时代的：老的 MCP 客户端走 `initialize` 握手，
2026-07-28 起的客户端跳过握手、直接发请求，两者都能正常接入，无需在客户端侧
指定协议版本。前提是插件已启用（Everything 选项对话框「插件 → MCP」页勾选，
或 `%APPDATA%\Everything\Plugins.ini` 的 `[everything_mcp64.dll]` 小节里
`mcp_enabled=1`）。

接入后 LLM 会自动发现以下四个工具：

#### 1. `search_in_folder`

递归搜索指定文件夹下的文件 / 文件夹（Everything 搜索语法）。

```json
{
  "folder": "D:\\source\\repos\\my-project",
  "pattern": "ext:rs;toml",
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
- `content:` 正文检索需要 Everything 端开启内容索引（默认关闭；
  开启后首次建索引会明显变慢）。权限位已随插件放开，开关在
  Everything 选项里。
- 空字符串 `""` 表示列出该文件夹下所有文件。
- `max_results = 0` 表示无上限（大目录慎用）。
- 响应里 `count` 是实际返回的条数（受 `max_results` 截断），`total` 是
  命中的总条数 —— 两者不等即说明结果被截断了，可以据此调大
  `max_results` 或用 `count` 先摸底。

`exclude` 参数（三个工具都支持）：字符串或字符串数组，每项作为一条
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

#### 2. `list_folder`

列出某个文件夹的直接子项（非递归），返回名字、类型、大小。

```json
{ "folder": "D:\\source\\repos\\my-project", "exclude": ["\\.git\\"] }
```

- 文件夹条目的 `size` 恒为 0 —— Everything 不计算目录大小，这是口径
  而非缺失。要整个目录树的内容，用 `search_in_folder` 递归搜索。
- 响应同样带 `total`（该目录直接子项总数，排除后的数量）。

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

三个工具共用的入参规则：`folder` 必须是绝对路径（`C:\…` 或 `\\server\share\…`）。
带引号、正斜杠、重复反斜杠、尾斜杠的写法会被自动规范化；通配符属于 `pattern`
而不属于 `folder`。规范化失败时返回 `-32602 INVALID_PARAMS`，消息里带期望格式
的范例与收到的原值 —— LLM 据此一次改对，不会带着坏参数反复重试。

`index_changes` 的入参全部可选，校验规则同上：坏 `action`、越界的
`max_results`（1–2000）、格式不对的时间戳、`since` 晚于 `until`，都在
触达文件系统之前返回 `-32602`。

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

### 线程模型与并发

- Everything 主程序假定**单线程**访问其数据库接口，且 `db_query_search2` 有
  主线程亲和性 —— 从插件后台线程调用会在主程序内部崩溃（`0xc0000005`），
  仅加互斥锁不够。
- 本插件因此采用与官方 `etp_server` 相同的主线程 marshaling 方案：
  1. PM_START 时用 host 的 `os_register_class` + `os_create_window` 创建消息窗口
     （必须用 host 函数，自建 Win32 窗口收不到主程序消息泵的投递）；
  2. 后台 HTTP 线程通过 `PostMessage` 把 `db_query_search2` 与结果读取投递到
     该窗口的过程里执行，用 Win32 事件把完成状态交还后台线程。
- `HOST_LOCK`（进程级 `Mutex`）保证所有 host 调用串行执行。
- db 引用与 query 对象**懒创建**于第一次搜索（此时主程序已完整启动）——
  在 PM_START 期间创建会导致 Everything 启动约 1 秒后崩溃。
- `db_query_search2` 的主排序键必须传 `property_get_builtin_type(NAME)` 的
  返回值，传 NULL 同样会崩。详见 `docs/PLUGIN_SDK_API_CN.md` 第 19 节。

### 协议参考

- 插件 SDK：`docs/PLUGIN_SDK_API_CN.md`（完整的中文注释 API 参考）
- MCP：https://spec.modelcontextprotocol.io/
- Everything 1.5：https://www.voidtools.com/

### 参考材料

`reference/` 目录存放 voidtools 官方发布的 C 插件源码（均不参与本 crate
编译），开发本插件时的主线程 marshaling、`db_query_search2`
参数表等实现均对照 `reference/etp_server-1.0.2.5` 的源码。各材料的许可证见
[`reference/README.md`](reference/README.md)。

### 许可证

MIT，见 [LICENSE](LICENSE)。
