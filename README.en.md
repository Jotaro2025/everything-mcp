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
- **Index change queries** — `index_changes` answers "what changed lately":
  file/folder creations, modifications, deletions, renames and moves, newest
  first, filterable by action, path prefix, name substring and time window.
- **Timestamps, sorting and paging** — every search result carries
  modification / creation time (ISO 8601 UTC), results can be sorted by
  name / path / size / time (ascending or descending), large result sets page
  through with `offset`, and matching supports case-sensitive, whole-word and
  regex switches.
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
│   │   ├── journal.rs           Index journal (index-journal-*.txt) parsing & querying
│   │   ├── read.rs              read_file: single-file content read (line window, size cap)
│   │   ├── search.rs            Async search → sync wait wrapper (main-thread marshaling)
│   │   ├── main_thread.rs       Main-thread window + PostMessage task dispatch
│   │   └── state.rs             Runtime state (lazily created db/query, shutdown flag)
│   └── mcp/
│       ├── mod.rs
│       ├── protocol.rs          JSON-RPC 2.0 + dual-era MCP types (2024-11-05 / 2026-07-28)
│       ├── server.rs            HTTP server on std::net; per-request version
│       │                         negotiation and Origin validation
│       ├── tools.rs             Tool implementations (search_in_folder/list_folder/count/index_changes/read_file)
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
| `everything-mcp-1.1.2-x64-setup.exe` | x64  | `everything_mcp64.dll` |
| `everything-mcp-1.1.2-x86-setup.exe` | x86  | `everything_mcp32.dll` |

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

Settings live in `%APPDATA%\Everything\Plugins.ini`, in this plugin's own
section `[everything_mcp64.dll]` (`[everything_mcp32.dll]` on 32-bit) — not in
Everything's `Settings.ini`, and not shared with the official http_server
plugin's section. They can also be edited in Everything's options dialog under
Plugins → MCP (enable switch, bind address, port, restore defaults); clicking
Apply takes effect immediately — no Everything restart needed.

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
`mcp_enabled=1` under `[everything_mcp64.dll]` in
`%APPDATA%\Everything\Plugins.ini`).

Once connected, the LLM discovers five tools automatically:

#### 1. `search_in_folder`

Recursively find files/folders under a given folder using Everything search syntax.

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

- `pattern` supports the full Everything syntax: `*.rs`, `"readme"`,
  `ext:md;txt`, `dm:lastweek`, `size:>1mb`, `content:"fn main"`, …
