//! ini_settings.rs — 插件设置的兜底读取（PM_START 时用）
//!
//! 背景：主程序在 PM_START 传入的设置上下文（sorted_list）实测读不到插件
//! 自己的设置项 —— 即使 PM_SAVE_SETTINGS 明明把 `mcp_enabled=1` 写进了
//! `%APPDATA%\Everything\Plugins.ini` 的 `[everything_mcp64.dll]` 段，
//! 下次启动 PM_START 仍然只拿到默认值（enabled=false、port=8285、
//! bind=127.0.0.1）。而选项对话框的 Apply 是即时生效的，于是出现
//! 「启用后重启 Everything 又变回未启用」。
//!
//! 主程序的写入路径（PM_SAVE_SETTINGS → Plugins.ini）是可靠的，所以这里
//! 自己按同样格式解析 Plugins.ini 作为回退：host 读不到未启用时用它。
//! 只读不写，绝不与主程序抢这个文件。

use std::path::PathBuf;

/// 部署后的 DLL 文件名 —— 主程序按它给 Plugins.ini 里的段命名。
const SECTION: &str = "everything_mcp64.dll";

/// 从 Plugins.ini 解析出来的设置。段存在但没有 `mcp_enabled` 键时为 None。
pub struct PersistedSettings {
    pub enabled: bool,
    pub port: Option<u16>,
    pub bind: Option<String>,
}

/// 读持久化设置：先 `%APPDATA%\Everything\Plugins.ini`（app_data=1 时主程序
/// 就用这个位置），再退到 Everything.exe 旁边的 Plugins.ini（app_data=0 的
/// 便携安装）。都读不到或没有我们的段时返回 None。
pub fn read_persisted() -> Option<PersistedSettings> {
    for path in candidate_paths() {
        if let Ok(bytes) = std::fs::read(&path) {
            let content = String::from_utf8_lossy(&bytes);
            if let Some(s) = parse_section(&content) {
                super::diag::write(&format!(
                    "ini_settings: loaded from {} (enabled={})",
                    path.display(),
                    s.enabled
                ));
                return Some(s);
            }
        }
    }
    None
}

fn candidate_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(appdata) = std::env::var("APPDATA") {
        paths.push(
            PathBuf::from(appdata)
                .join("Everything")
                .join("Plugins.ini"),
        );
    }
    // current_exe() 是宿主进程的 Everything.exe；便携安装时 Plugins.ini 在它旁边。
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            paths.push(dir.join("Plugins.ini"));
        }
    }
    paths
}

/// 解析 `[everything_mcp64.dll]` 段里的 `mcp_*` 键。段名大小写不敏感、
/// 允许空白；其他段的同名键一律忽略。
fn parse_section(content: &str) -> Option<PersistedSettings> {
    let mut in_section = false;
    let mut enabled: Option<bool> = None;
    let mut port: Option<u16> = None;
    let mut bind: Option<String> = None;

    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            // 行首是 '['（ASCII），切到索引 1 一定是字符边界。
            in_section = line[1..]
                .trim_end_matches(']')
                .trim()
                .eq_ignore_ascii_case(SECTION);
            continue;
        }
        if !in_section {
            continue;
        }
        let (key, value) = match line.split_once('=') {
            Some(kv) => kv,
            None => continue,
        };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "mcp_enabled" => enabled = Some(value.parse::<i64>().map(|v| v != 0).unwrap_or(false)),
            "mcp_port" => port = value.parse::<u16>().ok().filter(|p| *p != 0),
            "mcp_bind" => {
                if !value.is_empty() {
                    bind = Some(value.to_string());
                }
            }
            _ => {}
        }
    }

    let enabled = enabled?;
    Some(PersistedSettings {
        enabled,
        port,
        bind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_our_section_with_all_keys() {
        let content = "; comment\r\n[etp_server64.dll]\r\nenabled=0\r\n\r\n[everything_mcp64.dll]\r\nmcp_enabled=1\r\nmcp_port=8285\r\nmcp_bind=127.0.0.1\r\n\r\n[http_server64.dll]\r\nport=80\r\n";
        let s = parse_section(content).expect("section present");
        assert!(s.enabled);
        assert_eq!(s.port, Some(8285));
        assert_eq!(s.bind.as_deref(), Some("127.0.0.1"));
    }

    #[test]
    fn missing_section_returns_none() {
        assert!(parse_section("[etp_server64.dll]\nenabled=1\n").is_none());
    }

    #[test]
    fn disabled_value_is_preserved() {
        let content = "[everything_mcp64.dll]\nmcp_enabled=0\n";
        let s = parse_section(content).expect("section present");
        assert!(!s.enabled);
        assert_eq!(s.port, None);
    }

    #[test]
    fn keys_outside_our_section_are_ignored() {
        // mcp_enabled 出现在别的段里，不应被误读。
        let content = "[other]\nmcp_enabled=1\n[everything_mcp64.dll]\nmcp_port=9000\n";
        assert!(parse_section(content).is_none());
    }

    #[test]
    fn invalid_port_and_empty_bind_are_dropped() {
        let content = "[everything_mcp64.dll]\nmcp_enabled=1\nmcp_port=99999\nmcp_bind=\n";
        let s = parse_section(content).unwrap();
        assert_eq!(s.port, None);
        assert_eq!(s.bind, None);
    }

    #[test]
    fn section_match_is_case_insensitive_and_trims() {
        let content = "[ Everything_MCP64.DLL ]\nmcp_enabled=1\n";
        assert!(parse_section(content).is_some());
    }
}
