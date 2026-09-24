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
  the tools are folder-scoped by design instead of scanning the whole disk. An
  optional global name search (`search_everywhere`) covers the "I know what the
  file is called but not which share it lives in" case — off by default,
  governed by a three-mode Deny / Review / Allow switch (see Configuration).
- **Settings page** — enable switch, bind address, port and the global-search
  mode are configurable in Everything's own options dialog; clicking Apply
  takes effect immediately, without restarting Everything.
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
│   │                            (enable/bind/port/global-search mode/restore defaults) + settings state
│   ├── plugin/
│   │   ├── mod.rs               Submodule summary + PM_* constants
│   │   ├── diag.rs              Disk diagnostic log (%LOCALAPPDATA%\everything-mcp\plugin.log)
│   │   ├── ffi_types.rs         #[repr(C)] types (Utf8Buf/DbHandle/FileInfoFd...)
│   │   ├── grep.rs              grep: Everything picks candidates, regex matches lines
│   │   ├── host.rs              Host function pointer table + HOST/HOST_LOCK
│   │   ├── journal.rs           Index journal (index-journal-*.txt) parsing & querying
│   │   ├── read.rs              read_file: single-file content read (line window, size cap)
│   │   ├── search.rs            Async search → sync wait wrapper (main-thread marshaling)
│   │   ├── sensitive.rs         Sensitive-path denylist (keys, .env, credentials, .git/objects)
│   │   ├── main_thread.rs       Main-thread window + PostMessage task dispatch
│   │   └── state.rs             Runtime state (lazily created db/query, shutdown flag)
│   └── mcp/
│       ├── mod.rs
│       ├── protocol.rs          JSON-RPC 2.0 + dual-era MCP types (2024-11-05 / 2026-07-28)
│       ├── server.rs            HTTP server on std::net; per-request version
│       │                         negotiation and Origin validation
│       ├── tools.rs             Tool implementations (search_in_folder / list_folder / count / index_changes / read_file / grep / search_everywhere)
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
| `everything-mcp-1.1.7-x64-setup.exe` | x64  | `everything_mcp64.dll` |
| `everything-mcp-1.1.7-x86-setup.exe` | x86  | `everything_mcp32.dll` |

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
| `mcp_global_search` | int | `0`     | Global-search mode: `0` deny (default — `search_everywhere` calls return `GLOBAL_SEARCH_DISABLED`) / `1` review (usable, but the tool is annotated as needing user confirmation, so the client prompts first) / `2` allow (served directly, no prompt) |

Settings live in `%APPDATA%\Everything\Plugins.ini`, in this plugin's own
section `[everything_mcp64.dll]` (`[everything_mcp32.dll]` on 32-bit) — not in
Everything's `Settings.ini`, and not shared with the official http_server
plugin's section. They can also be edited in Everything's options dialog under
Plugins → MCP (enable switch, bind address, port, global-search mode, restore
defaults); clicking
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

Once connected, the LLM discovers seven tools automatically:

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
- **Don't know which folder?** Locate the file by name with
  `search_everywhere` (section 7; needs to be enabled in the settings), then
  come back with the full path for scoped searches.
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
  (default 0) and `max_results` is 1..2000 (default 50; `0` is a mistake, not
  "unlimited"). The response reports `count` (entries returned), `total` (all
  matches found) and `truncated` (true when more remain), so advance `offset`
  by `count` and query again. **Prefer narrowing `pattern` / `exclude` to
  paging**: every page re-runs the search, and a single call carries a fixed
  cost of roughly 50 ms, so paging multiplies that cost (measured: 4419 entries
  in 500-entry pages takes ~450 ms, while fetching them in one call takes 88 ms).
- **The entries also carry a 512 KiB byte budget**: on top of the entry count
  there is a limit measured in bytes, so unusually long paths can come back
  fewer than `max_results` — `truncated` is `true` in that case too.

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
(default 10000; it must be an integer in 1..4294967295 — 0 and out-of-range
values are rejected rather than silently mangled).

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
- **Page through big directories**: `max_results` defaults to 500 (and caps at
  2000), with `offset` to move on. When `count` < `total` there is another page,
  and `truncated` states that outright. A busy directory can hold thousands of
  children (`C:\Windows\System32` measures ~4900), and the older fixed cap of
  500 with no `offset` made everything past the 501st child unreachable.