- **Not shell glob**: write `*.rs`, not `**/*.rs` — the folder scope
  already restricts the tree. A leading `**/`, `**\` or `./` is stripped
  for you (this is exactly why `**/Program.cs` returned 0 hits in the
  eval); a globstar in the middle of a pattern cannot be translated
  faithfully and is passed through as-is.
- Exclude matches with a `!` prefix (e.g. `ext:rs !test`), or use the
  `exclude` parameter (below).
- `content:` works **out of the box** — you do not need content indexing on
  the Everything side first. But an unfiltered content search is slow enough
  to time out, so see
  [Content search and making it fast](#content-search-and-making-it-fast)
  below for the syntax and the speedups.
- An empty string `""` lists everything under the folder.
- **Every result carries `modified` / `created`** (modification / creation
  time, ISO 8601 UTC, e.g. `2026-09-23T11:18:31Z`; `null` when the index
  entry has no such time. Everything does not index creation time by
  default, so `created` is often `null` while `modified` is present).
- **Sorting**: `sort` is one of `name` (default) / `path` / `size` /
  `modified` / `created`, and `descending: true` reverses it. "Recently
  changed files" is `"sort": "modified", "descending": true`; "largest
  files" is `"sort": "size", "descending": true`.
- **Match switches**: `match_case` (case-sensitive), `match_whole_word`
  (whole words) and `match_regex` (regular expression, implemented via
  Everything's `regex:` search function) — all default `false`. You can
  also write `case:` / `ww:` / `regex:` directly in the pattern, but
  **`case:` must not be followed by a space**: `case:content:"x"` applies,
  while `case: content:"x"` is silently ignored and the search comes back
  case-insensitive.
- **Paging**: `offset` is the index of the first result to return
  (default 0). `max_results = 0` means unlimited (careful with huge
  folders). The response reports `count` (entries returned, capped by
  `max_results`) and `total` (all matches found) — when they differ there
  is more, so advance `offset` by `count` and query again.

`exclude` (all three tools accept it): a string or an array of strings,
each appended as an Everything NOT term. Repository noise is in the
results by default (Everything knows nothing about gitignore), so:

```json
{
  "folder": "D:\\source\\repos\\my-project",
  "pattern": "*.cs",
  "exclude": ["\\obj\\", "\\.git\\", "\\node_modules\\"]
}
```

Exclude terms match path fragments literally — keep the surrounding
backslashes (`\obj\`) so that files merely *named* `obj…` are not
dropped. The equivalent pattern spelling is `*.cs !\obj\ !\.git\`.

##### Content search and making it fast

`content:` is Everything's file-content search function. The plugin already
passes the `allow_read_access` permission bit it needs, so it works
**without building a content index first** — with no index, Everything opens
candidate files on demand and the results are still correct, just slow. The
slowness comes from the size of the candidate set, so speed it up in this
order.

**1. Narrow the scope first (most effective, zero setup).** Limit the
extension with `ext:`, point `folder` at a subdirectory, drop `\target\`,
`\.git\` and `\node_modules\` with `exclude`, or restrict to recently
modified files with `dm:lastweek`. Measured on one repository:

| pattern | result |
| --- | --- |
| `content:"db_query_search2"` (repo root, including `target/`) | times out at the default 10s |
| `ext:rs content:"db_query_search2"` | returns 9 files immediately |

**2. Raise `timeout_ms`.** If it still times out after narrowing, increase it
(default 10000, no upper bound is enforced).

**3. Turn on content indexing (permanent, but only for the indexed
folders).** In Everything, go to Tools → Options → Indexes → Folders, select
the folder and tick "Index file content" (newer 1.5 builds moved these
options to the Advanced page — search for `content` in the options dialog if
you cannot find them). The ini keys are `content_indexing_enabled`,
`content_indexing_include_only_files` and `content_indexing_max_size`. Two
traps:

- **Add your source extensions to `content_indexing_include_only_files`.**
  Its default is only `*.doc;*.docx;*.pdf;*.txt;*.xls;*.xlsx` — no `.rs`,
  `.cs`, `.py` or `.md`. Without them those files still go through on-demand
  reading, so the index buys you nothing.
- Building the index makes Everything read every candidate file once, so
  **the first build is markedly slower**; and an ini edit only takes effect
  after **restarting Everything** (a running instance writes its in-memory
  values back over the file on exit).

Content-search spellings that work (all measured on 1.5.0.1422b):

| spelling | meaning |
| --- | --- |
| `content:"fn main"` | content contains the text (case-insensitive) |
| `case:content:"Fn Main"` | case-sensitive content search — no space after `case:` |
| `utf8content:"fn main"` | read content as UTF-8 (code / scripts) |
| `ansicontent:"系统日志"` | read content as ANSI/GBK (older Chinese documents) |
| `regex:content:"\d{3}-\d{2}-\d{4}"` | regex match against content |

The run-together `casecontent:"…"` spelling does **not** work on
1.5.0.1422b (it always returns 0 hits) — use `case:content:` instead, or
the tool's `match_case: true` parameter.

#### 2. `list_folder`

List the direct children (non-recursive) of a folder: name, kind, size,
modification / creation time.

```json
{ "folder": "D:\\source\\repos\\my-project", "exclude": ["\\.git\\"] }
```

- Folder entries always report `size` 0 — Everything does not compute
  directory sizes; that is the convention, not missing data. Use
  `search_in_folder` for the whole tree.
- Each entry also carries `modified` / `created` (ISO 8601 UTC, `null`
  when unavailable).
- The response also carries `total` (number of direct children, after
  exclusions).

#### 3. `count`

Count matching files without returning the name list. Only the hit count
is taken — no per-entry names or paths are materialized. Supports the
same `pattern` and `exclude` arguments as `search_in_folder`.

```json
{ "folder": "D:\\source\\repos\\my-project", "pattern": "ext:rs" }
```

#### 4. `index_changes`

Query the Everything index journal: which files/folders were created,
modified, deleted, renamed or moved, **most recent first**. It answers
"what changed", not "what exists" — for the current state use
`search_in_folder`.

```json
{ "action": "created", "path": "D:\\source\\repos\\my-project", "max_results": 50 }
```

- Needs Everything to log index changes: Options → Index → Journal →
  Log changes (or `journal_log=1` under `[Everything]` in
  `%APPDATA%\Everything\Everything.ini`, then restart Everything).
  **When the switch is off this tool returns an error naming the exact
  fix instead of an empty result** — so confirm it is on before the first
  call.
- **Log writes are delayed, measured at roughly 8–70 seconds**: Everything
  does not append to the text log in real time, so something that just
  happened may not be in there yet. **An empty result does not mean
  "nothing happened"** — when you get one, wait a minute and query
  again, or confirm the file's current state with `search_in_folder` /
  `count` first.
- `action` takes `created` / `modified` / `deleted` / `renamed` / `moved` /
  `any` (default `any`). Everything distinguishes "renamed" (same folder)
  from "moved" (different folder); each entry also carries `action_text`
  with Everything's original localized action label, so locales the
  interface is not set to still read sensibly.
- `path` is a case-insensitive path prefix; `name` is a file/folder-name
  substring (renames match the new name too). `since` / `until` bound the
  window inclusively and accept `2026-09-23`, `2026-09-23 11:18`,
  `2026-09-23 11:18:31` (space or `T` between date and time); a bare
  number is read as unix seconds.
- The response carries `count`, `truncated` (true when more matching
  history exists beyond `max_results`), `days_searched` (how many daily
  logs were read), `skipped_lines` (lines that failed to parse, usually a
  half-written last line) and `log_directory` (the directory actually
  read). The log is scanned backwards in chunks, so "the last N changes"
  reads only a small piece of the file tail rather than the whole log.
- History depth follows Everything's own retention: one log file per day,
  and this tool reads back at most 92 days, up to 64 MiB per file.

#### 5. `read_file`

Read the contents of **one** text file, optionally just a window of lines.
It is the companion to `search_in_folder`: search to locate files, then read
one — a search itself returns only paths and metadata, never content.

```json
{ "path": "D:\\source\\repos\\my-project\\README.md", "start_line": 1, "max_lines": 200 }
```

- `path` must be an **absolute path** to a **single existing file**.
  Wildcards (`*` / `?`) are rejected with a pointer to `search_in_folder`, a
  folder is rejected with a pointer to `list_folder`, and a relative path is
  an error. Those pure argument problems come back as `-32602
  INVALID_PARAMS` before anything touches the disk; a missing file, a binary
  file or one over the size limit comes back as a tool error (`isError:
  true`).
- **8 MiB per file**, refused above that — a large file is never read into
  memory. The response is additionally capped at 512 KiB of text (minified
  single-line files, very long log lines); when either cap bites,
  `truncated` is `true`.
- **Paging**: `start_line` is the 1-based first line to return (default 1)
  and `max_lines` the number of lines (default 200; `0` means all
  remaining). `total_lines` and `lines_returned` in the response tell you
  whether more remains; a `start_line` past the end returns an empty window
  rather than an error.
- **The response has two content items**: a metadata JSON object (`path` /
  `size` / `total_lines` / `start_line` / `lines_returned` / `truncated` /
  `encoding`) followed by the text itself. The body is a separate item so it
  stays verbatim — inside JSON every newline would become `\n`, which is
  both harder to read and easy to mangle when quoting later.
- **`encoding` reports how the bytes were decoded**: `utf-8` / `utf-16le` /
  `utf-16be` are certain (valid UTF-8, or an explicit BOM); `ansi` means the
  bytes were neither valid UTF-8 nor BOM-prefixed, so **the machine's ANSI
  code page** was assumed (that is GBK on a Chinese Windows, which is what
  older documents and logs use); `utf-8-lossy` means some bytes were
  undecodable and became `\uFFFD`. Treat an `ansi` or `utf-8-lossy` body as a
  guess before quoting it.
- **Binary files are rejected** (content containing NUL bytes) with an
  error instead of a page of garbage.
- To find files *by their contents* use `search_in_folder` with
  `content:"…"` (see
  [Content search and making it fast](#content-search-and-making-it-fast));
  `read_file` only reads the one file you name.
- ⚠️ This tool can read **any** absolute path the Everything process can
  read, not only indexed files. The server listens on `127.0.0.1` by default
  (see Configuration) — do not expose it to a network.

Argument rules shared by all three tools: `folder` must be an absolute path
(`C:\…` or `\\server\share\…`). Quoted, forward-slash, doubled-backslash and
trailing-slash spellings are normalized automatically; wildcards belong in
`pattern`, not in `folder`. A path that cannot be normalized comes back as
`-32602 INVALID_PARAMS` with the expected format and the received value in the
message — so the LLM fixes the argument in one round trip instead of retrying
with the same bad input.

Every `index_changes` argument is optional, and the same validation rule
applies: a bad `action`, an out-of-range `max_results` (1–2000), a
malformed timestamp or `since` later than `until` all come back as
`-32602` before any file is touched.

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
