# MCP tools and protocol (detailed)

Split from the main README so the homepage stays compact. Back to: [README.en.md](../README.en.md)

## Tools (detailed)

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

`exclude` (`search_in_folder` / `list_folder` / `count` / `grep` / `search_everywhere`): a string or an array of strings,
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
    server-side gate, effective immediately); the other seven tools keep
    working (folder-scoped search / list / count / journal / read / grep /
    diagnostics). The error payload names the fix, so the LLM can relay it to
    the user.
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

#### 8. `server_diagnostics`

Server self-diagnostics: argument-free, read-only, never touches the Everything
database. It aggregates current configuration and state so the LLM can triage a
misbehaving tool on its own before deciding whether to retry, switch tools, or
ask the user to open the options page:

- `server`: plugin version (same source as serverInfo), supported protocol
  versions, whether the HTTP server is **actually listening**
  (`listening` / `bound_port`) plus the configured values (`enabled` / `bind` /
  `configured_port`) — `listening: false` means the server was never enabled or
  Apply failed (e.g. a busy port).
- `global_search`: the current mode for `search_everywhere` with a one-line
  note (the Deny tier carries enablement instructions).
- `index_changes_prerequisite`: the journal log directory path and **whether it
  exists** — a missing directory almost always means `journal_log` is off,
  which is the most common cause of empty `index_changes` results.
- `diagnostic_log`: the path of `%LOCALAPPDATA%\everything-mcp\plugin.log`,
  the first stop when chasing "why didn't the plugin start" (see Debugging).
- `stats`: lifetime counters — total calls, requests, connections, plus
  `calls` / `ok` / `err` / `avg_ms` per tool. A tool that keeps erring is a
  strong hint.

Argument rules shared by tools that take `folder`: `folder` must be an absolute path
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

**Bind address and credential boundaries (evaluate honestly):** the settings
page lets you set `mcp_bind` to `0.0.0.0` (all interfaces), but there is
**no authentication** today (no token / password) — only the browser `Origin`
check blocks DNS rebinding; any LAN HTTP client can still call the tools.
Keep `127.0.0.1` unless you know what you are doing. The sensitive-path
denylist used by `read_file` / `grep` is intentionally narrow (private keys,
`.env`, common keystores, `.git\objects`) and does **not** cover files like
`~/.aws/credentials`, `.npmrc`, or `.docker/config.json`. Outside that list,
the tools can read any path the Everything process can read.

### Known limitations

Behavior boundaries collected from the per-tool docs — worth a read before wiring up:

- **Search results are capped by Everything's index**. Only indexed locations are
  searched: all local NTFS drives plus network shares added under Everything >
  Options > Indexes > Folders. Un-indexed (or currently offline) shares return
  0 results — tools attach a `folder_warning` on empty results to help you tell
  "genuinely nothing" from "not indexed".
- **`index_changes` depends on Everything's change journal**. Enable journal_log
  in Everything's options (Indexes > Journal) first; without it there is no log
  to parse and only empty results come back.
- **`search_everywhere`'s review tier relies on the client honoring it**. The
  server only publishes tool annotations (`readOnlyHint` / `destructiveHint`);
  the confirmation dialog is the MCP client's job. Only the deny tier is
  enforced server-side — this is an inherent boundary of MCP annotations.
- **The transport is simple one-request-per-connection HTTP**: `Connection:
  close`, no SSE channel, no GET notification stream (405). Concurrency is
  capped at 64; excess connections get 503 immediately. Fine for a localhost,
  serially-calling MCP client — do not use it as a high-concurrency web service.
- **The HTTP server defaults to this machine only**. The bind address defaults
  to the loopback; non-localhost `Origin` headers get 403 (DNS-rebinding
  protection). If you deliberately bind `0.0.0.0`, the endpoint is reachable on
  the LAN **with no auth** — see "Bind address and credential boundaries"
  above. Public remote exposure is not a design goal.
- **Text only**. `read_file` refuses binary content (by extension and content
  sniffing) and clips lines longer than 16 KiB; `grep` likewise skips binary
  files and files above 8 MiB.

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
