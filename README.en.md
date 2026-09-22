# everything-mcp

[中文](README.md)

---

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
- **Dual-era MCP protocol** — one endpoint serves both client generations:
  2024-11-05 (`initialize` handshake) and 2026-07-28 (per-request `_meta`
  protocol version + `server/discover`). Unsupported versions get `-32022`
  with the supported list; legacy client behavior is completely unchanged.
- **Input validation & path normalization** — `folder` is de-quoted,
  slash-unified, backslash-collapsed and trailing-slash-trimmed before it ever
  reaches Everything; invalid paths come back as `INVALID_PARAMS` with
  examples instead of silently returning zero results.
- **Installable** — one command produces x64 and x86 installers; Everything
  itself performs the install and uninstall (same mechanism as the official
  plugins).

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
│       ├── protocol.rs          JSON-RPC 2.0 + dual-era MCP types (2024-11-05 / 2026-07-28)
│       ├── server.rs            HTTP server on std::net; per-request version
│       │                         negotiation and Origin validation
│       ├── tools.rs             Tool implementations (search_in_folder/list_folder/count)
│       └── validate.rs          Argument validation & path normalization
├── installer/
│   ├── build-installers.ps1     One-command packaging script (x64 + x86 installers)
│   ├── setup/
│   │   ├── setup.c              Installer launcher source (follows the official plugins)
│   │   ├── setup.rc             Version info + resource script embedding the bz2'd plugin dll
│   │   ├── resource.h           Resource IDs (IDR_DLL_BZ2 = 107)
│   │   └── version.h            Version numbers (generated from Cargo.toml at build time)
│   └── client-config-example.json  MCP client configuration example
├── tests/
│   └── mcp.rs                  MCP protocol integration tests (cargo test)
├── docs/
│   └── PLUGIN_SDK_API_CN.md     Chinese plugin SDK API reference (with field notes)
├── reference/                  Third-party reference material (official voidtools C
│                               plugins; not compiled into this crate — see
│                               reference/README.md)
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

Packaging follows the official plugins: `setup.exe` copies no files itself. It
locates Everything and relaunches it with `-setup-plugin`, and Everything
extracts the plugin dll from the exe's resources, installs it into `Plugins\`
and registers it.

```powershell
cd installer
powershell -ExecutionPolicy Bypass -File build-installers.ps1
```