- `timeout_ms` defaults to 10000, matching the other three search tools (this
  call used to be pinned to 10 s, with no way to widen it for a slow share).

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
{ "path": "D:\\source\\repos\\my-project\\README.md", "start_line": 1, "max_lines": 2000 }
```

- `path` must be an **absolute path** to a **single existing file**.
  Wildcards (`*` / `?`) are rejected with a pointer to `search_in_folder`, a
  folder is rejected with a pointer to `list_folder`, and a relative path is
  an error. Those pure argument problems come back as `-32602
  INVALID_PARAMS` before anything touches the disk. Runtime problems (missing
  file, binary file, over the size limit, denylisted path) come back as a
  tool error (`isError: true`) whose body is structured JSON —
  `{ "error": "…", "code": "NOT_FOUND" }`. **Branch on `code`, not on the
  wording**: `NOT_FOUND` / `PATH_IS_DIRECTORY` / `PATH_DENIED` /
  `BINARY_CONTENT` / `TOO_LARGE` / `READ_FAILED`. The directory case also
  carries `suggested_tool: "list_folder"` and `suggested_args`, so a
  recovering agent needs no extra round trip.
- 🔒 **Sensitive-path denylist**: private keys and credential bundles are
  **never** returned — `id_rsa`/`id_dsa`/`id_ecdsa`/`id_ed25519`, `.env`
  (except `.env.example` / `.sample` / `.template` / `.dist`), `.netrc`,
  `.git-credentials`, `credentials.json`, `*.pem` / `*.key` / `*.p12` /
  `*.pfx` / `*.jks` / `*.keystore` / `*.ppk`, and everything under
  `.git/objects`. A hit comes back as `PATH_DENIED`, refused **before any
  stat** — the path is not even touched. **The list is not configurable**: it
  is the only thing standing between this LLM-facing read tool and a prompt
  injection that tries to exfiltrate `~/.ssh/id_rsa`. It applies to content
  reads only — search results do not hide these files, because the file name
  is not the secret; the content is.
- **8 MiB per file**, refused above that (`TOO_LARGE`) — a large file is
  never read into memory. The response is additionally capped at a **128 KiB
  byte budget**, accumulated **whole lines at a time**, so the budget never
  cuts a line in half.
- **Overlong lines are cut**: a line longer than 16384 characters is
  truncated, and `clipped_lines` reports how many were (so a minified
  single-line file cannot swallow the window). A clipped line still counts
  as one line in `lines_returned`.
- **Paging**: `start_line` is the 1-based first line to return (default 1)
  and `max_lines` the number of lines (default **2000**, max **4000**). The
  line count is the coarse filter — **the 128 KiB budget is the real
  limiter**; whichever comes first wins, and it always stops on a whole line.
  The response hands you `next_start_line` — pass it straight back as
  `start_line` for the next page, and stop when it is `null`. No arithmetic
  needed. A `start_line` past the end returns an empty window rather than an
  error.
- **The response has two content items**: a metadata JSON object (`path` /
  `size` / `total_lines` / `start_line` / `lines_returned` /
  `next_start_line` / `truncated` / `clipped_lines` / `encoding`) followed by
  the text itself. The body is a separate item so it stays verbatim — inside
  JSON every newline would become `\n`, which is both harder to read and easy
  to mangle when quoting later. `truncated` means only "there is more beyond
  this window"; it is a different thing from `clipped_lines`.
- **`encoding` reports how the bytes were decoded**: `utf-8` / `utf-16le` /
  `utf-16be` are certain (valid UTF-8, or an explicit BOM); `ansi` means the
  bytes were neither valid UTF-8 nor BOM-prefixed, so **the machine's ANSI
  code page** was assumed (that is GBK on a Chinese Windows, which is what
  older documents and logs use); `utf-8-lossy` means some bytes were
  undecodable and became `\uFFFD`. Treat an `ansi` or `utf-8-lossy` body as a
  guess before quoting it.
- **Binary files are rejected** by two independent checks: **extension**
  (documents, archives, executables, media, fonts and databases —
  `.pdf`/`.docx`/`.xlsx`/`.zip`/`.exe`/`.dll`/`.png`/`.mp4`/`.ttf`/`.sqlite`
  and friends) and **content sniff** (a NUL byte, or over 30% non-printable
  bytes in the first 4 KiB). Text formats (`.json`/`.xml`/`.svg`/`.log`/
  `.csv`/`.md`/`.dat`) are never in the extension table, so they are not
  blocked by accident, and a UTF-16 file with a BOM is exempt from the
  sniff because its raw bytes are full of NULs. Both checks were added after
  a measurement: with only the old "contains NUL" rule, a 37-byte all-ASCII
  PDF was returned as file content.
- `truncated` means **content remains beyond this window** (its companion is
  `next_start_line`); it is not a signal that something was cut mid-line —
  that is `clipped_lines`. The two are independent.
- To find files *by their contents* use `search_in_folder` with
  `content:"…"` (see
  [Content search and making it fast](#content-search-and-making-it-fast));
  `read_file` only reads the one file you name.
- ⚠️ Apart from that denylist, this tool can read **any** absolute path the
  Everything process can read, not only indexed files. The server listens on
  `127.0.0.1` by default (see Configuration) — do not expose it to a network.

#### 6. `grep`

Search file contents by regular expression and return the **matching lines with
their line numbers**. It is the line-level companion to `search_in_folder`:
that one uses Everything's index to answer *which files* contain something
(paths only), while `grep` reads the candidate files and tells you *which line*.

```json
{
  "folder": "D:\\source\\repos\\my-project",
  "pattern": "fn\\s+main",
  "filter": "ext:rs",
  "output_mode": "content",
  "head_limit": 200
}
```

- **`pattern` is the regex; `filter` is Everything syntax** — the two are easy
  to mix up. `pattern` is matched against each line separately (so `^` / `$`
  anchor to line boundaries, and a pattern containing a literal newline can
  never match); `filter` is the candidate filter handed to Everything, with
  exactly the same syntax as `search_in_folder`'s `pattern` (`ext:rs;toml`,
  `dm:lastweek`, `!\target\`), and it applies **before any file is read**.
- **Always pass `filter`.** Without it, every file under `folder` gets read —
  the same trap as an unscoped `content:` search. Three internal caps bound
  the damage: at most 2000 candidate files, 64 MiB read, and a **128 KiB
  budget on the matched text returned** (the same constant `read_file` uses).
  When one bites, `truncated` is `true`, and `candidates` / `files_scanned` /
  `bytes_scanned` tell you which. Note that the 128 KiB counts the matched
  text only; the JSON envelope (paths, line numbers, escaping) sits on top, so
  the actual response is somewhat larger.
- **`output_mode`** picks the shape:
  - `content` (default): a `matches` array of `{ path, line, text }`;
  - `filesWithMatches`: a `files` array of paths only;
  - `count`: a `counts` array of `{ path, count }`.
- `head_limit` caps matches (`content`) or files (other modes); default 200,
  max 2000. Matched lines longer than 16384 characters are cut, and
  `clipped_lines` reports how many.
- Results are ordered by file **modification time, newest first** — the file
  you just touched is the likeliest target.
- **Binary files, files above 8 MiB and denylisted paths are skipped
  silently** (no error, and the search is not aborted). The denylist applies
  here too: content from `.env` files or private keys never shows up in
  results.
- `case_insensitive` defaults to `false`; `exclude` works as in the other
  tools.

#### 7. `search_everywhere`

Search by **file name** across the entire Everything index — every local drive
and indexed network share, no folder needed. It answers "I know what the file
is called but not which share it lives in" (otherwise an agent can only
`net view \\NAS` and probe shares one by one). Results come back with **full
paths**, ready to hand to `search_in_folder` / `list_folder` for scoped
follow-up work.

```json
{
  "pattern": "*.vhd",
  "exclude": ["\\\\old\\\\"],
  "sort": "modified",
  "descending": true,
  "max_results": 50
}
```

- **Off by default, governed by a three-mode global-search switch**
  (Everything → Options → Plugins → MCP → global search):
  - **Deny** (default): every call returns `GLOBAL_SEARCH_DISABLED` (a hard
    server-side gate, effective immediately); the other six tools are
    unaffected and stay folder-scoped. The error payload names the fix, so the
    LLM can relay it to the user.
  - **Review**: calls go through, but the tool is annotated as NOT read-only /
    destructive (`destructiveHint: true`), so the client shows its permission
    prompt first. Note that annotations are **untrusted hints** by the spec —
    2026-07-28 states: *clients MUST consider tool annotations to be untrusted
    unless they come from trusted servers*. A client that reads them will ask
    first; one that ignores them will call straight through. **Deny is the only
    tier the server actually enforces.** Nothing on disk is modified either way
    — the annotation only drives the prompt.
  - **Allow**: annotated read-only (`readOnlyHint: true`); calls are served
    directly, without bothering the user.
  The mode only changes the tool annotations and the POLICY paragraph in the
  description; the tool-list shape never changes. Note the `tools/list` result
  is cached for 5 minutes, so annotations can lag a switch by up to one TTL —
  but the Deny gate is checked at call time and bites immediately.
- **Two guard rails**: `pattern` must be at least 2 characters (a global empty
  or single-character pattern matches a catastrophic number of entries), and
  **`content:` is rejected in both `pattern` and `exclude`** — an excluded
  content term still forces Everything to evaluate file contents, so both
  arguments are checked; content search always stays folder-scoped in
  `search_in_folder`. Both come back as `-32602 INVALID_PARAMS` before anything
  touches Everything. Note the 2-character rule only blocks empty / one-char
  patterns and is **not a scope control**: `*.`, `a*` and `dm:thisyear` are all
  legal and match enormous sets — the real controls are the mode gate and the
  `max_results` cap (2000), with `offset` paging through the rest.
- Same argument shape as `search_in_folder`: `exclude` / `sort` / `descending`
  / `match_case` / `match_whole_word` / `match_regex` / `offset` / `timeout_ms`
  all mean the same thing, and `max_results` shares the same hard window as the
  other two search tools (1..2000): `0` or out-of-range is a plain `-32602`,
  not "unlimited".
- Each result carries `name` / `path` / `kind` / `size` / `modified` /
  `created` (ISO 8601 UTC timestamps), with the same `count` / `total` pair
  and `offset` paging.

Argument rules shared by all three tools: `folder` must be an absolute path
(`C:\…` or `\\server\share\…`) — UNC network shares are on equal footing with
local drives. Search scope follows Everything's index: local drives are
included automatically, while a network share must be added under
Tools → Options → Indexes → Folders first — an un-indexed share yields
0 results, not an error. Quoted, forward-slash, doubled-backslash and
trailing-slash spellings are normalized automatically; wildcards belong in
`pattern`, not in `folder`. A path that cannot be normalized comes back as
`-32602 INVALID_PARAMS` with the expected format and the received value in the
message — so the LLM fixes the argument in one round trip instead of retrying
with the same bad input.

Every `exclude` term also gets its doubled backslashes collapsed
(`\\target\\` → `\target\`). That is not fussiness: before 1.1.5 that spelling
**silently did nothing** — measured on one query, no exclude returned 30 hits,
`\target\` returned 22, and `\\target\\` returned 30 again, with no error.
Collapsing is safe for UNC excludes too (`!\\NAS\old\`) because Everything
substring-matches the full path, so `\NAS\old\` and `\\NAS\old\` hit the same
files. `pattern` is deliberately left alone: under `match_regex` it may be a
regex, where collapsing `\\.` to `\.` would change its meaning.

Every `index_changes` argument is optional, and the same validation rule
applies: a bad `action`, an out-of-range `max_results` (1–2000), a
malformed timestamp or `since` later than `until` all come back as
`-32602` before any file is touched. Every numeric window also declares its
`minimum` / `maximum` in the JSON Schema, so clients reject out-of-range values
up front instead of waiting for the server to complain.

**Empty results explain themselves.** When `search_in_folder` / `list_folder` /
`count` / `grep` match nothing, they take one look at the folder: if it does not
exist, cannot be accessed, or is not a directory at all, the response carries a
`folder_warning` saying so — an empty directory and a misspelled path no longer
look identical. **It is a soft signal, not an error**, and that distinction
matters: a network share that was never added to the index, or one that is
currently offline, is *supposed* to return 0 results — that is the NAS use case,
and erroring out would misreport it as a bad argument. The probe only runs when
the result is already empty, so a normal search pays nothing for it; conversely,
that single stat against an offline share can block until the SMB timeout, which
is why the cost is confined to calls that found nothing anyway.

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
