<h1 align="center">everything-mcp</h1>

<p align="center">
  <strong>In-process MCP plugin for Everything 1.5</strong><br>
  Expose local file search to Cursor, Claude Desktop, and other LLM clients via the Model Context Protocol.
</p>

<p align="center">
  <a href="https://github.com/Jotaro2025/everything-mcp/stargazers"><img src="https://img.shields.io/github/stars/Jotaro2025/everything-mcp?style=flat-square&logo=github&label=Stars" alt="GitHub Stars"></a>
  <a href="https://github.com/Jotaro2025/everything-mcp/forks"><img src="https://img.shields.io/github/forks/Jotaro2025/everything-mcp?style=flat-square&logo=github&label=Forks" alt="GitHub Forks"></a>
  <a href="https://github.com/Jotaro2025/everything-mcp/releases"><img src="https://img.shields.io/github/downloads/Jotaro2025/everything-mcp/total?style=flat-square&label=Downloads" alt="GitHub Downloads"></a>
  <a href="https://github.com/Jotaro2025/everything-mcp/releases/latest"><img src="https://img.shields.io/github/v/release/Jotaro2025/everything-mcp?style=flat-square&label=Release" alt="Latest Release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/Jotaro2025/everything-mcp?style=flat-square&label=License" alt="License"></a>
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/Rust-1.3.0-dea584?style=flat-square&logo=rust&logoColor=white" alt="Rust"></a>
</p>

<p align="center">
  <a href="https://github.com/Jotaro2025/everything-mcp/releases/latest"><img src="https://img.shields.io/badge/Download-Latest%20Release-0969DA?style=for-the-badge&logo=github&logoColor=white" alt="Download latest release"></a>
  <a href="#install"><img src="https://img.shields.io/badge/Docs-Install-334155?style=for-the-badge&logo=readthedocs&logoColor=white" alt="Install"></a>
  <a href="docs/mcp-tools.en.md"><img src="https://img.shields.io/badge/Docs-MCP%20Tools-0F766E?style=for-the-badge&logo=json&logoColor=white" alt="MCP tools"></a>
</p>

<p align="center">
  <a href="README.md">中文</a> |
  <a href="docs/mcp-tools.en.md">Tool details</a> |
  <a href="RELEASE_NOTES.md">Release notes</a>
</p>

<p align="center">
  <a href="https://linux.do/"><img src="docs/images/linux-do-banner.png" alt="LINUX DO — Where possible begins." width="520"></a>
</p>

> [!NOTE]
> Requires [Everything 1.5](https://www.voidtools.com/). The plugin listens on `127.0.0.1` by default and keeps the MCP service **off** until you enable it in Options.

## Why everything-mcp

| Capability | Notes |
|---|---|
| In-process plugin | Single `everything_mcp64.dll` loaded by Everything via `LoadLibrary`. |
| Zero third-party DLLs | Only Windows system libraries; Rust / `serde` / CRT are statically linked. |
| Folder-first tools | Prefer scoped search; optional global name search with deny / review / allow. |
| GUI settings | Enable switch, bind address, port, and global-search mode apply immediately. |
| Dual-era MCP | Same endpoint serves 2024-11-05 (`initialize`) and 2026-07-28 (`_meta` + `server/discover`). |
| Packaged installers | Same `-setup-plugin` flow as official Everything plugins (x64 / x86). |

## Screenshots

### Install — add plugin

<p align="center">
  <img src="docs/images/install-add-plugin.png" alt="Everything Add Plugin dialog for Everything MCP 1.3.0" width="560">
</p>

### Config — MCP Server options

<p align="center">
  <img src="docs/images/config-mcp-server.png" alt="Everything Options → Plugins → MCP Server" width="720">
</p>

### Stats — per-tool invocations

<p align="center">
  <img src="docs/images/stats-mcp-server.png" alt="Everything Options → Plugins → MCP Server Statistics" width="720">
</p>

## Install

1. Download the matching setup from [Latest Release](https://github.com/Jotaro2025/everything-mcp/releases/latest) (`everything-mcp-*-x64-setup.exe` or `*-x86-setup.exe`).
2. Run it → click **Add** in the Everything plugin dialog (optionally restart Everything).
3. Open **Options → Plugins → MCP Server**, enable the service, click **Apply**.

<details>
<summary><strong>Manual DLL / build from source</strong></summary>

```powershell
cargo build --release
# Copy target\release\everything_mcp.dll as everything_mcp64.dll into
# C:\Program Files\Everything\Plugins\

cd installer
powershell -ExecutionPolicy Bypass -File build-installers.ps1
```

</details>

## Configuration

| Key | Type | Default | Meaning |
|---|---|---|---|
| `mcp_enabled` | int | `0` | `0` off / `1` on |
| `mcp_bind` | string | `127.0.0.1` | Bind address |
| `mcp_port` | int | `8285` | HTTP port |
| `mcp_global_search` | int | `0` | `0` deny / `1` review / `2` allow |

Stored under `%APPDATA%\Everything\Plugins.ini` in `[everything_mcp64.dll]` (or `everything_mcp32.dll`).

### MCP client snippet

```json
{
  "mcpServers": {
    "everything": {
      "url": "http://127.0.0.1:8285/"
    }
  }
}
```

## MCP tools

| Tool | Purpose |
|---|---|
| `search_in_folder` | Scoped Everything search (sort / page / timestamps) |
| `list_folder` | List folder entries |
| `count` | Lightweight match count |
| `index_changes` | Index journal (create / modify / delete / rename / move) |
| `read_file` | Read file text (line window / size cap; sensitive-path denylist) |
| `grep` | Candidate search + regex line match |
| `search_everywhere` | Global by name (gated by `mcp_global_search`, off by default) |
| `server_diagnostics` | Diagnostics and runtime stats |

Full parameters and troubleshooting: [docs/mcp-tools.en.md](docs/mcp-tools.en.md).

## Community

Discuss on [LINUX DO](https://linux.do/):

<p align="center">
  <a href="https://linux.do/"><img src="docs/images/linux-do-banner.png" alt="LINUX DO" width="420"></a>
</p>

## License

- Project code: [MIT](LICENSE)
- Everything is a voidtools product; this project is not affiliated with or endorsed by voidtools.