Outputs (in `installer\dist\`):

| Installer                            | Arch | Embedded plugin dll   |
| ------------------------------------ | ---- | --------------------- |
| `everything-mcp-1.0.0-x64-setup.exe` | x64  | `everything_mcp64.dll` |
| `everything-mcp-1.0.0-x86-setup.exe` | x86  | `everything_mcp32.dll` |

To install, run the installer for your architecture; Everything then shows its
"Setup Plugin" dialog, where you click Install. Both installers can be
distributed together — `everything_mcp64.dll` and `everything_mcp32.dll` coexist
in `Plugins\`, and Everything picks the one matching its own bitness.
Uninstalling happens in Everything's options dialog under Plugins; the Plugins
folder itself is never touched.

Packaging needs a little more than the Rust toolchain (building the plugin dll
alone needs none of this):

- Visual Studio with the "Desktop development with C++" workload (provides
  `cl.exe` and `rc.exe`; the script picks up the environment via VsDevCmd)
- Windows SDK (`rc.exe` compiles the resource script)
- `rustup target add i686-pc-windows-msvc` (added automatically when missing)
- Either 7-Zip or Python (bz2-compresses the plugin dll before embedding it;
  the official scripts use 7-Zip)

#### Official plugins (Everything 1.5)

Everything's official plugin page:
<https://www.voidtools.com/support/everything/plugins/>

The official voidtools plugins below also target Everything 1.5 and install the
same way as this one (run the installer, or drop the plugin dll into the Plugins
folder and restart Everything):

| Plugin           | Version  | Description                                                              | Source                                                |
| ---------------- | -------- | ------------------------------------------------------------------------ | ----------------------------------------------------- |
| HTTP Server      | 1.0.5.6  | Search and access your files from a web browser                          | [voidtools/http_server](https://github.com/voidtools/http_server) |
| ETP/FTP Server   | 1.0.2.5  | Search and access your files from Everything or an FTP client            | [voidtools/etp_server](https://github.com/voidtools/etp_server)   |
| Everything Server | 1.0.4.5 | Let other Everything clients access your index (requires 1.5.0.1408 or later, plus a site license) | — |

Official installation instructions:

- **Installer**: download a plugin installer (`Setup.exe`), run it, click Add.
- **Manual**: download a plugin zip, extract the plugin dll, move it to
  `C:\Program Files\Everything\plugins` (the Plugins folder in your Everything
  installation folder), then exit Everything from the File menu and restart it.

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

**A URL is all you need.** The endpoint is dual-era: older MCP clients go
through the `initialize` handshake, while 2026-07-28-and-newer clients skip the
handshake and send requests directly — both connect fine, with no protocol
version to configure on the client side. The only prerequisite is that the
plugin is enabled (the Plugins → MCP page in Everything's options dialog, or
`mcp_enabled=1` in `Settings.ini`).

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

Argument rules shared by all three tools: `folder` must be an absolute path
(`C:\…` or `\\server\share\…`). Quoted, forward-slash, doubled-backslash and
trailing-slash spellings are normalized automatically; wildcards belong in
`pattern`, not in `folder`. A path that cannot be normalized comes back as
`-32602 INVALID_PARAMS` with the expected format and the received value in the
message — so the LLM fixes the argument in one round trip instead of retrying
with the same bad input.

#### Verifying by hand (curl)

Before wiring up a client, confirm the server is alive with curl. One request
per era:

```powershell
# legacy: initialize handshake (the old-client flow)
curl -X POST http://127.0.0.1:8285/ -H "Content-Type: application/json" -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}"

# modern: no handshake, straight to server/discover (version in both the header and _meta)
curl -X POST http://127.0.0.1:8285/ -H "Content-Type: application/json" -H "MCP-Protocol-Version: 2026-07-28" -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"server/discover\",\"params\":{\"_meta\":{\"io.modelcontextprotocol/protocolVersion\":\"2026-07-28\"}}}"

# modern: tools/list (the response result carries "resultType": "complete" plus the ttlMs / cacheScope cache hints)
curl -X POST http://127.0.0.1:8285/ -H "Content-Type: application/json" -H "MCP-Protocol-Version: 2026-07-28" -d "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{\"_meta\":{\"io.modelcontextprotocol/protocolVersion\":\"2026-07-28\"}}}"
```

The `supportedVersions` in the second response is the list of protocol versions
this endpoint speaks.

#### Troubleshooting a connection

| HTTP status | JSON-RPC code | Meaning and fix |
| --- | --- | --- |
| 200 | — | Normal response |
| 202 | — | Modern notification accepted (the spec requires an empty body) |
| 400 | `-32020` | `MCP-Protocol-Version` header and `_meta` version disagree — make both the same value |
| 400 | `-32022` | Requested protocol version not supported — retry with a version from `data.supported` |
| 403 | `-32600` | Request carried a non-localhost `Origin` header (a browser page was blocked) — use a local client instead |
| 404 | `-32601` | Unknown method in the modern era — check the method name spelling |
| 200 | `-32602` | Invalid tool arguments — the message carries the expected format and the received value; fix and retry |
| 400/405 | — | See "Debugging": anything but a POST to `/` or `/mcp` gets 405 |

If the server does not respond at all, first confirm `mcp_enabled=1` and that
the port is free (see "Debugging").

### Protocol versions (dual-era)

The same endpoint speaks two semantics, chosen by the protocol version the
request declares, so both client generations work:

| Client era              | How the version is declared                                            | Behavior                                                                 |
| ----------------------- | ---------------------------------------------------------------------- | ------------------------------------------------------------------------ |
| legacy (2024-11-05)     | `initialize` handshake, or no version information at all               | 200 + JSON-RPC response; unknown methods 200 + `-32601`; notifications get an `id: null` ack |
| modern (2026-07-28)     | `MCP-Protocol-Version` request header + `io.modelcontextprotocol/protocolVersion` in `_meta` | `server/discover` available; unknown methods 404; notifications 202 with empty body; header/body mismatch `-32020`; unsupported version 400 + `-32022` (`data.supported` lists the usable versions) |

**`resultType` (2026-07-28):** every modern-era response result carries
`resultType: "complete"` (a MUST from this revision onward; a missing field is
treated by clients as an invalid result and flagged). Legacy responses keep the
2024-11-05 shape, where clients apply the absent-means-complete rule and treat a
missing `resultType` as `"complete"`.

**`ttlMs` / `cacheScope` (2026-07-28):** `ListToolsResult` and `DiscoverResult`
extend `CacheableResult`, so in the modern era these two results must also carry
`ttlMs` (how long the client may cache the response, in milliseconds, analogous
to HTTP `Cache-Control: max-age`; 5 minutes for tools/list, 1 hour for discover)
and `cacheScope: "public"` (the results hold no user-specific data, so they may
be cached across authorization contexts). Legacy responses keep the original
shape without these fields.

Other security and compatibility rules: requests carrying an `Origin` header
from anywhere but localhost are rejected with 403 (DNS rebinding protection);
`Mcp-Method` / `Mcp-Name` mirror headers, when present, are checked against
the request body (non-ASCII values use the `=?base64?…?=` sentinel); and
`tools/call` argument validation (path normalization) happens before anything
reaches the Everything host, so a malformed argument never becomes a real
search.

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

The `reference/` directory holds the official voidtools C plugin sources (none
of it is compiled into this crate). The main-thread
marshaling scheme and the `db_query_search2` parameter list used here were
derived from `reference/etp_server-1.0.2.5`. Licenses for each item are listed in
[`reference/README.md`](reference/README.md).

### License

MIT, see [LICENSE](LICENSE).
