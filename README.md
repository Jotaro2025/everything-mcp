# everything-mcp

[中文](#中文) | [English](#english)

---

<a id="中文"></a>

## 中文

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
- **可打包安装** —— 提供 NSIS 脚本，一键生成 setup.exe。

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
│   │   ├── search.rs           异步搜索 → 同步等待的高层封装（主线程 marshaling）
│   │   ├── main_thread.rs      主线程窗口 + PostMessage 任务分发
│   │   └── state.rs            运行期状态（懒创建的 db/query、关闭标志）
│   └── mcp/
│       ├── mod.rs
│       ├── protocol.rs         JSON-RPC 2.0 + MCP 类型
│       ├── server.rs           基于 std::net 的 HTTP 服务
│       └── tools.rs            工具实现（search_in_folder/list_folder/count）
├── installer/
│   ├── everything-mcp.nsi      NSIS 安装脚本
│   └── client-config-example.json  MCP 客户端接入示例
├── tests/
│   └── mcp.rs                  MCP 协议层集成测试（cargo test）
├── docs/
│   └── PLUGIN_SDK_API_CN.md    插件 SDK 中文 API 参考（含实战踩坑记录）
├── reference/                  第三方参考材料（voidtools 官方 C 插件与 SDK，
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

```powershell
cd installer
mkdir bin
copy ..\target\release\everything_mcp.dll bin\everything_mcp64.dll
# 需要 NSIS：https://nsis.sourceforge.io/
makensis everything-mcp.nsi
# 生成 everything-mcp-1.0.0-setup.exe
```

安装包默认装到 `C:\Program Files\Everything\Plugins\`，只放置
`everything_mcp64.dll` 与随附文档；卸载时也只删除这些文件（不动 Plugins 目录本身）。

### 配置

插件在 Everything 设置中读取以下项（缺省使用默认值）：

| 配置项        | 类型   | 默认值        | 说明                          |
| ------------- | ------ | ------------- | ----------------------------- |
| `mcp_enabled` | int    | `0`           | 0 关闭 MCP 服务（默认不启用，需勾选） |
| `mcp_port`    | int    | `8285`        | HTTP 监听端口                 |
| `mcp_bind`    | string | `127.0.0.1`   | 绑定地址（仅本机访问）        |

设置存放于 Everything 自带的 `Settings.ini`，与 http_server 等官方插件同节。
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

接入后 LLM 会自动发现以下三个工具：

#### 1. `search_in_folder`

在指定文件夹下按 Everything 搜索语法查找文件 / 文件夹。

```json
{
  "folder": "D:\\source\\repos\\my-project",
  "pattern": "ext:rs;toml",
  "max_results": 30,
  "timeout_ms": 10000
}
```

- `pattern` 支持完整 Everything 语法：`*.rs`、`"readme"`、`ext:md;txt`、
  `dm:lastweek`、`size:>1mb` 等。
- 空字符串 `""` 表示列出该文件夹下所有文件。
- `max_results = 0` 表示无上限（大目录慎用）。

#### 2. `list_folder`

列出某个文件夹的直接子项（非递归），返回名字、类型、大小。

```json
{ "folder": "D:\\source\\repos\\my-project" }
```

#### 3. `count`

统计匹配文件数量，不返回名字列表。

```json
{ "folder": "D:\\source\\repos\\my-project", "pattern": "ext:rs" }
```

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

`reference/` 目录存放 voidtools 官方发布的 C 插件源码与 Everything 3.0 SDK
（均不参与本 crate 编译），开发本插件时的主线程 marshaling、`db_query_search2`
参数表等实现均对照 `reference/etp_server-1.0.2.5` 的源码。各材料的许可证见
[`reference/README.md`](reference/README.md)。

### 许可证

MIT，见 [LICENSE](LICENSE)。

---

<a id="english"></a>

## English

An in-process plugin for Everything 1.5 that exposes Everything's file search as
an **MCP (Model Context Protocol)** server for LLMs (Claude Desktop, Cursor, …).

### Features

- **In-process plugin** — builds to a single `everything_mcp.dll` that
  Everything 1.5 loads directly via `LoadLibrary`; no external process overhead.
- **Zero third-party DLL dependencies** — apart from Windows system libraries
  (`kernel32` / `ws2_32` / `advapi32`, …), nothing else is imported. Rust std,
  `serde` / `serde_json` and `windows-sys` are all statically linked, and so is
  the CRT (see `.cargo/config.toml`).
- **Folder-first** — LLMs usually work inside a specific project directory, so
  the tools are folder-scoped by design instead of scanning the whole disk.
- **Settings page** — enable switch, bind address and port are configurable in
  Everything's own options dialog; clicking Apply takes effect immediately,
  without restarting Everything.
- **MCP Streamable HTTP** — listens on `127.0.0.1:8285` by default, one
  JSON-RPC message over HTTP per request.
- **Installable** — ships an NSIS script that produces a setup.exe in one step.

### Repository layout

```
everything-mcp/
├── Cargo.toml                  Rust package manifest (cdylib + rlib, static CRT)
├── Cargo.lock                   Locked dependency versions (binary output: keep it)
├── everything_mcp.def          DLL module definition, exports only everything_plugin_proc
├── build.rs                     Passes the .def to the MSVC linker
├── .cargo/config.toml          target-feature=+crt-static (key to zero third-party DLLs)
├── src/
│   ├── lib.rs                   Plugin entry everything_plugin_proc + PM_* dispatch
│   ├── options.rs              Settings page in Everything's options dialog
│   │                            (enable/bind/port/restore defaults) + settings state
│   ├── plugin/
│   │   ├── mod.rs               Submodule summary + PM_* constants
│   │   ├── diag.rs              Disk diagnostic log (%LOCALAPPDATA%\everything-mcp\plugin.log)
│   │   ├── ffi_types.rs         #[repr(C)] types (Utf8Buf/DbHandle/FileInfoFd...)
│   │   ├── host.rs              Host function pointer table + HOST/HOST_LOCK
│   │   ├── search.rs            Async search → sync wait wrapper (main-thread marshaling)
│   │   ├── main_thread.rs       Main-thread window + PostMessage task dispatch
│   │   └── state.rs             Runtime state (lazily created db/query, shutdown flag)
│   └── mcp/
│       ├── mod.rs
│       ├── protocol.rs          JSON-RPC 2.0 + MCP types
│       ├── server.rs            HTTP server on std::net
│       └── tools.rs             Tool implementations (search_in_folder/list_folder/count)
├── installer/
│   ├── everything-mcp.nsi      NSIS setup script
│   └── client-config-example.json  MCP client configuration example
├── tests/
│   └── mcp.rs                  MCP protocol integration tests (cargo test)
├── docs/
│   └── PLUGIN_SDK_API_CN.md     Chinese plugin SDK API reference (with field notes)
├── reference/                  Third-party reference material (official voidtools C
│                               plugins + Everything 3.0 SDK; not compiled into this
│                               crate — see reference/README.md)
└── LICENSE
```

### Prerequisites

Only a Rust toolchain is required — no Visual Studio (the rustup MSVC toolchain
is enough):

```powershell
# Install Rust (once)
Invoke-WebRequest https://win.rustup.com/x86_64 -OutFile rustup-init.exe
.\rustup-init.exe -y
# Installs the stable-x86_64-pc-windows-msvc target by default.

# Build from the repository root
cargo build --release

# Run the tests (MCP protocol integration tests; no Everything host needed)
cargo test

# Output
ls target\release\everything_mcp.dll
```

**About "zero third-party DLLs":**

| Dependency        | Origin        | Adds a DLL? |
| ----------------- | ------------- | ----------- |
| `serde` / `serde_json` | Pure Rust | ❌ statically linked |
| `windows-sys`     | FFI bindings only | ❌ binds system DLLs via `#[link]` |
| Rust std (incl. alloc) | Ships with Rust | ❌ statically linked |
| MSVC CRT          | `/MT` + `libcmt.lib` (`crt-static`) | ❌ statically linked |

`panic = "abort"` and `lto = true` further guarantee the output contains nothing
but the single export Everything needs.

### Deployment

Copy the built `everything_mcp.dll` to **`everything_mcp64.dll`** and place it in:

```
C:\Program Files\Everything\Plugins\everything_mcp64.dll
```

Everything 1.5 loads 64-bit plugins from the root of `Plugins\` following the
`<name>64.dll` convention (same as the official `etp_server64.dll` /
`http_server64.dll`). It is picked up on the next start — no registry entries or
extra configuration.

#### Deploying via the installer

```powershell
cd installer
mkdir bin
copy ..\target\release\everything_mcp.dll bin\everything_mcp64.dll
# Requires NSIS: https://nsis.sourceforge.io/
makensis everything-mcp.nsi
# Produces everything-mcp-1.0.0-setup.exe
```

The installer targets `C:\Program Files\Everything\Plugins\` and places only
`everything_mcp64.dll` plus the accompanying documentation; uninstalling removes
just those files (never the Plugins directory itself).

### Configuration

The plugin reads the following items from Everything's settings (defaults apply
when absent):

| Setting        | Type   | Default       | Description                    |
| -------------- | ------ | ------------- | ------------------------------ |
| `mcp_enabled`  | int    | `0`           | 0 disables the MCP server (off until you opt in) |
| `mcp_port`     | int    | `8285`        | HTTP listen port               |
| `mcp_bind`     | string | `127.0.0.1`   | Bind address (localhost only)  |

Settings live in Everything's own `Settings.ini`, in the same section as the
official http_server plugin. They can also be edited in Everything's options
dialog under Plugins → MCP (enable switch, bind address, port, restore
defaults); clicking Apply takes effect immediately — no Everything restart
needed.

### Connecting an MCP client

Merge the following `mcpServers` block into Claude Desktop's
`claude_desktop_config.json` or an equivalent configuration file:

```json
{
  "mcpServers": {
    "everything": {
      "url": "http://127.0.0.1:8285/"
    }
  }
}
```

Once connected, the LLM discovers three tools automatically:

#### 1. `search_in_folder`

Find files/folders under a given folder using Everything search syntax.

```json
{
  "folder": "D:\\source\\repos\\my-project",
  "pattern": "ext:rs;toml",
  "max_results": 30,
  "timeout_ms": 10000
}
```

- `pattern` supports the full Everything syntax: `*.rs`, `"readme"`,
  `ext:md;txt`, `dm:lastweek`, `size:>1mb`, …
- An empty string `""` lists everything directly under the folder.
- `max_results = 0` means unlimited (careful with huge folders).

#### 2. `list_folder`

List the direct children (non-recursive) of a folder: name, kind, size.

```json
{ "folder": "D:\\source\\repos\\my-project" }
```

#### 3. `count`

Count matching files without returning the name list.

```json
{ "folder": "D:\\source\\repos\\my-project", "pattern": "ext:rs" }
```

### Debugging

Diagnostics go to two places:

1. **Host debug output** — via the host's `debug_printf`, viewable live with
   [DebugView](https://learn.microsoft.com/sysinternals/downloads/debugview).
2. **Disk log** — `%LOCALAPPDATA%\everything-mcp\plugin.log`, recording the PM_*
   lifecycle, host function resolution details, and the submit/wait/read steps
   of every search. This file is the fastest route when chasing crashes.

If the MCP server stops responding, verify with curl first:

```powershell
curl -X POST http://127.0.0.1:8285/ -H "Content-Type: application/json" -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}"
```

### Threading model and concurrency

- The Everything host assumes **single-threaded** access to its database API,
  and `db_query_search2` has **main-thread affinity** — calling it from a plugin
  worker thread crashes inside the host (`0xc0000005`); a mutex is not enough.
- The plugin therefore uses the same main-thread marshaling scheme as the
  official `etp_server`:
  1. On PM_START, create a message window via the host's `os_register_class` +
     `os_create_window` (host functions are mandatory — a self-created Win32
     window never receives the host's message pump);
  2. The background HTTP thread posts `db_query_search2` and the result reads
     to that window's procedure via `PostMessage`, and uses a Win32 event to
     hand completion back to the worker.
- `HOST_LOCK` (a process-wide `Mutex`) serializes all host calls.
- The db reference and query object are created **lazily on the first search**
  (by then the host is fully up) — creating them during PM_START crashes
  Everything about a second after startup.
- The primary sort key of `db_query_search2` must be the return value of
  `property_get_builtin_type(NAME)`; passing NULL crashes as well. See section
  19 of `docs/PLUGIN_SDK_API_CN.md`.

### Protocol references

- Plugin SDK: `docs/PLUGIN_SDK_API_CN.md` (complete annotated Chinese API reference)
- MCP: https://spec.modelcontextprotocol.io/
- Everything 1.5: https://www.voidtools.com/

### Reference material

The `reference/` directory holds the official voidtools C plugin sources and the
Everything 3.0 SDK (none of it is compiled into this crate). The main-thread
marshaling scheme and the `db_query_search2` parameter list used here were
derived from `reference/etp_server-1.0.2.5`. Licenses for each item are listed in
[`reference/README.md`](reference/README.md).

### License

MIT, see [LICENSE](LICENSE).
