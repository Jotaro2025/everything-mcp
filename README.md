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
│       ├── protocol.rs         JSON-RPC 2.0 + MCP 双时代类型（2024-11-05 / 2026-07-28）
│       ├── server.rs           基于 std::net 的 HTTP 服务，按请求协商协议版本、校验 Origin
│       ├── tools.rs            工具实现（search_in_folder/list_folder/count）
│       └── validate.rs         入参校验与路径规范化（folder 规范化、pattern 校验）
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

**只需要一个 URL。** 插件端点是双时代的：老的 MCP 客户端走 `initialize` 握手，
2026-07-28 起的客户端跳过握手、直接发请求，两者都能正常接入，无需在客户端侧
指定协议版本。前提是插件已启用（Everything 选项对话框「插件 → MCP」页勾选，
或 `Settings.ini` 里 `mcp_enabled=1`）。

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

三个工具共用的入参规则：`folder` 必须是绝对路径（`C:\…` 或 `\\server\share\…`）。
带引号、正斜杠、重复反斜杠、尾斜杠的写法会被自动规范化；通配符属于 `pattern`
而不属于 `folder`。规范化失败时返回 `-32602 INVALID_PARAMS`，消息里带期望格式
的范例与收到的原值 —— LLM 据此一次改对，不会带着坏参数反复重试。

#### 手动验证（curl）

不接客户端，先用 curl 确认服务活着。两种时代的请求各发一次：

```powershell
# legacy：initialize 握手（老客户端流程）
curl -X POST http://127.0.0.1:8285/ -H "Content-Type: application/json" -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}"

# modern：不握手，直接 server/discover（版本同时写在请求头和 _meta 里）
curl -X POST http://127.0.0.1:8285/ -H "Content-Type: application/json" -H "MCP-Protocol-Version: 2026-07-28" -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"server/discover\",\"params\":{\"_meta\":{\"io.modelcontextprotocol/protocolVersion\":\"2026-07-28\"}}}"
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

`reference/` 目录存放 voidtools 官方发布的 C 插件源码与 Everything 3.0 SDK
（均不参与本 crate 编译），开发本插件时的主线程 marshaling、`db_query_search2`
参数表等实现均对照 `reference/etp_server-1.0.2.5` 的源码。各材料的许可证见
[`reference/README.md`](reference/README.md)。

### 许可证

MIT，见 [LICENSE](LICENSE)。
