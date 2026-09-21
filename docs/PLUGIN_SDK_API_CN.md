# Everything 1.5 Plugin SDK API 文档（中文）

> 版本基准：`everything_plugin.h`（HTTP Server 1.0.5.6 / ETP Server 1.0.2.5 共用同一份头文件，463 行）+ 官方 Plugin SDK 函数列表（`t=16535`，约 300 个可索取函数）+ 从两个示例插件源码反向工程的函数签名。
>
> **签名来源标注**：
> - ✅ **已验证** —— 签名直接来自示例插件 `static ... (EVERYTHING_PLUGIN_API *everything_plugin_xxx)(...)` 声明，可靠。
> - ⚠️ **推断** —— 官方 API 列表里有名字，但示例插件没用，签名根据命名/语义推断，**使用前请在调试器里核对一遍参数布局**。
> - ❓ **未知** —— 仅知名字，签名未推断出来，需自行探索。

---

## 目录

1. [概述与核心机制](#1-概述与核心机制)
2. [插件消息（Plugin Messages）](#2-插件消息plugin-messages)
3. [数据结构（Data Structures）](#3-数据结构data-structures)
4. [常量定义（Constants）](#4-常量定义constants)
5. [内存管理（Memory）](#5-内存管理memory)
6. [数据库访问（Database）](#6-数据库访问database)
7. [查询（db_query_*）](#7-查询db_query_)
8. [文件夹列举（db_find_*）](#8-文件夹列举db_find_)
9. [数据库快照（db_snapshot_*）](#9-数据库快照db_snapshot_)
10. [索引变更日志（db_journal_*）](#10-索引变更日志db_journal_)
11. [属性系统（property_*）](#11-属性系统property_)
12. [UTF-8 字符串（utf8_* / utf8_buf_*）](#12-utf-8-字符串utf8_--utf8_buf_)
13. [ANSI / 宽字符缓冲（ansi_* / wchar_*）](#13-ansi--宽字符缓冲ansi_--wchar_)
14. [设置存储（plugin_get/set_setting_* / config_*）](#14-设置存储plugin_getset_setting_--config_)
15. [INI 文件（ini_*）](#15-ini-文件ini_)
16. [输出流（output_stream_*）](#16-输出流output_stream_)
17. [操作系统（os_*）](#17-操作系统os_)
18. [窗口与对话框 UI（os_create_* / os_*_dlg_* / ui_*）](#18-窗口与对话框-ui)
19. [套接字（os_winsock_*）](#19-套接字os_winsock_)
20. [网络收发（network_*）](#20-网络收发network_)
21. [线程 / 事件 / 互锁 / 定时器（并发原语）](#21-线程--事件--互锁--定时器并发原语)
22. [调试与日志（debug_*）](#22-调试与日志debug_)
23. [本地化（localization_*）](#23-本地化localization_)
24. [版本信息（version_*）](#24-版本信息version_)
25. [类型安全算术（safe_*）](#25-类型安全算术safe_)
26. [未分类 / 工具杂项](#26-未分类--工具杂项)
27. [Host API 索引（按字母）](#27-host-api-索引按字母)

---

## 1. 概述与核心机制

### 1.1 插件是什么

Everything 1.5 插件是一个 **DLL**，由 Everything 主进程在启动时自动加载（放到 `C:\Program Files\Everything\plugins\` 目录下）。插件运行在 Everything 进程内部，**直接访问其内存索引**，无需任何 IPC。

官方的设计目的（`t=16535` 原文）：

> The purpose of plugins is to move the ETP, FTP, HTTP and Everything servers out of the main program and into optional plugins.

即把 ETP/FTP/HTTP/Everything 服务从主程序剥离为可选插件。

### 1.2 唯一的导出函数

插件 **只导出一个函数**：

```c
__declspec(dllexport)
void * WINAPI everything_plugin_proc(DWORD msg, void *data);
```

- 文件名导出即可，函数名必须是 `everything_plugin_proc`，调用约定 `WINAPI`（即 `__stdcall` 在 x86 上；x64 上只有一种调用约定所以无所谓）。
- Everything 用不同的 `msg`（`EVERYTHING_PLUGIN_PM_*`）反复调这一个函数，`data` 的含义随 `msg` 变化。
- 返回值含义也随 `msg` 变化，未处理的消息返回 `NULL`。

### 1.3 如何拿到 host 的能力（get_proc_address 机制）

当 Everything 发送 `PM_INIT` 时，`data` 是一个 **按名字索取函数指针的回调**：

```c
typedef void *(WINAPI *everything_plugin_get_proc_address_t)(
    const everything_plugin_utf8_t *name  // 以 null 结尾的 UTF-8 函数名
);
```

插件在 `PM_INIT` 里循环调用这个回调，把自己需要的 host 函数按名字取出来，存进全局函数指针表。**这是 host 暴露能力的唯一通道**——不存在静态链接库，全部按字符串名字动态索取。

```c
// 标准用法（来自 http_server.c）
static const struct {
    const everything_plugin_utf8_t *name;
    void **proc_address_ptr;
} proc_array[] = {
    { "mem_alloc",   (void **)&everything_plugin_mem_alloc },
    { "mem_free",    (void **)&everything_plugin_mem_free },
    { "db_query_create", (void **)&everything_plugin_db_query_create },
    // ...
};

case EVERYTHING_PLUGIN_PM_INIT:
    for (i = 0; i < count; i++) {
        proc = ((everything_plugin_get_proc_address_t)data)(proc_array[i].name);
        if (!proc) return NULL;          // 必需函数缺失，加载失败
        *proc_array[i].proc_address_ptr = proc;
    }
    break;
```

**关键规则**：
- `name` 必须是 null 结尾的 UTF-8 字符串（如 `"db_query_search"`），不含 `everything_plugin_` 前缀（host 那边注册的是短名）。
- 返回 `NULL` 表示 host 没有这个函数。**插件要决定某个函数是必需还是可选**——必需的拿不到就直接 `return NULL` 让 Everything 卸载你；可选的（如 `output_stream_flush`）给个空实现兜底。
- **不要在 `PM_INIT` 之外的时刻索取 host 函数**——`get_proc_address` 回调只在 `PM_INIT` 的 `data` 里给一次。

### 1.4 内存所有权（最容易踩坑的地方）

- host 使用**自己的内存分配器**（`mem_alloc` / `mem_free`），不是系统的 `malloc`/`free`。
- 凡是 host 填给你的缓冲（如 `utf8_buf_t`），必须用 host 的 `utf8_buf_kill` 释放，**绝对不能用 `free()` 或 Rust 的 `Drop` 直接释放**。
- host 返回的字符串指针（如某些 `utf8_string_alloc_*` 的返回值）如果是 host 分配的，也要用 host 的释放函数（`utf8_basic_string_free` 等）。
- **Rust 实践**：为每个 host 拥有的资源包一个 RAII guard，`Drop` 里调对应的 host 释放函数。

### 1.5 线程模型

- Everything 的 UI 和索引操作跑在**主线程**。host 的多数函数（尤其 `db_query_*`）**不保证线程安全**。
- 多线程插件（如起后台服务器）调用 host API 时，建议**用一把全局互斥锁串行化所有 host 调用**，避免并发撞车。
- 异步事件（如 `db_query` 的 `QUERY_COMPLETE` 回调）从 host 的线程触发，回调里只能做最小工作（唤醒一个 `os_event` 或 tokio 的 `Notify`），重活回到自己的线程做。

> **⚠️ 实测补充（everything-mcp 插件，Everything 1.5.0.1422b）：`db_query_search2` 有主线程亲和性，仅加互斥锁不够。**
>
> 从插件自己的后台线程（即使持有互斥锁）调用 `db_query_search2`，Everything 会在调用内部崩溃（异常码 `0xc0000005`，崩溃点在 Everything.exe 自身）。原因是主程序把查询状态存放在**主线程的 TLS / 线程本地结构**里，只有主线程能安全进入。
>
> 可靠做法是把调用**投递（marshal）到 Everything 主线程**执行：
> 1. PM_START 期间用 host 的 `os_register_class` + `os_create_window` 创建一个消息窗口（**必须用这两个 host 函数**，不能直接调 Win32 API —— 主程序的消息泵只处理它自己创建的窗口的消息）；
> 2. 后台线程通过 `PostMessage` 发自定义消息（`WM_USER+4` 以上，避开主程序已占用的段）到该窗口；
> 3. 在窗口过程里执行 `db_query_search2`，用完成事件把结果交还后台线程。
>
> 另见文档末尾「19. 实战经验」中关于 `sort_property_type` 不可为 NULL 的说明。

---

## 2. 插件消息（Plugin Messages）

`everything_plugin_proc(msg, data)` 的 `msg` 取值。所有常量来自 `everything_plugin.h` 第 203–221 行。

| 消息常量 | 值 | `data` 含义 | 返回值 | 说明 |
|---|---|---|---|---|
| `EVERYTHING_PLUGIN_PM_INIT` | 1 | `everything_plugin_get_proc_address_t` | 非 0 成功 / 0 失败 | **最先收到**，索取 host 函数指针。返回 0 会让 Everything 卸载该插件 |
| `EVERYTHING_PLUGIN_PM_GET_PLUGIN_VERSION` | 2 | 无 | `(void*)EVERYTHING_PLUGIN_VERSION` (= 1) | 返回插件协议版本 |
| `EVERYTHING_PLUGIN_PM_GET_NAME` | 3 | 无 | `const utf8_t*` | 返回插件名（UTF-8 字符串字面量地址） |
| `EVERYTHING_PLUGIN_PM_GET_DESCRIPTION` | 4 | 无 | `const utf8_t*` | 返回插件描述 |
| `EVERYTHING_PLUGIN_PM_GET_AUTHOR` | 5 | 无 | `const utf8_t*` | 返回作者 |
| `EVERYTHING_PLUGIN_PM_GET_VERSION` | 6 | 无 | `const utf8_t*` | 返回插件自身版本字符串 |
| `EVERYTHING_PLUGIN_PM_GET_LINK` | 7 | 无 | `const utf8_t*` | 返回插件主页 URL |
| `EVERYTHING_PLUGIN_PM_START` | 8 | 无 | 忽略 | 插件启动。**适合在这里起后台服务线程** |
| `EVERYTHING_PLUGIN_PM_STOP` | 9 | 无 | 忽略 | Everything 关闭中（窗口仍在）。停止后台线程、保存状态 |
| `EVERYTHING_PLUGIN_PM_KILL` | 10 | 无 | 忽略 | **最后一条消息**，窗口已关闭。释放所有资源 |
| `EVERYTHING_PLUGIN_PM_UNINSTALL` | 11 | 无 | 忽略 | 自定义卸载逻辑，在 `STOP` 之后 |
| `EVERYTHING_PLUGIN_PM_ADD_OPTIONS_PAGES` | 12 | `_ui_options_add_custom_page_t` | 忽略 | 注册选项页 |
| `EVERYTHING_PLUGIN_PM_LOAD_OPTIONS_PAGE` | 13 | `plugin_load_options_page_t` | 忽略 | 选项页加载，从设置读回 UI 状态 |
| `EVERYTHING_PLUGIN_PM_SAVE_OPTIONS_PAGE` | 14 | `plugin_save_options_page_t` | 忽略 | 选项页保存，把 UI 状态写回设置 |
| `EVERYTHING_PLUGIN_PM_GET_OPTIONS_PAGE_MINMAX` | 15 | `plugin_get_options_page_minmax_t` | 忽略 | 给出最小尺寸 |
| `EVERYTHING_PLUGIN_PM_SIZE_OPTIONS_PAGE` | 16 | `plugin_size_options_page_t` | 忽略 | 选项页尺寸变化 |
| `EVERYTHING_PLUGIN_PM_OPTIONS_PAGE_PROC` | 17 | `plugin_options_page_proc_t` | 忽略 | 选项页窗口消息分发 |
| `EVERYTHING_PLUGIN_PM_KILL_OPTIONS_PAGE` | 18 | `user_data` | 忽略 | 选项页已销毁后回调 |
| `EVERYTHING_PLUGIN_PM_SAVE_SETTINGS` | 19 | `output_stream_t` | 忽略 | 保存所有设置到给定输出流 |

### 选项页相关结构（来自 `everything_plugin.h` 第 302–369 行）

```c
typedef struct everything_plugin_load_options_page_s {
    void *user_data;     // 你在 ADD_OPTIONS_PAGES 时给的 userdata
    HWND   page_hwnd;    // 选项页窗口句柄
    HWND   tooltip_hwnd; // 工具提示句柄
} everything_plugin_load_options_page_t;

typedef struct everything_plugin_options_page_proc_s {
    void  *user_data;
    HWND   options_hwnd;
    HWND   page_hwnd;
    int    msg;
    WPARAM wParam;
    LPARAM lParam;
    LRESULT result;
    int    handled;      // 设置非 0 表示你处理了
} everything_plugin_options_page_proc_t;

typedef struct everything_plugin_save_options_page_s {
    void *user_data;
    HWND  page_hwnd;
    int   enable_apply;   // 设非 0 保持"应用"按钮可用
} everything_plugin_save_options_page_t;

typedef struct everything_plugin_get_options_page_minmax_s {
    void *user_data;
    HWND  page_hwnd;
    int   wide, high;    // 逻辑像素最小宽/高
} everything_plugin_get_options_page_minmax_t;

typedef struct everything_plugin_size_options_page_s {
    void *user_data;
    HWND  page_hwnd;
} everything_plugin_size_options_page_t;
```

### 完整插件入口示例（最小可用）

```c
// .def 文件：EXPORTS everything_plugin_proc

static utf8_buf_init_func   utf8_buf_init;
static utf8_buf_kill_func   utf8_buf_kill;
static db_query_create_func db_query_create;
// ... 其它 host 函数指针

static const struct { const utf8_t* name; void** ptr; } PROCS[] = {
    { "utf8_buf_init",     (void**)&utf8_buf_init },
    { "utf8_buf_kill",     (void**)&utf8_buf_kill },
    { "db_query_create",   (void**)&db_query_create },
    // ...
    { NULL, NULL }
};

void* WINAPI everything_plugin_proc(DWORD msg, void* data) {
    switch (msg) {
    case EVERYTHING_PLUGIN_PM_INIT: {
        get_proc_t get = (get_proc_t)data;
        for (int i = 0; PROCS[i].name; i++) {
            void* p = get(PROCS[i].name);
            if (!p) return NULL;       // 必需函数缺失
            *PROCS[i].ptr = p;
        }
        return (void*)1;
    }
    case EVERYTHING_PLUGIN_PM_GET_NAME:        return (void*)"everything-mcp";
    case EVERYTHING_PLUGIN_PM_GET_DESCRIPTION: return (void*)"MCP server for LLMs";
    case EVERYTHING_PLUGIN_PM_GET_VERSION:     return (void*)"0.1.0";
    case EVERYTHING_PLUGIN_PM_GET_PLUGIN_VERSION: return (void*)EVERYTHING_PLUGIN_VERSION;
    case EVERYTHING_PLUGIN_PM_START:
        start_background_server();     // 起后台线程跑 MCP HTTP 服务
        return (void*)1;
    case EVERYTHING_PLUGIN_PM_STOP:
    case EVERYTHING_PLUGIN_PM_KILL:
        stop_background_server();
        return (void*)1;
    }
    return NULL;
}
```

---

## 3. 数据结构（Data Structures）

全部 `#[repr(C)]` 对齐。Rust 中需保证字段顺序、大小、对齐与 host 编译产物完全一致。

### 3.1 不透明类型（opaque handles）

```c
typedef void *everything_plugin_db_t;                       // 数据库句柄
typedef struct everything_plugin_db_query_s    *... db_query_t;    // 查询对象
typedef struct everything_plugin_db_find_s      *... db_find_t;    // 文件夹列举句柄
typedef struct everything_plugin_db_snapshot_s  *... db_snapshot_t;// 数据库快照
typedef struct everything_plugin_db_snapshot_file_s *... db_snapshot_file_t;
typedef struct everything_plugin_db_remap_array_s   *... db_remap_array_t;
typedef struct everything_plugin_db_remap_list_s    *... db_remap_list_t;
typedef struct everything_plugin_db_journal_file_s   *... db_journal_file_t;
typedef struct everything_plugin_db_journal_notification_s *... db_journal_notification_t;
typedef struct everything_plugin_property_s     *... property_t;   // 属性类型描述
typedef void *everything_plugin_output_stream_t;           // 输出流（追加写）
typedef struct everything_plugin_os_thread_s    *... os_thread_t;  // 线程
typedef struct everything_plugin_ini_s          *... ini_t;        // INI 文件
typedef struct everything_plugin_timer_s        *... timer_t;      // 定时器
typedef uintptr_t EVERYTHING_PLUGIN_OS_WINSOCK_SOCKET;     // 套接字（Win64 是 u64）
```

Rust 实现：
```rust
#[repr(C)] pub struct Db { _p: () }
#[repr(C)] pub struct DbQuery { _p: () }
#[repr(C)] pub struct DbFind { _p: () }
// ... 其余不透明类型同理
```

### 3.2 `utf8_buf_t` —— 可增长 UTF-8 缓冲（核心字符串类型）

```c
#define EVERYTHING_PLUGIN_UTF8_BUF_STACK_SIZE  (MAX_PATH)   // 260

typedef struct everything_plugin_utf8_buf_s {
    everything_plugin_utf8_t *buf;    // 指向数据（初始指向 stack[]）
    uintptr_t  len;                    // 长度，不含 null 终止符
    uintptr_t  size;                   // buf 容量；=0 表示 buf 指向外部 const 缓冲（不可写）
    everything_plugin_utf8_t stack[EVERYTHING_PLUGIN_UTF8_BUF_STACK_SIZE]; // 内嵌栈缓冲
} everything_plugin_utf8_buf_t;
```

**生命周期**：
1. 用 `utf8_buf_init(&buf)` 初始化（栈上分配即可）。
2. 调用 host 函数填充（如 `db_query_get_result_name(q, i, &buf)`）。
3. 通过 `buf.buf`（长度 `buf.len`）读出 UTF-8 字符串。
4. 用完调 `utf8_buf_kill(&buf)` 释放可能分配的堆内存。
5. 想复用：`utf8_buf_empty(&buf)` 清空但保留容量。

⚠️ **`size == 0`** 表示 `buf` 指向 host 拥有的外部常量缓冲（如 `utf8_string_get_extension` 的返回），**不要写、不要 free**。

### 3.3 `fileinfo_fd_t` —— 索引里的文件元数据

```c
typedef struct everything_plugin_fileinfo_fd_s {
    EVERYTHING_PLUGIN_QWORD size;            // 文件大小（字节）；文件夹通常 0
    EVERYTHING_PLUGIN_QWORD date_created;    // FILETIME（1601-01-01 起 100ns 单位）
    EVERYTHING_PLUGIN_QWORD date_modified;
    EVERYTHING_PLUGIN_QWORD date_accessed;
    DWORD                   attributes;       // Win32 文件属性（FILE_ATTRIBUTE_*）
} everything_plugin_fileinfo_fd_t;
```

**FILETIME → Unix 秒**：`(filetime - 116444736000000000) / 10000000`。
**FILETIME → ISO 8601**：先用 `os_filetime_to_localtime` 转本地 `SYSTEMTIME`，再格式化，或直接用 `utf8_buf_format_filetime`。

### 3.4 `utf8_string_t` / `utf8_const_string_t` / `utf8_basic_string_t`

```c
typedef struct everything_plugin_utf8_string_s {
    everything_plugin_utf8_t *text;   // null 结尾
    uintptr_t len;                    // 字节数（不含 null）
} everything_plugin_utf8_string_t;

typedef struct everything_plugin_utf8_const_string_s {
    const everything_plugin_utf8_t *text;
    uintptr_t len;
} everything_plugin_utf8_const_string_t;

// "basic string"：长度字段后紧跟文本，常用于 host 分配的字符串
typedef struct everything_plugin_utf8_basic_string_s {
    uintptr_t len;          // 字节数
    // utf8_t text[len + 1];  // 紧随其后，null 结尾
} everything_plugin_utf8_basic_string_t;
```

辅助宏（来自头文件第 156 行）：
```c
#define EVERYTHING_PLUGIN_UTF8_BASIC_STRING_TEXT(s) \
    ((everything_plugin_utf8_t *)(((everything_plugin_utf8_basic_string_t *)(s)) + 1))
```

### 3.5 `ansi_buf_t`

```c
#define EVERYTHING_PLUGIN_ANSI_BUF_STACK_SIZE  (MAX_PATH)

typedef struct everything_plugin_ansi_buf_s {
    char     *buf;
    uintptr_t len;
    uintptr_t size;
    char      stack[EVERYTHING_PLUGIN_ANSI_BUF_STACK_SIZE];
} everything_plugin_ansi_buf_t;
```

### 3.6 `interlocked_t`

```c
typedef struct everything_plugin_interlocked_s {
    uintptr_t unaligned_a;
    uintptr_t unaligned_b;
} everything_plugin_interlocked_t;
```
（用于 `interlocked_*` 系列原子操作的存储单元，对齐到 cache line 防伪共享。）

---

## 4. 常量定义（Constants）

### 4.1 插件版本

```c
#define EVERYTHING_PLUGIN_VERSION  1   // 当前插件协议版本
```

### 4.2 内置属性类型 ID（`property_get_builtin_type` 的入参）

来自头文件第 189–201 行。**这些 ID 是稳定的**，可直接用。

| 常量 | 值 | 含义 |
|---|---|---|
| `EVERYTHING_PLUGIN_PROPERTY_TYPE_NAME` | 0 | 文件名 |
| `EVERYTHING_PLUGIN_PROPERTY_TYPE_PATH` | 1 | 路径（不含文件名） |
| `EVERYTHING_PLUGIN_PROPERTY_TYPE_SIZE` | 2 | 文件大小 |
| `EVERYTHING_PLUGIN_PROPERTY_TYPE_EXTENSION` | 3 | 扩展名 |
| `EVERYTHING_PLUGIN_PROPERTY_TYPE_TYPE` | 4 | 类型（File/Folder） |
| `EVERYTHING_PLUGIN_PROPERTY_TYPE_DATE_MODIFIED` | 5 | 修改时间 |
| `EVERYTHING_PLUGIN_PROPERTY_TYPE_DATE_CREATED` | 6 | 创建时间 |
| `EVERYTHING_PLUGIN_PROPERTY_TYPE_DATE_ACCESSED` | 7 | 访问时间 |
| `EVERYTHING_PLUGIN_PROPERTY_TYPE_ATTRIBUTES` | 8 | Win32 属性 |
| `EVERYTHING_PLUGIN_PROPERTY_TYPE_DATE_RECENTLY_CHANGED` | 9 | 最近变更时间 |
| `EVERYTHING_PLUGIN_PROPERTY_TYPE_RUN_COUNT` | 10 | 运行次数 |
| `EVERYTHING_PLUGIN_PROPERTY_TYPE_DATE_RUN` | 11 | 最近运行时间 |
| `EVERYTHING_PLUGIN_PROPERTY_TYPE_FILE_LIST_FILENAME` | 12 | 文件列表来源文件名 |

### 4.3 搜索/过滤 flag（`db_query_search` 的多个 bool 参数，或 `filter_flags` 位域）

来自头文件第 243–252 行：

| 常量 | 值 | 含义 |
|---|---|---|
| `EVERYTHING_PLUGIN_FILTER_FLAG_CASE` | 0x00000001 | 区分大小写 |
| `EVERYTHING_PLUGIN_FILTER_FLAG_WHOLEWORD` | 0x00000002 | 全字匹配 |
| `EVERYTHING_PLUGIN_FILTER_FLAG_PATH` | 0x00000004 | 匹配整个路径 |
| `EVERYTHING_PLUGIN_FILTER_FLAG_DIACRITICS` | 0x00000008 | 区分变音符 |
| `EVERYTHING_PLUGIN_FILTER_FLAG_REGEX` | 0x00000010 | 正则表达式 |
| `EVERYTHING_PLUGIN_FILTER_FLAG_PREFIX` | 0x00000020 | 匹配前缀 |
| `EVERYTHING_PLUGIN_FILTER_FLAG_SUFFIX` | 0x00000040 | 匹配后缀 |
| `EVERYTHING_PLUGIN_FILTER_FLAG_IGNORE_PUNCTUATION` | 0x00000080 | 忽略标点 |
| `EVERYTHING_PLUGIN_FILTER_FLAG_IGNORE_WHITESPACE` | 0x00000100 | 忽略空白 |
| `EVERYTHING_PLUGIN_FILTER_FLAG_SORT_DESCENDING` | 0x00000200 | 降序排序 |

### 4.4 大小单位标准（`db_query_search` 的 `size_standard` 参数）

| 常量 | 值 | 含义 |
|---|---|---|
| `EVERYTHING_PLUGIN_CONFIG_SIZE_STANDARD_JEDEC` | 0 | JEDEC（KB=1024，MB=1024KB…） |
| `EVERYTHING_PLUGIN_CONFIG_SIZE_STANDARD_IEC` | 1 | IEC（KiB/MiB…） |
| `EVERYTHING_PLUGIN_CONFIG_SIZE_STANDARD_METRIC` | 2 | 公制（KB=1000） |

### 4.5 查询事件类型（`db_query_create` 的 `event_proc` 第二参数）

来自头文件第 158–174 行。`db_query_search` 是异步的，host 通过这个回调通知状态：

| 常量 | 值 | 触发时机 |
|---|---|---|
| `EVERYTHING_PLUGIN_DB_QUERY_EVENT_RESULTS_CHANGED` | 0 | 结果列表变化 |
| `EVERYTHING_PLUGIN_DB_QUERY_EVENT_STATUS_CHANGED` | 1 | 状态变化 |
| `EVERYTHING_PLUGIN_DB_QUERY_EVENT_FILE_INFO_CHANGED` | 2 | 文件信息变化 |
| `EVERYTHING_PLUGIN_DB_QUERY_EVENT_READY` | 3 | 就绪 |
| `EVERYTHING_PLUGIN_DB_QUERY_EVENT_ACCESS_DENIED` | 4 | 访问被拒 |
| `EVERYTHING_PLUGIN_DB_QUERY_EVENT_QUERY_COMPLETE` | 5 | **搜索完成**（异步等待这个） |
| `EVERYTHING_PLUGIN_DB_QUERY_EVENT_SORT_COMPLETE` | 6 | **排序完成** |
| `EVERYTHING_PLUGIN_DB_QUERY_EVENT_QUERY_START` | 7 | 搜索开始 |
| `EVERYTHING_PLUGIN_DB_QUERY_EVENT_SORT_START` | 8 | 排序开始 |
| `EVERYTHING_PLUGIN_DB_QUERY_EVENT_ON_LOADED` | 9 | 索引加载完成 |
| `EVERYTHING_PLUGIN_DB_QUERY_EVENT_ON_INDEX_CANCELLED` | 10 | 索引被取消 |
| `EVERYTHING_PLUGIN_DB_QUERY_EVENT_TREEVIEW_CHANGED` | 11 | 树视图变化 |
| `EVERYTHING_PLUGIN_DB_QUERY_EVENT_TREEVIEW_PROPERTY_CHANGED` | 12 | 树视图属性变化 |
| `EVERYTHING_PLUGIN_DB_QUERY_EVENT_TREEVIEW_SELECTION_CHANGED` | 13 | 树视图选择变化 |
| `EVERYTHING_PLUGIN_DB_QUERY_EVENT_TREEVIEW_CLEARED` | 14 | 树视图清空 |
| `EVERYTHING_PLUGIN_DB_QUERY_EVENT_OFFLINE_CHANGED` | 15 | 离线状态变化 |

### 4.6 去重模式（`db_query_sort` 的 `find_duplicate_type`）

来自头文件第 175–183 行：

| 常量 | 值 | 含义 |
|---|---|---|
| `..._FIND_DUPLICATES_NONE` | 0 | 不去重 |
| `..._DUPLICATED_ONLY` | 1 | 只保留重复项 |
| `..._DUPLICATED_ONLY_NOCASE` | 2 | 同上，忽略大小写 |
| `..._UNIQUE` | 3 | 只保留唯一项 |
| `..._UNIQUE_NOCASE` | 4 | 同上，忽略大小写 |
| `..._DISTINCT` | 5 | 去重但每组留一个 |
| `..._DISTINCT_NOCASE` | 6 | 同上，忽略大小写 |
| `..._NOT_DISTINCT` | 7 | 只保留次要重复 |
| `..._NOT_DISTINCT_NOCASE` | 8 | 同上，忽略大小写 |

### 4.7 本地化字符串 ID

头文件第 4–116 行列了一堆 `EVERYTHING_PLUGIN_LOCALIZATION_*`。插件自定义字符串建议用自己的字符串字面量，这些 ID 主要用于复用 Everything 内置的 UI 文案。常用：

| 常量 | 值 | 文案 |
|---|---|---|
| `..._OK` | 4 | "OK" |
| `..._NAME` / `..._PATH` / `..._SIZE` / `..._DATE_MODIFIED` | 101–104 | 列标题 |
| `..._CANCEL` | 172 | "Cancel" |

---

## 5. 内存管理（Memory）

⚠️ **host 自有分配器**，所有 host 返回/填充的内存都用这里管理。

| 函数 | 签名 | 来源 | 说明 |
|---|---|---|---|
| `mem_alloc` | `void* (uintptr_t size)` | ✅ | 分配 `size` 字节，不初始化。失败行为未知（很可能返回 NULL 或抛出）。 |
| `mem_calloc` | `void* (uintptr_t size)` | ✅ | 分配 `size` 字节并清零。 |
| `mem_free` | `void (void *ptr)` | ✅ | 释放 `mem_alloc/mem_calloc` 的内存。`ptr=NULL` 行为未定义。 |

### Rust 建议

```rust
pub struct HostBox<T>(*mut T);
impl<T> Drop for HostBox<T> {
    fn drop(&mut self) { unsafe { host().mem_free(self.0 as *mut u8) } }
}
```

---

## 6. 数据库访问（Database）

### 6.1 数据库句柄与引用计数

| 函数 | 签名 | 来源 | 说明 |
|---|---|---|---|
| `db_add_local_ref` | `db_t* (void)` | ✅ | 拿到当前数据库句柄并增加引用计数。**所有 db 操作前都要先调它**。返回的 `db_t*` 在不用时调 `db_release`。 |
| `db_release` | `void (db_t *db)` | ✅ | 释放引用。配对 `db_add_local_ref`。 |
| `db_would_block` | ⚠️ 推断 `int (db_t *db)` | ❓ | 推测：检查 DB 是否正忙（如正在重建索引）。 |
| `db_get_indexed_fd` | `void (db_t *db, const utf8_t *filename, fileinfo_fd_t *fd)` | ✅ | 按完整路径取一条文件的索引元数据（不走搜索）。 |
| `db_is_index_folder_size` | `int (db_t *db)` | ✅ | 是否已索引文件夹大小。 |
| `db_folder_exists` | `int (db_t *db, const utf8_t *filename)` | ✅ | 文件夹是否存在（按索引）。 |
| `db_file_exists` | `int (db_t *db, const utf8_t *filename)` | ✅ | 文件是否存在（按索引）。 |
| `db_onready_add` | ⚠️ 推断（基于命名） | ❓ | 注册一个"DB 就绪"回调。 |
| `db_onready_remove` | ⚠️ | ❓ | 取消注册。 |

### 6.2 用法要点

```c
// 标准搜索骨架
db_t *db = db_add_local_ref();                  // ① 拿引用
db_query_t *q = db_query_create(db,             // ② 建查询
                                on_query_event, //    事件回调
                                my_user_data);
db_query_search(q, 0,0,0,0, 0,0, 0,0,0,         // ③ 触发搜索（异步）
                 1,1,1,
                 "report parent:\"C:\\proj\"",
                 0, NULL,1, NULL,1, NULL,1,
                 0, 0,0, 0, 1,1,1,1, 0, 0);
// 等待 on_query_event 收到 QUERY_COMPLETE（见第 21 节 os_event）
uintptr_t n = db_query_get_result_count(q);
for (uintptr_t i = 0; i < n; i++) { /* 读结果 */ }
db_query_destroy(q);                            // ④ 销毁
db_release(db);                                 // ⑤ 释放引用
```

---

## 7. 查询（`db_query_*`）

搜索的核心。**`db_query_search` 是异步的**，通过 `db_query_create` 注册的事件回调通知完成。

### 7.1 查询对象生命周期

| 函数 | 签名 | 来源 | 说明 |
|---|---|---|---|
| `db_query_create` | `db_query_t* (db_t *db, void(WINAPI* event_proc)(void* user_data, int type), void *user_data)` | ✅ | 创建查询对象。`event_proc` 是异步事件回调，`type` 取 `DB_QUERY_EVENT_*`。`user_data` 透传回回调。 |
| `db_query_destroy` | `void (db_query_t *q)` | ✅ | 销毁查询对象并释放资源。配对 `db_query_create`。 |
| `db_query_cancel` | `void (db_query_t *q)` | ✅ | 取消正在进行的搜索。 |

### 7.2 执行搜索

#### `db_query_search` ✅ 已验证

完整签名（33 个参数）：

```c
void db_query_search(
    db_query_t  *q,
    int  match_case,                // 区分大小写
    int  match_whole_word,          // 全字匹配
    int  match_path,                // 匹配路径
    int  match_diacritics,          // 区分变音符
    int  match_prefix,              // 匹配前缀
    int  match_suffix,              // 匹配后缀
    int  ignore_punctuation,        // 忽略标点
    int  ignore_whitespace,         // 忽略空白
    int  match_regex,               // 正则
    int  hide_empty_search_results, // 隐藏空结果项
    int  clear_selection,           // 清除选中
    int  clear_item_refs,           // 清除条目引用
    const utf8_t *search_string,    // 搜索串（Everything 语法）
    int  fast_sort_only,            // 只用快速排序键
    const property_t *sort_property_type,  int sort_ascending,   // 主排序键
    const property_t *sort_property_type2, int sort_ascending2,  // 次排序键
    const property_t *sort_property_type3, int sort_ascending3,  // 三排序键
    int  folders_first,             // 文件夹优先（见 4.3 的 folders_first 语义）
    int  track_selected_and_total_file_size, // 跟踪选中及总大小
    int  track_selected_folder_size,         // 跟踪选中文件夹大小
    int  force,                     // 强制重新搜索
    int  allow_query_access,        // 允许查询访问
    int  allow_read_access,         // 允许读访问
    int  allow_disk_access,         // 允许磁盘访问
    int  hide_omit_results,         // 隐藏被排除结果
    int  size_standard,             // 大小单位标准（见 4.4）
    int  sort_mix                   // 排序混合
);
```

**关键提示**：
- 排序键用 `property_get_builtin_type(TYPE_xxx)` 取得 `property_t*`。
- **`db_query_search2` 的第一个 `sort_property_type` 不能传 `NULL`** —— everything-mcp 实测：传 NULL 会在调用内部崩溃（`0xc0000005`）。必须至少给主排序键一个有效指针，例如 `property_get_builtin_type(EVERYTHING_PLUGIN_PROPERTY_TYPE_NAME)`（etp_server.c 即如此）。第二、三个排序键传 NULL 是安全的。
- 注意：上面 `db_query_search`（33 参数版）文档中「都传 NULL」的说法对 search2 不成立，以本节为准。
- `allow_query_access` / `allow_read_access` / `allow_disk_access` 是权限控制——MCP 服务暴露给 LLM 时建议都传 `1`（如果允许）或 `0`（保守）。
- `folders_first` 的语义可能不止 0/1，参考 `Everything3_SetSearchFoldersFirst` 的 4 个枚举值（ascending/always/never/descending）。

#### `db_query_search2` ✅ 已验证（ETP Server 用）

`db_query_search` 的增强版，多了过滤、视图、树视图参数：

```c
void db_query_search2(
    db_query_t *q,
    int match_case, int match_whole_word, int match_path, int match_diacritics,
    int match_prefix, int match_suffix,
    int ignore_punctuation, int ignore_whitespace, int match_regex,
    int hide_empty_search_results, int clear_selection, int clear_item_refs,
    const utf8_t *search_string,
    DWORD filter_flags,                  // 位域，见 4.3 的 FILTER_FLAG_*
    const utf8_t *filter,                // 过滤表达式
    const utf8_t *filter_columns,
    const property_t *filter_sort, int filter_sort_ascending,
    int filter_view,
    int fast_sort_only,
    const property_t *sort_property_type,  int sort_ascending,
    const property_t *sort_property_type2, int sort_ascending2,
    const property_t *sort_property_type3, int sort_ascending3,
    int folders_first,
    int dialog_center_x, int dialog_center_y,    // 对话框中心坐标（UI 用）
    int track_selected_and_total_file_size,
    int track_selected_folder_size,
    int force,
    int allow_query_access, int allow_read_access, int allow_disk_access,
    int hide_omit_results,
    int size_standard,
    int match_treeview,                  // 树视图匹配
    int treeview_subfolders,
    int sort_mix
);
```

### 7.3 排序

#### `db_query_sort` ✅ 已验证

对已有结果重新排序（不重新搜索）：

```c
void db_query_sort(
    db_query_t *q,
    const property_t *column_type,  int ascending,
    const property_t *column_type2, int ascending2,
    const property_t *column_type3, int ascending3,
    int folders_first,
    int force,
    int find_duplicate_type,   // 见 4.6
    int sort_mix
);
```

#### `db_query_is_fast_sort` ✅ 已验证

```c
int db_query_is_fast_sort(db_query_t *q, const property_t *property_type);
```
返回非 0 表示该属性可快速排序（不需要扫描全表）。

### 7.4 读取结果

| 函数 | 签名 | 来源 | 说明 |
|---|---|---|---|
| `db_query_get_result_count` | `uintptr_t (const db_query_t *q)` | ✅ | 结果总数。 |
| `db_query_get_result_name` | `void (db_query_t *q, uintptr_t index, utf8_buf_t *cbuf)` | ✅ | 取第 `index` 条结果的文件名，写入 `cbuf`（host 分配）。 |
| `db_query_get_result_path` | `void (db_query_t *q, uintptr_t index, utf8_buf_t *cbuf)` | ✅ | 取路径（不含文件名）。 |
| `db_query_get_result_indexed_fd` | `void (db_query_t *q, uintptr_t index, fileinfo_fd_t *fd)` | ✅ | 取索引元数据（size/dates/attributes）。 |
| `db_query_is_folder_result` | `int (db_query_t *q, uintptr_t index)` | ✅ | 该条目是否为文件夹。 |
| `db_query_get_result_file_list_filename` | `void (db_query_t *q, uintptr_t index, utf8_buf_t *cbuf)` | ✅ | 文件列表来源（针对 file-list 索引项）。 |
| `db_query_get_result_date_recently_changed` | ⚠️ 推断 `QWORD (db_query_t *q, uintptr_t index)` | ✅（ETP） | 取最近变更时间（FILETIME）。 |

**完整路径** = `path + "\\" + name`，或直接用 `utf8_buf_path_cat_filename` 拼。

### 7.5 异步等待模式

```c
// 全局
static os_event_t *g_query_done;
static void WINAPI on_query_event(void *ud, int type) {
    if (type == DB_QUERY_EVENT_QUERY_COMPLETE ||
        type == DB_QUERY_EVENT_SORT_COMPLETE) {
        os_event_set(g_query_done);   // 见 21 节
    }
}

// 调用
g_query_done = os_event_create();
db_query_search(q, ...);            // 异步触发
os_event_wait(g_query_done);        // 阻塞等完成（或用 OS 事件 + tokio Notify）
uintptr_t n = db_query_get_result_count(q);
// ...
```

---

## 8. 文件夹列举（`db_find_*`）

不走搜索语法，直接列目录。比 `db_query_search` 轻，适合"列这个文件夹下一层"。

| 函数 | 签名 | 来源 | 说明 |
|---|---|---|---|
| `db_find_first_file` | `db_find_t* (db_t *db, const utf8_t *path, utf8_buf_t *filename_cbuf, fileinfo_fd_t *fd)` | ✅ | 开始列目录，返回第一条条目。`path` 是要列的目录（如 `C:\proj`），`filename_cbuf` 接收文件名，`fd` 接收元数据。返回句柄供后续 `next` 用，失败返回 NULL。 |
| `db_find_next_file` | `int (db_find_t *fh, utf8_buf_t *filename_cbuf, fileinfo_fd_t *fd)` | ✅ | 取下一条。返回非 0 表示还有，0 表示结束。 |
| `db_find_close` | `void (db_find_t *fh)` | ✅ | 关闭列举，必须调。 |
| `db_find_get_count` | `uintptr_t (db_find_t *fh)` | ✅ | 该目录的总条目数。 |

```c
db_t *db = db_add_local_ref();
utf8_buf_t name;  fileinfo_fd_t fd;
utf8_buf_init(&name);
db_find_t *fh = db_find_first_file(db, "C:\\proj", &name, &fd);
if (fh) {
    do {
        printf("%.*s  size=%llu\n", (int)name.len, name.buf, fd.size);
    } while (db_find_next_file(fh, &name, &fd));
    db_find_close(fh);
}
utf8_buf_kill(&name);
db_release(db);
```

---

## 9. 数据库快照（`db_snapshot_*`）

⚠️ 这些函数示例插件没用，签名未验证。语义推断：快照是 DB 在某个时刻的**只读冻结视图**，用于在查询过程中不受索引变化影响，或导出全量数据。

| 函数 | 签名 | 来源 | 说明 |
|---|---|---|---|
| `db_snapshot_create` | ⚠️ 推断 `db_snapshot_t* (db_t *db)` | ❓ | 创建当前 DB 的快照。 |
| `db_snapshot_destroy` | ⚠️ 推断 `void (db_snapshot_t *s)` | ❓ | 销毁快照。 |
| `db_snapshot_get_size` | ⚠️ 推断 `QWORD (db_snapshot_t *s)` | ❓ | 快照大小（字节数）。 |
| `db_snapshot_is_out_of_date` | ⚠️ 推断 `int (db_snapshot_t *s)` | ❓ | 快照是否已过期（DB 已变化）。 |
| `db_snapshot_file_open` | ⚠️ 推断 `db_snapshot_file_t* (db_snapshot_t *s, const utf8_t *path)` | ❓ | 打开快照里的某个文件视图。 |
| `db_snapshot_file_close` | ⚠️ 推断 `void (db_snapshot_file_t *f)` | ❓ | 关闭。 |
| `db_snapshot_file_read` | ⚠️ 推断 `uintptr_t (db_snapshot_file_t *f, void *buf, uintptr_t len)` | ❓ | 从快照读文件内容。 |

**MCP 场景一般用不到**，除非要做文件内容预览。建议先用 `db_query_*`，需要时再探索。

---

## 10. 索引变更日志（`db_journal_*`）

索引变化的事件流（增量更新）。用于实现"文件变更通知"。⚠️ 签名未验证，是流式回调，**不适合 MCP 请求/响应工具**，更适合做成 MCP 资源订阅。

| 函数 | 签名 | 来源 | 说明 |
|---|---|---|---|
| `db_journal_file_open` | ⚠️ 推断 `db_journal_file_t* (...)` | ❓ | 打开 journal。 |
| `db_journal_file_close` | ⚠️ 推断 `void (db_journal_file_t *j)` | ❓ | 关闭。 |
| `db_journal_file_read` | ⚠️ 推断 `int (db_journal_file_t *j, ...)` | ❓ | 读取变更记录。 |
| `db_journal_file_would_block` | ⚠️ 推断 `int (db_journal_file_t *j)` | ❓ | 是否会阻塞（无新变更）。 |
| `db_journal_notification_register` | ⚠️ 推断 `db_journal_notification_t* (callback, user_data)` | ❓ | 注册变更通知回调。 |
| `db_journal_notification_unregister` | ⚠️ 推断 `void (db_journal_notification_t *n)` | ❓ | 取消注册。 |

---

## 11. 属性系统（`property_*`）

属性 = 列（Name / Size / DateModified / 自定义元数据如 Title/Artist）。搜索/排序都需要 `property_t*`。

| 函数 | 签名 | 来源 | 说明 |
|---|---|---|---|
| `property_get_builtin_type` | `const property_t* (int type)` | ✅ | 按内置类型 ID（见 4.2）取属性描述符。**最常用**。 |
| `property_get_type` | `int (const property_t *property_type)` | ✅ | 取属性的值类型（数字/字符串/日期…）。 |

返回的类型值含义未公开，推测对应 Everything3 SDK 头里的 `EVERYTHING3_PROPERTY_VALUE_TYPE_*`（pstring/uint64/dword 等）。

---

## 12. UTF-8 字符串（`utf8_*` / `utf8_buf_*`）

### 12.1 缓冲管理

| 函数 | 签名 | 来源 | 说明 |
|---|---|---|---|
| `utf8_buf_init` | `void (utf8_buf_t *cbuf)` | ✅ | 初始化栈上的 `utf8_buf_t`（让 `buf` 指向内嵌 `stack`）。**第一步必调**。 |
| `utf8_buf_kill` | `void (utf8_buf_t *cbuf)` | ✅ | 释放可能分配的堆内存。**用完必调**。 |
| `utf8_buf_empty` | `void (utf8_buf_t *cbuf)` | ✅ | 清空（len=0）但保留容量，便于复用。 |
| `utf8_buf_grow_length` | `void (utf8_buf_t *cbuf, uintptr_t length_in_bytes)` | ✅ | 预留至少 `length_in_bytes` 容量。 |

### 12.2 写入与拼接

| 函数 | 签名 | 来源 | 说明 |
|---|---|---|---|
| `utf8_buf_printf` | `void (utf8_buf_t *cbuf, const utf8_t *format, ...)` | ✅ | 格式化写入（printf 风格）。 |
| `utf8_buf_vprintf` | `void (utf8_buf_t *cbuf, const utf8_t *format, va_list argptr)` | ✅ | 同上，可变参数版。 |
| `utf8_buf_copy_utf8_string` | `void (utf8_buf_t *cbuf, const utf8_t *s)` | ✅ | 拷贝一个 null 结尾 UTF-8 串。 |
| `utf8_buf_copy_utf8_string_n` | `void (utf8_buf_t *cbuf, const utf8_t *s, uintptr_t slen)` | ✅ | 拷贝指定字节长度。 |
| `utf8_buf_cat_c_list_utf8_string` | ⚠️ 推断 | ❓ | 追加 C 列表风格字符串。 |
| `utf8_buf_cat_utf8_string` | ⚠️ 推断 `void (utf8_buf_t *cbuf, const utf8_t *s)` | ❓ | 追加。 |
| `utf8_buf_path_cat_filename` | `void (utf8_buf_t *cbuf, const utf8_t *path, const utf8_t *filename)` | ✅ | 路径拼接（自动加 `\`）。 |
| `utf8_buf_path_canonicalize` | `void (utf8_buf_t *cbuf)` | ✅ | 路径规范化（`.`/`..` 解析）。 |
| `utf8_buf_escape_html` | `void (utf8_buf_t *cbuf, const utf8_t *str)` | ✅ | HTML 转义。 |

### 12.3 格式化输出

| 函数 | 签名 | 来源 | 说明 |
|---|---|---|---|
| `utf8_buf_format_filetime` | `void (utf8_buf_t *cbuf, QWORD ft)` | ✅ | 把 FILETIME 格式化为可读字符串。 |
| `utf8_buf_format_size` | `void (utf8_buf_t *cbuf, QWORD number)` | ✅ | 文件大小格式化（KB/MB…）。 |
| `utf8_buf_format_qword` | `void (utf8_buf_t *cbuf, QWORD number)` | ✅ | 64 位整数转十进制字符串。 |
| `utf8_buf_format_title` | `void (cbuf, program_name, search, setting_format)` | ✅ | 按 Everything 标题格式串格式化窗口标题。 |
| `utf8_buf_format_peername` | `void (utf8_buf_t *cbuf, SOCKET socket_handle)` | ✅ | 格式化套接字对端地址。 |

### 12.4 字符串字面量分配（host 拥有）

| 函数 | 签名 | 来源 | 说明 |
|---|---|---|---|
| `utf8_string_alloc_utf8_string` | `utf8_t* (const utf8_t *s)` | ✅ | 复制一个 null 结尾串，返回 host 分配的内存。用 `mem_free` 释放。 |
| `utf8_string_alloc_utf8_string_n` | `utf8_t* (const utf8_t *s, uintptr_t slen)` | ✅ | 同上，指定字节长度。 |
| `utf8_string_realloc_utf8_string` | `utf8_t* (utf8_t *ptr, const utf8_t *s)` | ✅ | 重新分配并复制（类似 realloc + strcpy）。 |
| `utf8_string_realloc_utf8_string_n` | ⚠️ 推断 `utf8_t* (utf8_t *ptr, const utf8_t *s, uintptr_t slen)` | ❓ | 同上带长度。 |
| `utf8_string_copy_utf8_string` | `utf8_t* (utf8_t *buf, const utf8_t *s)` | ✅ | 拷贝到已分配缓冲。 |
| `utf8_basic_string_get_text_plain_file` | `utf8_basic_string_t* (const utf8_t *filename)` | ✅ | 读整个文本文件为 basic_string（host 分配）。 |
| `utf8_basic_string_free` | `void (utf8_basic_string_t *s)` | ✅ | 释放 basic_string。 |

### 12.5 解析与比较

| 函数 | 签名 | 来源 | 说明 |
|---|---|---|---|
| `utf8_string_get_extension` | `const utf8_t* (const utf8_t *filename)` | ✅ | 取扩展名（指向输入串内部，不分配）。 |
| `utf8_string_get_path_part` | `void (const utf8_t *file_name, utf8_buf_t *cbuf)` | ✅ | 取路径的某部分。 |
| `utf8_string_get_length_in_bytes` | `uintptr_t (const utf8_t *string)` | ✅ | strlen（字节数）。 |
| `utf8_string_get_win32_file_namespace` | `void (const utf8_t *path, utf8_buf_t *cbuf)` | ✅ | 加 `\\?\` 命名空间前缀。 |
| `utf8_string_compare` | `int (const utf8_t *start1, const utf8_t *start2)` | ✅ | 字节比较。 |
| `utf8_string_compare_nice_n_n` | `int (s1, s1len, s2, s2len)` | ✅ | "自然"比较（数字部分按数值比，便于排序）。 |
| `utf8_string_compare_nocase_n_n` | ⚠️ 推断 | ❓ | 不分大小写的定长比较。 |
| `utf8_string_compare_nocase_s_sla` | `int (const utf8_t *s1start, const utf8_t *lowercase_ascii_s2start)` | ✅ | 不分大小写比较，第二参数已是小写。 |
| `utf8_string_icompare` | ⚠️ 推断 `int (const utf8_t *s1, const utf8_t *s2)` | ❓ | 不分大小写比较。 |
| `utf8_string_is_url_scheme_name_with_double_forward_slash` | `const utf8_t* (const utf8_t *s)` | ✅ | 检测 `scheme://` 前缀，返回指向 `//` 后的指针或 NULL。 |
| `utf8_string_skip_ascii_ws` | `const utf8_t* (const utf8_t *p)` | ✅ | 跳过 ASCII 空白，返回第一个非空白字符。 |
| `utf8_string_parse_check` | `const utf8_t* (const utf8_t **pp, const utf8_t *string)` | ✅ | 解析校验。 |
| `utf8_string_parse_qword` | `QWORD (const utf8_t **pp)` | ✅ | 解析 64 位整数，移动 `*pp`。 |
| `utf8_string_parse_csv_item` | `void (const utf8_t *s, utf8_buf_t *cbuf)` | ✅ | 解析 CSV 项。 |
| `utf8_string_parse_c_item` | ⚠️ 推断 | ❓ | 解析 C 风格项。 |
| `utf8_string_parse_sockaddr_in` | `void (const utf8_t *s, struct sockaddr_in *addr)` | ✅ | 解析 IPv4 地址。 |
| `utf8_string_parse_sockaddr_in6` | `void (const utf8_t *s, struct sockaddr_in6 *addr)` | ✅ | 解析 IPv6 地址。 |
| `utf8_string_to_dword` | `DWORD (const utf8_t *s)` | ✅ | 字符串转 DWORD。 |
| `utf8_string_to_int` | `int (const utf8_t *str)` | ✅ | 字符串转 int。 |
| `utf8_string_to_qword` | `QWORD (const utf8_t *s)` | ✅ | 字符串转 64 位整数。 |

---

## 13. ANSI / 宽字符缓冲（`ansi_*` / `wchar_*`）

| 函数 | 签名 | 来源 | 说明 |
|---|---|---|---|
| `ansi_buf_init` | `void (ansi_buf_t *acbuf)` | ✅ | 初始化 ANSI 缓冲。 |
| `ansi_buf_kill` | `void (ansi_buf_t *acbuf)` | ✅ | 释放。 |
| `ansi_buf_copy_utf8_string` | `void (ansi_buf_t *acbuf, const utf8_t *s)` | ✅ | UTF-8 → ANSI 拷贝。 |
| `wchar_buf_init` | ⚠️ 推断 `void (wchar_buf_t *wbuf)` | ❓ | 初始化宽字符缓冲。 |
| `wchar_buf_kill` | ⚠️ 推断 `void (wchar_buf_t *wbuf)` | ❓ | 释放。 |
| `wchar_buf_copy_utf8_string` | ⚠️ 推断 `void (wchar_buf_t *wbuf, const utf8_t *s)` | ❓ | UTF-8 → UTF-16 拷贝。 |

---

## 14. 设置存储（`plugin_get/set_setting_*` / `config_*`）

持久化插件配置。Everything 把每个插件的设置存进自己的 ini。

### 14.1 读写设置（plugin_*）

| 函数 | 签名 | 来源 | 说明 |
|---|---|---|---|
| `plugin_get_setting_int` | `int (struct sorted_list_s *sorted_list, const utf8_t *name, int current_value)` | ✅ | 读整型设置。`current_value` 是找不到时的默认值。`sorted_list` 来自选项页保存上下文。 |
| `plugin_get_setting_string` | `void (struct sorted_list_s *sorted_list, const utf8_t *name, utf8_t *current_string)` | ✅ | 读字符串设置。 |
| `plugin_set_setting_int` | `void (output_stream_t *output_stream, const utf8_t *name, int value)` | ✅ | 写整型设置（写入给定的输出流）。 |
| `plugin_set_setting_string` | `void (output_stream_t *output_stream, const utf8_t *name, const utf8_t *value)` | ✅ | 写字符串设置。 |
| `plugin_get_version` | ⚠️ 推断 | ❓ | 取 Everything 版本。 |

### 14.2 配置值（config_*）⚠️ 未验证

| 函数 | 签名 | 来源 | 说明 |
|---|---|---|---|
| `config_get_int_value` | ⚠️ 推断 `int (...)` | ❓ | 取整型配置。 |
| `config_set_int_value` | ⚠️ 推断 `void (...)` | ❓ | 设置整型配置。 |

---

## 15. INI 文件（`ini_*`）

| 函数 | 签名 | 来源 | 说明 |
|---|---|---|---|
| `ini_open` | `ini_t* (utf8_t *s, const utf8_t *lowercase_ascii_section)` | ✅ | 打开 INI 文本（`s` 是文件内容）的某个 section。 |
| `ini_close` | `void (ini_t *ini)` | ✅ | 关闭。 |
| `ini_find_keyvalue` | `int (ini_t *ini, const utf8_t *nocase_ascii_key, utf8_const_string_t *value_string)` | ✅ | 查键值，写入 `value_string`。返回是否找到。 |

---

## 16. 输出流（`output_stream_*`）

追加写文件流。`PM_SAVE_SETTINGS` 的 `data` 就是一个 `output_stream_t`。

| 函数 | 签名 | 来源 | 说明 |
|---|---|---|---|
| `output_stream_append_file` | `output_stream_t* (const utf8_t *filename)` | ✅ | 打开文件用于追加。 |
| `output_stream_close` | `void (output_stream_t *s)` | ✅ | 关闭。 |
| `output_stream_write_printf` | `void (output_stream_t *output_stream, const utf8_t *format, ...)` | ✅ | 格式化写入。 |
| `output_stream_flush` | `void (output_stream_t *output_stream)` | ✅ | 刷盘（**可选**——示例里是单独索取的）。 |

---

## 17. 操作系统（`os_*`）

### 17.1 文件与路径

| 函数 | 签名 | 来源 | 说明 |
|---|---|---|---|
| `os_open_file` | `void (const utf8_t *filename)` | ✅ | 用关联程序打开文件（ShellExecute 语义）。 |
| `os_open_url` | ⚠️ 推断 `void (const utf8_t *url)` | ❓ | 用默认浏览器打开 URL。 |
| `os_resize_file` | `void (const utf8_t *filename, uintptr_t max_size, uintptr_t delta_size)` | ✅ | 调整文件大小。 |
| `os_make_sure_path_to_file_exists` | `void (const utf8_t *file_name)` | ✅ | 确保文件所在目录存在（递归创建）。 |
| `os_set_file_pointer` | `void (HANDLE h, QWORD position, int move_method)` | ✅ | 设置文件指针。 |
| `os_get_volume_label` | `void (const utf8_t *volume_path, utf8_buf_t *cbuf)` | ✅ | 取卷标。 |
| `os_get_app_data_path_cat_filename` | `void (const utf8_t *filename, utf8_buf_t *cbuf)` | ✅ | 拼出 `%APPDATA%\Everything\filename`。 |
| `os_get_local_app_data_path_cat_filename` | `void (const utf8_t *filename, utf8_buf_t *cbuf)` | ✅ | 拼出 `%LOCALAPPDATA%\...`。 |
| `os_get_local_app_data_path_cat_make_filename` | `void (const utf8_t *name, const utf8_t *extension, utf8_buf_t *cbuf)` | ✅ | 生成唯一的本地路径。 |
| `os_load_system_library` | ⚠️ 推断 | ❓ | 安全加载系统 DLL。 |
| `os_registry_get_string` | `void (HKEY root, const utf8_t *key, const utf8_t *value, utf8_buf_t *cbuf)` | ✅ | 读注册表字符串。 |

### 17.2 时间

| 函数 | 签名 | 来源 | 说明 |
|---|---|---|---|
| `os_get_system_time_as_file_time` | `QWORD (void)` | ✅ | 当前 UTC 时间（FILETIME）。 |
| `os_filetime_to_localtime` | `void (SYSTEMTIME *localst, QWORD ft)` | ✅ | FILETIME → 本地 SYSTEMTIME。 |

### 17.3 内存

| 函数 | 签名 | 来源 | 说明 |
|---|---|---|---|
| `os_copy_memory` | `void (void *dst, const void *src, uintptr_t size)` | ✅ | memcpy。 |
| `os_move_memory` | `void (void *dst, const void *src, uintptr_t size)` | ✅ | memmove（区域可重叠）。 |
| `os_zero_memory` | `void (void *ptr, uintptr_t size)` | ✅ | 清零（SecureZeroMemory 语义，不被编译器优化掉）。 |

### 17.4 杂项

| 函数 | 签名 | 来源 | 说明 |
|---|---|---|---|
| `os_sort_MT` | ⚠️ 推断 `void (void *base, uintptr_t count, uintptr_t size, compare_proc, ...)` | ❓ | 多线程排序。 |

---

## 18. 窗口与对话框 UI

UI 相关函数。**MCP 服务通常不需要 UI**，除非你要做选项页让用户配端口/白名单。函数太多不全列，按需索取：

### 18.1 创建控件（`os_create_*`）

签名都形如 `HWND (HWND parent, int id, DWORD extra_style, ...)`，例如：

```c
HWND os_create_button(HWND parent, int id, DWORD extra_window_style, const utf8_t *text);
HWND os_create_edit  (HWND parent, int id, DWORD extra_style,     const utf8_t *text);
HWND os_create_checkbox(HWND parent, int id, DWORD extra_style, int checked, const utf8_t *text);
HWND os_create_static  (HWND parent, int id, DWORD extra_window_style, const utf8_t *text);
HWND os_create_number_edit(HWND parent, int id, DWORD extra_style, __int64 number);
HWND os_create_password_edit(HWND parent, int id, DWORD extra_style, const utf8_t *text);
HWND os_create_tooltip(HWND parent, ...);
HWND os_create_window(DWORD dwExStyle, const utf8_t *lpClassName, ...);    // 全套 CreateWindow 参数
HWND os_create_blank_dialog(...);
HWND os_create_listbox(...);
HWND os_create_group_box(...);
ATOM os_register_class(UINT style, const utf8_t *lpszClassName, WNDPROC lpfnWndProc, uintptr_t window_extra, HICON, HICON, HCURSOR);
```

### 18.2 窗口操作

| 函数 | 签名（精简） | 说明 |
|---|---|---|
| `os_set_dlg_rect` | `(parent, id, x, y, wide, high)` | 设置控件矩形 |
| `os_set_dlg_text` | `(hDlg, nIDDlgItem, s)` | 设置控件文本 |
| `os_get_dlg_text` | `(hwnd, id, cbuf)` | 取控件文本到 `utf8_buf_t` |
| `os_enable_or_disable_dlg_item` | `(parent, id, enable)` | 启用/禁用 |
| `os_get_logical_wide` | `void → int` | 逻辑像素宽度（DPI 感知） |
| `os_get_logical_high` | `void → int` | 逻辑像素高度 |
| `os_center_dialog` | ⚠️ | 居中对话框 |
| `os_set_dlg_redraw` | ⚠️ | 重绘控制 |
| `os_force_ltr_edit` | ⚠️ | 强制 LTR 编辑控件 |
| `os_expand_dialog_text_logical_wide_no_prefix` | `(parent, text, wide) → int` | 展开 && 助记符 |
| `os_set_default_button` | ⚠️ | 设置默认按钮 |
| `os_get_window_user_data` / `os_set_window_user_data` | ⚠️ | 窗口 user data |
| `os_add_tooltip` | `(tooltip, parent, id, text)` | 给控件加 tooltip |

### 18.3 列表框

| 函数 | 说明 |
|---|---|
| `os_add_listbox_string_and_data` | 加项带 data |
| `os_clear_listbox` | 清空 |
| `os_get_listbox_cur_sel` | 取当前选择索引 |
| `os_get_listbox_data` | 取当前选择 data |
| `os_set_listbox_cur_sel` | 设置当前选择 |

### 18.4 文件/文件夹对话框

| 函数 | 签名（精简） | 说明 |
|---|---|---|
| `os_browse_for_folder` | `(parent, title, default_folder, cbuf)` | 选文件夹 |
| `os_get_open_file_name` | `(parent, title, initial_file, filter, filter_len, filter_index, default_extension, out_filter_index, cbuf)` | 打开文件 |
| `os_get_save_file_name` | 同上 | 保存文件 |

### 18.5 选项页集成

```c
// PM_ADD_OPTIONS_PAGES 时调用
void ui_options_add_plugin_page(
    struct everything_plugin_ui_options_add_custom_page_s *add_custom_page,
    void *user_data,
    const utf8_t *name
);
```

### 18.6 任务对话框

```c
int ui_task_dialog_show(HWND parent_hwnd, UINT flags,
                        const utf8_t *caption, const utf8_t *main_task,
                        const utf8_t *format, ...);
```

---

## 19. 套接字（`os_winsock_*`）

Winsock 的 host 封装。**写 MCP HTTP 服务推荐用 Rust 的 tokio/axum（直接用系统 socket），不必走这些**。这里列出来仅供参考：

| 函数 | 说明 |
|---|---|
| `os_winsock_WSAStartup` / `os_winsock_WSACleanup` | 初始化/清理 |
| `os_winsock_socket(af, type, protocol)` | 创建 socket |
| `os_winsock_bind` / `os_winsock_listen` / `os_winsock_accept` | 服务端 |
| `os_winsock_connect` | 客户端 |
| `os_winsock_send` / `os_winsock_recv` （或 `WSASend`/`WSARecv`） | 收发 |
| `os_winsock_shutdown` / `os_winsock_closesocket` | 关闭 |
| `os_winsock_getaddrinfo` / `os_winsock_freeaddrinfo` | DNS |
| `os_winsock_getpeername` / `os_winsock_getsockname` | 取地址 |
| `os_winsock_ntohs` | 网络字节序转主机 |
| `os_winsock_WSAAsyncSelect` / `os_winsock_WSAEventSelect` | 事件通知 |
| `os_winsock_WSACreateEvent` / `WSACloseEvent` / `WSASetEvent` / `WSAResetEvent` | 事件对象 |
| `os_winsock_WSAWaitForMultipleEvents` / `WSAEnumNetworkEvents` | 等待/枚举 |
| `os_winsock_WSAGetLastError` | 错误码 |

---

## 20. 网络收发（`network_*`）

高层网络读写封装（带错误处理/重试）：

| 函数 | 签名 | 来源 | 说明 |
|---|---|---|---|
| `network_recv` | `uintptr_t (SOCKET s, void *buf, uintptr_t len)` | ✅ | 接收，返回读到的字节数。 |
| `network_send` | `void (SOCKET s, const void *data, uintptr_t len)` | ✅ | 发送全部数据。 |
| `network_set_tcp_nodelay` | `void (SOCKET socket_handle)` | ✅ | 设 TCP_NODELAY。 |
| `network_set_keepalive` | `void (SOCKET socket_handle)` | ✅ | 开 keepalive。 |
| `network_set_nonblocking` | ⚠️ 推断 `void (SOCKET socket_handle)` | ❓ | 设非阻塞。 |

---

## 21. 线程 / 事件 / 互锁 / 定时器（并发原语）

### 21.1 线程

| 函数 | 签名 | 来源 | 说明 |
|---|---|---|---|
| `os_thread_create` | `void (DWORD(WINAPI* thread_proc)(void*), void *param)` | ✅ | 创建并启动线程。 |
| `os_thread_wait_and_close` | `void (os_thread_t *t)` | ✅ | 等线程结束并关闭句柄。 |
| `os_thread_cancel_synchronous_io` | ⚠️ 推断 | ❓ | 取消阻塞 IO。 |

### 21.2 事件（手动/自动重置）

| 函数 | 签名 | 来源 | 说明 |
|---|---|---|---|
| `os_event_create` | `os_event_t* (void)` | ✅ | 创建事件对象。 |
| `os_event_is_set` | ⚠️ 推断 `int (os_event_t *e)` | ❓ | 是否已触发。 |

⚠️ 推测还有 `os_event_set` / `os_event_wait` / `os_event_destroy`，但示例里没明示，可能在 `event_post` / `event_remove` 里。

### 21.3 通用事件 API（`event_*`）⚠️ 未验证

| 函数 | 推断签名 | 说明 |
|---|---|---|
| `event_post` | ⚠️ | 触发事件。 |
| `event_remove` | ⚠️ | 移除事件回调。 |

### 21.4 原子操作（`interlocked_*`）

存储单元是 `interlocked_t`（见 3.6）。⚠️ 签名未验证，参考 Win32 Interlocked* 语义：

| 函数 | 推断签名 | 说明 |
|---|---|---|
| `interlocked_inc` | ⚠️ `uintptr_t (interlocked_t *v)` | 自增并返回新值。 |
| `interlocked_dec` | ⚠️ `uintptr_t (interlocked_t *v)` | 自减并返回新值。 |
| `interlocked_get` | ⚠️ `uintptr_t (interlocked_t *v)` | 原子读。 |
| `interlocked_set` | ⚠️ `void (interlocked_t *v, uintptr_t value)` | 原子写。 |

### 21.5 定时器（`timer_*`）⚠️ 未验证

| 函数 | 说明 |
|---|---|
| `timer_create` | 创建定时器。 |
| `timer_destroy` | 销毁定时器。 |

---

## 22. 调试与日志（`debug_*`）

写入 Everything 的调试输出（Debug View、`-debug` 启动时的控制台、调试日志文件）。

| 函数 | 签名 | 来源 | 说明 |
|---|---|---|---|
| `debug_printf` | `void (const utf8_t *format, ...)` | ✅ | 一般调试输出。 |
| `debug_error_printf` | `void (const utf8_t *format, ...)` | ✅ | 错误输出。 |
| `debug_color_printf` | `void (DWORD color, const utf8_t *format, ...)` | ✅ | 带颜色输出。 |
| `debug_fatal2` | ⚠️ 推断 | ❓ | 致命错误（很可能弹窗 + 退出）。 |
| `debug_is_verbose` | `int (void)` | ✅ | 是否开了 verbose 调试。 |

---

## 23. 本地化（`localization_*`）

| 函数 | 签名 | 来源 | 说明 |
|---|---|---|---|
| `localization_get_string` | `const utf8_t* (int id)` | ✅ | 按 ID 取当前语言的本地化字符串（见 4.7）。 |
| `localization_get_en_us_string` | ⚠️ 推断 `const utf8_t* (int id)` | ❓ | 强制取 en-US 字符串。 |
| `localization_substitute1` | ⚠️ 推断 | ❓ | 替换 1 个占位符。 |
| `localization_substitute2` | ⚠️ 推断 | ❓ | 替换 2 个占位符。 |

---

## 24. 版本信息（`version_*`）

| 函数 | 签名 | 来源 | 说明 |
|---|---|---|---|
| `version_get_text` | `void (utf8_buf_t *cbuf)` | ✅ | 取完整版本字符串。 |
| `version_get_major` | ⚠️ 推断 `int (void)` | ❓ | 主版本号。 |
| `version_get_minor` | ⚠️ 推断 `int (void)` | ❓ | 次版本号。 |
| `version_get_revision` | ⚠️ 推断 `int (void)` | ❓ | 修订号。 |
| `version_get_build` | ⚠️ 推断 `int (void)` | ❓ | 构建号。 |

---

## 25. 类型安全算术（`safe_*`）

防整数溢出的算术，返回安全值（溢出时返回最大值）。

| 函数 | 签名 | 来源 | 说明 |
|---|---|---|---|
| `safe_uintptr_add` | `uintptr_t (uintptr_t a, uintptr_t b)` | ✅ | 加法。 |
| `safe_uintptr_mul_sizeof_pointer` | `uintptr_t (uintptr_t a)` | ✅ | `a * sizeof(void*)`，防溢出。 |

---

## 26. 未分类 / 工具杂项

| 函数 | 来源 | 说明 |
|---|---|---|
| `unicode_base64_index` | ✅ `int (int c)` | base64 字符索引。 |
| `unicode_hex_char` | ✅ `utf8_t (int value)` | 数值 → hex 字符。 |
| `unicode_is_digit` | ✅ `int (int c)` | 是否十进制数字。 |
| `unicode_is_ascii_ws` | ✅ `int (int c)` | 是否 ASCII 空白。 |

### db_remap_* （结果重映射，⚠️ 全部未验证）

用于把结果列表按某种顺序重映射。MCP 场景一般不需要。

| 函数 | 说明 |
|---|---|
| `db_remap_array_create` | 创建重映射数组。 |
| `db_remap_array_destroy` | 销毁。 |
| `db_remap_array_get_hashcode` | 取哈希。 |
| `db_remap_list_create` | 创建重映射列表。 |
| `db_remap_list_destroy` | 销毁。 |
| `db_remap_list_add` | 加入项。 |

---

## 27. Host API 索引（按字母）

> 完整的官方函数名列表（来自 `t=16535`）。带 ✅ 的是签名已验证；其余需自行核对。

```
ansi_buf_copy_utf8_string                ✅
ansi_buf_init                            ✅
ansi_buf_kill                            ✅
config_get_int_value                     ⚠️
config_set_int_value                     ⚠️
db_add_local_ref                         ✅
db_file_exists                           ✅
db_find_close                            ✅
db_find_first_file                       ✅
db_find_get_count                        ✅
db_find_next_file                        ✅
db_folder_exists                         ✅
db_get_indexed_fd                        ✅
db_is_index_folder_size                  ✅
db_journal_file_close                    ⚠️
db_journal_file_open                     ⚠️
db_journal_file_read                     ⚠️
db_journal_file_would_block              ⚠️
db_journal_notification_register         ⚠️
db_journal_notification_unregister       ⚠️
db_onready_add                           ⚠️
db_onready_remove                        ⚠️
db_query_cancel                          ✅
db_query_create                          ✅
db_query_destroy                         ✅
db_query_get_result_count                ✅
db_query_get_result_date_recently_changed ✅
db_query_get_result_file_list_filename   ✅
db_query_get_result_indexed_fd           ✅
db_query_get_result_name                 ✅
db_query_get_result_path                 ✅
db_query_is_fast_sort                    ✅
db_query_is_folder_result                ✅
db_query_search                          ✅
db_query_search2                         ✅
db_query_sort                            ✅
db_release                               ✅
db_remap_array_create                    ⚠️
db_remap_array_destroy                   ⚠️
db_remap_array_get_hashcode              ⚠️
db_remap_list_add                        ⚠️
db_remap_list_create                     ⚠️
db_remap_list_destroy                    ⚠️
db_snapshot_create                       ⚠️
db_snapshot_destroy                      ⚠️
db_snapshot_file_close                   ⚠️
db_snapshot_file_open                    ⚠️
db_snapshot_file_read                    ⚠️
db_snapshot_get_size                     ⚠️
db_snapshot_is_out_of_date               ⚠️
db_would_block                           ⚠️
debug_color_printf                       ✅
debug_error_printf                       ✅
debug_fatal2                             ⚠️
debug_is_verbose                         ✅
debug_printf                             ✅
event_post                               ⚠️
event_remove                             ⚠️
ini_close                                ✅
ini_find_keyvalue                        ✅
ini_open                                 ✅
interlocked_dec                          ⚠️
interlocked_get                          ⚠️
interlocked_inc                          ⚠️
interlocked_set                          ⚠️
localization_get_en_us_string            ⚠️
localization_get_string                  ✅
localization_substitute1                 ⚠️
localization_substitute2                 ⚠️
mem_alloc                                ✅
mem_calloc                               ✅
mem_free                                 ✅
network_recv                             ✅
network_send                             ✅
network_set_keepalive                    ✅
network_set_nonblocking                  ⚠️
network_set_tcp_nodelay                  ✅
os_add_listbox_string_and_data           ⚠️
os_add_tooltip                           ✅
os_browse_for_folder                     ✅
os_center_dialog                         ⚠️
os_clear_listbox                         ⚠️
os_copy_memory                           ✅
os_create_blank_dialog                   ⚠️
os_create_button                         ✅
os_create_checkbox                       ✅
os_create_edit                           ✅
os_create_group_box                      ⚠️
os_create_listbox                        ⚠️
os_create_number_edit                    ✅
os_create_password_edit                  ✅
os_create_static                         ✅
os_create_tooltip                        ⚠️
os_create_window                         ✅
os_enable_or_disable_dlg_item            ✅
os_event_create                          ✅
os_event_is_set                          ⚠️
os_expand_dialog_text_logical_wide_no_prefix ✅
os_filetime_to_localtime                 ✅
os_force_ltr_edit                        ⚠️
os_get_app_data_path_cat_filename        ✅
os_get_dlg_text                          ✅
os_get_listbox_cur_sel                   ⚠️
os_get_listbox_data                      ⚠️
os_get_local_app_data_path_cat_filename  ✅
os_get_local_app_data_path_cat_make_filename ✅
os_get_logical_high                      ✅
os_get_logical_wide                      ✅
os_get_open_file_name                    ✅
os_get_save_file_name                    ✅
os_get_system_time_as_file_time          ✅
os_get_volume_label                      ✅
os_get_window_user_data                  ⚠️
os_load_system_library                   ⚠️
os_make_sure_path_to_file_exists         ✅
os_move_memory                           ✅
os_open_file                             ✅
os_open_url                              ⚠️
os_register_class                        ✅
os_registry_get_string                   ✅
os_resize_file                           ✅
os_set_default_button                    ⚠️
os_set_dlg_rect                          ✅
os_set_dlg_redraw                        ⚠️
os_set_dlg_text                          ✅
os_set_file_pointer                      ✅
os_set_listbox_cur_sel                   ⚠️
os_set_window_user_data                  ⚠️
os_sort_MT                               ⚠️
os_thread_cancel_synchronous_io          ⚠️
os_thread_create                         ✅
os_thread_wait_and_close                 ✅
os_winsock_WSAAsyncSelect                ✅
os_winsock_WSACleanup                    ✅
os_winsock_WSACloseEvent                 ⚠️
os_winsock_WSACreateEvent                ⚠️
os_winsock_WSAEnumNetworkEvents          ⚠️
os_winsock_WSAEventSelect                ⚠️
os_winsock_WSAGetLastError               ✅
os_winsock_WSARecv                       ⚠️
os_winsock_WSAResetEvent                 ⚠️
os_winsock_WSASend                       ⚠️
os_winsock_WSASetEvent                   ⚠️
os_winsock_WSAStartup                    ✅
os_winsock_WSAWaitForMultipleEvents      ⚠️
os_winsock_accept                        ✅
os_winsock_bind                          ✅
os_winsock_closesocket                   ✅
os_winsock_connect                       ✅
os_winsock_freeaddrinfo                  ✅
os_winsock_getaddrinfo                   ✅
os_winsock_getpeername                   ✅
os_winsock_getsockname                   ✅
os_winsock_listen                        ✅
os_winsock_ntohs                         ✅
os_winsock_shutdown                      ✅
os_winsock_socket                        ✅
os_zero_memory                           ✅
output_stream_append_file                ✅
output_stream_close                      ✅
output_stream_flush                      ✅
output_stream_write_printf               ✅
plugin_get_setting_int                   ✅
plugin_get_setting_string                ✅
plugin_get_version                       ⚠️
plugin_set_setting_int                   ✅
plugin_set_setting_string                ✅
property_get_builtin_type                ✅
property_get_type                        ✅
safe_uintptr_add                         ✅
safe_uintptr_mul_sizeof_pointer          ✅
timer_create                             ⚠️
timer_destroy                            ⚠️
ui_options_add_plugin_page               ✅
ui_task_dialog_show                      ✅
unicode_base64_index                     ✅
unicode_hex_char                         ✅
unicode_is_ascii_ws                      ✅
unicode_is_digit                         ✅
utf8_basic_string_free                   ✅
utf8_basic_string_get_text_plain_file    ✅
utf8_buf_cat_c_list_utf8_string          ⚠️
utf8_buf_cat_utf8_string                 ⚠️
utf8_buf_copy_utf8_string                ✅
utf8_buf_copy_utf8_string_n              ✅
utf8_buf_empty                           ✅
utf8_buf_escape_html                     ✅
utf8_buf_format_filetime                 ✅
utf8_buf_format_peername                 ✅
utf8_buf_format_qword                    ✅
utf8_buf_format_size                     ✅
utf8_buf_format_title                    ✅
utf8_buf_grow_length                     ✅
utf8_buf_init                            ✅
utf8_buf_kill                            ✅
utf8_buf_path_canonicalize               ✅
utf8_buf_path_cat_filename               ✅
utf8_buf_printf                          ✅
utf8_buf_vprintf                         ✅
utf8_string_alloc_utf8_string            ✅
utf8_string_alloc_utf8_string_n          ✅
utf8_string_compare                      ✅
utf8_string_compare_nice_n_n             ✅
utf8_string_compare_nocase_n_n           ⚠️
utf8_string_compare_nocase_s_sla         ✅
utf8_string_copy_utf8_string             ✅
utf8_string_get_extension                ✅
utf8_string_get_length_in_bytes          ✅
utf8_string_get_path_part                ✅
utf8_string_get_win32_file_namespace     ✅
utf8_string_icompare                     ⚠️
utf8_string_is_url_scheme_name_with_double_forward_slash ✅
utf8_string_parse_c_item                 ⚠️
utf8_string_parse_check                  ✅
utf8_string_parse_csv_item               ✅
utf8_string_parse_qword                  ✅
utf8_string_parse_sockaddr_in            ✅
utf8_string_parse_sockaddr_in6           ✅
utf8_string_realloc_utf8_string          ✅
utf8_string_realloc_utf8_string_n        ⚠️
utf8_string_skip_ascii_ws                ✅
utf8_string_to_dword                     ✅
utf8_string_to_int                       ✅
utf8_string_to_qword                     ✅
version_get_build                        ⚠️
version_get_major                        ⚠️
version_get_minor                        ⚠️
version_get_revision                     ⚠️
version_get_text                         ✅
wchar_buf_copy_utf8_string               ⚠️
wchar_buf_init                           ⚠️
wchar_buf_kill                           ⚠️
```

---

## 附录 A：搜索流程完整代码示例

```c
// 假设 host 函数指针已通过 PM_INIT 全部索取完毕

// 事件回调（host 从其线程调用）
static os_event_t *g_done_evt;
static void WINAPI on_query_event(void *ud, int type) {
    if (type == EVERYTHING_PLUGIN_DB_QUERY_EVENT_QUERY_COMPLETE) {
        // 唤醒等待者（具体 API 见 21 节，示例简化）
        // os_event_set(g_done_evt);
    }
}

// 高层搜索函数
int search_in_folder(const char *folder, const char *pattern, int max) {
    db_t *db = db_add_local_ref();
    if (!db) return -1;

    db_query_t *q = db_query_create(db, on_query_event, NULL);
    if (!q) { db_release(db); return -2; }

    // 拼 Everything 搜索串：pattern parent:"folder"
    utf8_buf_t search;
    utf8_buf_init(&search);
    utf8_buf_printf(&search, "%s parent:\"%s\"", pattern, folder);

    const property_t *p_name = property_get_builtin_type(
        EVERYTHING_PLUGIN_PROPERTY_TYPE_NAME);

    g_done_evt = os_event_create();
    db_query_search(q,
        /*match_case*/0, /*whole_word*/0, /*path*/0, /*diacritics*/0,
        /*prefix*/0, /*suffix*/0,
        /*ign_punct*/0, /*ign_ws*/0, /*regex*/0,
        /*hide_empty*/1, /*clear_sel*/1, /*clear_refs*/1,
        search.buf,
        /*fast_sort*/0,
        p_name, /*asc*/1,
        NULL,1, NULL,1,
        /*folders_first*/1,
        /*track_size*/0, /*track_folder_size*/0,
        /*force*/0,
        /*allow_query*/1, /*allow_read*/1, /*allow_disk*/1,
        /*hide_omit*/0, /*size_std*/0, /*sort_mix*/0
    );
    // os_event_wait(g_done_evt);   // 等完成

    uintptr_t total = db_query_get_result_count(q);
    uintptr_t lim = total < (uintptr_t)max ? total : (uintptr_t)max;

    utf8_buf_t name, path;
    fileinfo_fd_t fd;
    utf8_buf_init(&name);
    utf8_buf_init(&path);

    for (uintptr_t i = 0; i < lim; i++) {
        db_query_get_result_name(q, i, &name);
        db_query_get_result_path(q, i, &path);
        db_query_get_result_indexed_fd(q, i, &fd);
        int is_folder = db_query_is_folder_result(q, i);
        // 处理 name/path/fd...
    }

    utf8_buf_kill(&name);
    utf8_buf_kill(&path);
    utf8_buf_kill(&search);
    db_query_destroy(q);
    db_release(db);
    return 0;
}
```

## 附录 B：Rust FFI 绑定骨架

```rust
// src/plugin/ffi_types.rs
use core::ffi::c_void;

pub const UTF8_BUF_STACK: usize = 260; // MAX_PATH

#[repr(C)]
pub struct Utf8Buf {
    pub buf: *mut u8,
    pub len: usize,
    pub size: usize,
    pub stack: [u8; UTF8_BUF_STACK],
}

#[repr(C)]
pub struct FileinfoFd {
    pub size: u64,
    pub date_created: u64,
    pub date_modified: u64,
    pub date_accessed: u64,
    pub attributes: u32,
}

#[repr(C)] pub struct Db { _p: () }
#[repr(C)] pub struct DbQuery { _p: () }
#[repr(C)] pub struct DbFind { _p: () }
#[repr(C)] pub struct Property { _p: () }
#[repr(C)] pub struct OutputStream { _p: () }

pub type EventProc = unsafe extern "system" fn(*mut c_void, i32);

// src/plugin/host.rs
#[repr(C)]
pub struct Host {
    pub mem_alloc:           unsafe extern "C" fn(usize) -> *mut u8,
    pub mem_free:            unsafe extern "C" fn(*mut u8),
    pub db_add_local_ref:    unsafe extern "C" fn() -> *mut Db,
    pub db_release:          unsafe extern "C" fn(*mut Db),
    pub db_query_create:     unsafe extern "C" fn(*mut Db, Option<EventProc>, *mut c_void) -> *mut DbQuery,
    pub db_query_destroy:    unsafe extern "C" fn(*mut DbQuery),
    pub db_query_search:     unsafe extern "C" fn(/* 33 参数，逐个声明 */),
    pub db_query_get_result_count: unsafe extern "C" fn(*const DbQuery) -> usize,
    pub db_query_get_result_name:  unsafe extern "C" fn(*mut DbQuery, usize, *mut Utf8Buf),
    pub db_query_get_result_path:  unsafe extern "C" fn(*mut DbQuery, usize, *mut Utf8Buf),
    pub db_query_get_result_indexed_fd: unsafe extern "C" fn(*mut DbQuery, usize, *mut FileinfoFd),
    pub db_query_is_folder_result: unsafe extern "C" fn(*mut DbQuery, usize) -> i32,
    pub property_get_builtin_type:  unsafe extern "C" fn(i32) -> *const Property,
    pub utf8_buf_init:  unsafe extern "C" fn(*mut Utf8Buf),
    pub utf8_buf_kill:  unsafe extern "C" fn(*mut Utf8Buf),
    pub utf8_buf_empty: unsafe extern "C" fn(*mut Utf8Buf),
    pub utf8_buf_printf: unsafe extern "C" fn(*mut Utf8Buf, *const u8, ...),
    // ...
}

// RAII guard
pub struct HostUtf8Buf(Utf8Buf);
impl HostUtf8Buf {
    pub fn new() -> Self {
        let mut b = std::mem::MaybeUninit::<Utf8Buf>::zeroed();
        unsafe { crate::plugin::host().utf8_buf_init(b.as_mut_ptr()); }
        Self(unsafe { b.assume_init() })
    }
    pub fn as_str(&self) -> &str {
        unsafe { core::slice::from_raw_parts(self.0.buf, self.0.len) }
            .to_str().unwrap_or("")
    }
}
impl Drop for HostUtf8Buf {
    fn drop(&mut self) { unsafe { crate::plugin::host().utf8_buf_kill(&mut self.0); } }
}
```

---

## 附录 C：参考来源

| 来源 | 内容 |
|---|---|
| `everything_plugin.h`（463 行，HTTP/ETP 共用） | 所有类型定义、消息 ID、常量、事件 ID、选项页结构 |
| 官方 Plugin SDK 页 `t=16535` | 全部可索取的 host 函数名（约 300 个） |
| `http_server-1.0.5.6/src/http_server.c` | 127 个已验证签名（按 `static (EVERYTHING_PLUGIN_API *everything_plugin_xxx)(...)` 抽取） |
| `etp_server-1.0.2.5/src/etp_server.c` | 补充签名（`db_query_search2`、`db_get_indexed_fd` 等） |
| 官方 Everything3 SDK `t=15853` | 外部 IPC SDK（与本插件 SDK 不同，但属性类型枚举可借鉴推断） |

---

## 19. 实战经验（everything-mcp 插件实测，Everything 1.5.0.1422b）

以下每条都在 everything-mcp（Rust，cdylib，`Plugins\everything_mcp64.dll`）上实测踩坑得到，按踩坑顺序排列。

### 19.1 部署：DLL 命名必须是 `<name>64.dll`

Everything 1.5 从 `<安装目录>\Plugins\` 根目录按 **`<插件名>64.dll`** 约定加载 64 位插件。官方自带的 `etp_server64.dll`、`http_server64.dll`、`everything_server64.dll` 都遵循此约定。我们的 `everything_mcp.dll` 构建产物需复制为 `everything_mcp64.dll` 放入 Plugins 根目录，启动即被加载（PM_INIT 自动到来）。不需要注册表、不需要额外配置。

### 19.2 `db_query_search2` 必须在 Everything 主线程调用

这是最深的坑：从插件后台线程（HTTP 服务器线程）调用 `db_query_search2`，即使持有全局互斥锁，Everything 也在调用内部崩溃（`0xc0000005`，崩溃地址在 Everything.exe 自身，插件 DLL 完全不在栈上）。主程序把查询状态放在主线程本地结构里。

可靠方案（etp_server.c 同款）：
1. PM_START 时用 host 的 `os_register_class` + `os_create_window` 创建消息窗口。**必须用这两个 host 函数**而不是直接调 Win32 `RegisterClass`/`CreateWindow` —— 主程序的消息泵只派发它自己创建的窗口的消息，自建窗口收不到投递。
2. 后台线程 `PostMessage` 自定义消息（用 `WM_USER+4` 及以上，避开主程序已占用的低段）到该窗口，把待执行调用包装成 **C 风格函数指针 + 上下文指针**（不要用语言闭包），窗口过程里执行并用原子标志通知后台线程完成。
3. `db_query_search2` 与结果读取（`db_query_get_result_*`）都走这条主线程通道。

### 19.3 `db_query_search2` 的 `sort_property_type` 不能传 NULL

第一排序键传 `NULL` 同样在调用内部崩溃（`0xc0000005`）。必须传 `property_get_builtin_type(EVERYTHING_PLUGIN_PROPERTY_TYPE_NAME)` 的返回值（etp_server.c 正是如此）。第二、第三排序键传 NULL 安全。

### 19.4 db 引用与 query 对象：懒创建更稳

在 PM_START 期间创建 db 引用 + query 对象，Everything 会在启动约 1 秒后崩溃（同样 `0xc0000005`）。改成**第一次搜索时才创建**（此时主程序已完整启动、主窗口就绪）后稳定。etp_server.c 的 query 也是 per-client-connection 创建而非启动时创建。

注意 `db_query_create` 也有主线程亲和性，懒创建要放在主线程任务里做。

### 19.5 异步查询的字符串保活

`db_query_search2` 是异步的：提交后立即返回，查询在主程序后台线程执行，期间仍会读取 `search_string`。因此搜索字符串必须保活到结果读出之后（分配在堆上、函数返回前不释放），不能是提交后即失效的栈缓冲区。

### 19.6 event_proc 的等待要用「已提交查询」守卫

`db_query_create` 注册的 event_proc 会收到**所有**查询完成事件（包括主程序 UI 自己的查询），不只是我们的。等待方要用一个「本次已提交查询」标志（etp_server.c 的 `c->is_query`）过滤，否则可能被无关事件提前唤醒、读到空结果。

### 19.7 host 函数解析

- 用 `get_proc_address(name)` 按 UTF-8 名字解析，失败要记录缺哪个。
- `property_get_builtin_type` 建议按强制项解析（没有它就不能安全搜索）。
- `os_register_class` / `os_create_window` 是主线程 marshaling 的前提，同样应按强制项对待。
- 实测 `get_setting_int` 在 1.5.0.1422b 上不存在，读配置走 `get_setting_string`。

### 19.8 编译产物零第三方 DLL

Rust cdylib + 静态链接 CRT（`+crt-static`）+ 纯 Rust 依赖（serde/serde_json 静态链接）+ windows-sys 仅绑定系统 DLL，产物只有一个 `everything_mcp.dll`，直接丢进 Plugins 目录即可运行。
