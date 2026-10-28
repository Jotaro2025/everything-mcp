//! sensitive.rs
//!
//! 敏感路径黑名单：私钥、凭据文件、密钥库与 `.git/objects` 一律不读。
//!
//! 面向 LLM 的读文件工具里，这是唯一能挡住「提示注入 → 读走 ~/.ssh/id_rsa」
//! 的机制，因此**不提供开关**。名单形状参考了主流 agent 客户端（如
//! PI-Desktop）的同类黑名单：私钥、`.env`、凭据文件三类，外加 git 对象库。
//!
//! 划线原则是「文件本身的存在目的就是放密钥」，而不是「可能含 token 的配置
//! 文件」—— 后者没有边界（`.npmrc`、`.pypirc`、`~/.aws/credentials`、
//! `.docker/config.json` 都是），收进来只会误伤正常读取。
//!
//! 只作用于**读正文**（`read_file` 与 `grep`，经 `read::read_text_for_matching`）：
//! 搜索结果不隐藏这些文件 —— 文件名不是秘密，正文才是；把索引结果悄悄过滤掉
//! 反而会让用户「搜了却没有」，破坏本工具作为索引镜像的可信度。

/// 精确文件名（比较前统一转小写）。
const DENIED_FILE_NAMES: &[&str] = &[
    // 凭据文件
    ".env",
    ".netrc",
    ".git-credentials",
    "credentials.json",
    // SSH 私钥（对应的 .pub 是公钥，不在名单里）
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    // 常见私钥副本命名 —— 它们的存在目的与原文件相同。
    "id_rsa.old",
    "id_dsa.old",
    "id_ecdsa.old",
    "id_ed25519.old",
];

/// 后缀黑名单（密钥库 / 证书私钥）。
const DENIED_SUFFIXES: &[&str] = &[".pem", ".key", ".p12", ".pfx", ".jks", ".keystore", ".ppk"];

/// `.env.*` 里属于「说明配置而非保存配置」的变体，放行。
const ENV_TEMPLATE_SUFFIXES: &[&str] = &[".example", ".sample", ".template", ".dist"];

/// Win32 路径的每个组件末尾的点号和空格会在 `CreateFileW` 层被剥掉
///（`id_rsa.` 打开的就是 `id_rsa`），而 8.3 短名（`CREDEN~1`）解析回的
/// 也是同一个文件 —— 黑名单如果只按调用方给的字符串比对，这两类拼写
/// 都能把私钥读出去。匹配前先按组件做同样的剥离，让「看到的路径」与
/// 「实际打开的路径」对齐。
///
/// 只做纯字符串变换（不触盘），规则与 Win32 文件系统命名一致：
/// 每个组件剥掉末尾的 `.` 和空格；`\\?\` 前缀关闭这种剥离，原样跳过。
/// 短名（含 `~`）组件交给 [`is_sensitive_file_name`] 的 `~` 检查。
pub fn normalize_win32_path(path: &str) -> String {
    let unified = path.replace('/', "\\");
    // 扩展长度前缀（\\?\C:\...）下 Win32 不做尾部剥离，保持原样比较。
    if unified.starts_with(r"\\?\") {
        return unified;
    }
    let unc = unified.starts_with(r"\\");
    let mut out = String::with_capacity(unified.len());
    if unc {
        out.push_str(r"\\");
    }
    let parts: Vec<&str> = unified
        .split('\\')
        .filter(|p| !p.is_empty())
        .map(|p| p.trim_end_matches(['.', ' ']))
        .collect();
    out.push_str(&parts.join("\\"));
    out
}

/// 文件名是否命中黑名单。
///
/// 除字面比对外还拦两类拼写：
///   - `~` 后缀的 8.3 短名 / 编辑器备份（`CREDEN~1`、`id_rsa~`、
///     `id_rsa.bak` 不含 `~`，走后缀表）—— 8.3 名解析回的就是长名文件；
///   - 黑名单条目加 `.bak/.old/~` 后缀的副本（`id_rsa.bak`、
///     `credentials.json.bak`）。
pub fn is_sensitive_file_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower.starts_with(".env.") {
        // `.env.local` 拒绝，`.env.example` 放行 —— 拿完整文件名比后缀。
        return !ENV_TEMPLATE_SUFFIXES
            .iter()
            .any(|suffix| lower.ends_with(suffix));
    }
    if DENIED_FILE_NAMES.contains(&lower.as_str()) {
        return true;
    }
    // `len >= suffix.len()`：连文件名就叫 `.key` 的也拦（PI-Desktop 那条用
    // `>` 会漏掉这种）。
    if DENIED_SUFFIXES
        .iter()
        .any(|suffix| lower.len() >= suffix.len() && lower.ends_with(suffix))
    {
        return true;
    }
    // 副本后缀：黑名单条目 + .bak/.old 仍是黑名单条目。基础名无论来自
    // 精确表（id_rsa.bak）还是后缀表（server.pem.bak）都要拦。
    for suffix in [".bak", ".old"] {
        if let Some(base) = lower.strip_suffix(suffix) {
            if DENIED_FILE_NAMES.contains(&base)
                || DENIED_SUFFIXES
                    .iter()
                    .any(|s| base.len() >= s.len() && base.ends_with(s))
            {
                return true;
            }
        }
    }
    // 8.3 短名 / 波浪号备份：`CREDEN~1`、`CREDEN~1.JSO`、`ID_ED2~1`、
    // `id_rsa~`。短名的 stem 以 `~序号` 收尾、扩展名被截成 3 字母
    // （.json → .JSO），精确比对必然落空 —— stem 按「黑名单条目是其前缀」
    // 比对，扩展名单独过一遍后缀表。真名带 `~` 的普通文件极少，
    // 宁误伤不泄密。
    let (stem, ext) = match lower.rsplit_once('.') {
        Some((s, e)) if s.contains('~') => (s, e),
        _ => (lower.as_str(), ""),
    };
    if let Some((base, tail)) = stem.rsplit_once('~') {
        if (tail.is_empty() || tail.chars().all(|c| c.is_ascii_digit())) && !base.is_empty() {
            if DENIED_FILE_NAMES.iter().any(|d| d.starts_with(base)) {
                return true;
            }
            if !ext.is_empty()
                && DENIED_SUFFIXES
                    .iter()
                    .any(|s| s.trim_start_matches('.') == ext)
            {
                return true;
            }
        }
    }
    false
}

/// 路径是否命中黑名单：文件名命中，或路径里存在 `.git\objects` 这一段。
///
/// 比对前先做 [`normalize_win32_path`] —— 尾随点/空格与短名拼写解析到的是
/// 同一个文件，必须与 Win32 的打开语义对齐，否则 `id_rsa.` / `CREDEN~1`
/// 就是绕过口。
pub fn is_sensitive_path(path: &str) -> bool {
    let normalized = normalize_win32_path(path);
    let name = normalized
        .rsplit('\\')
        .next()
        .unwrap_or(normalized.as_str());
    if is_sensitive_file_name(name) {
        return true;
    }
    // 逐段扫描，找相邻的 `.git` + `objects`。段的剥离已在 normalize 里做过；
    // `.git` 的 8.3 短名（`GIT~1` / `.GIT~1`）也一并识别 —— 短名解析回的就是
    // 同一个目录。
    let parts: Vec<&str> = normalized.split('\\').collect();
    parts
        .windows(2)
        .any(|w| {
            let git = w[0].eq_ignore_ascii_case(".git")
                || w[0].eq_ignore_ascii_case("git~1")
                || w[0].eq_ignore_ascii_case(".git~1");
            git && w[1].eq_ignore_ascii_case("objects")
        })
}

/// 命中黑名单时的统一错误文案。
pub fn denied_message(path: &str) -> String {
    format!(
        "{:?} is blocked by the sensitive-path denylist: private keys, .env files and \
         credential bundles are never returned by this tool (the list is not configurable)",
        path
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denies_keys_env_and_credential_files() {
        for name in [
            ".env",
            ".env.local",
            ".ENV.production",
            ".netrc",
            ".git-credentials",
            "credentials.json",
            "id_rsa",
            "id_dsa",
            "id_ecdsa",
            "id_ed25519",
            "server.pem",
            "private.key",
            "bundle.p12",
            "cert.pfx",
            "app.jks",
            "app.keystore",
            "putty.ppk",
            ".key",
        ] {
            assert!(is_sensitive_file_name(name), "{name} 应被拒绝");
        }
    }

    #[test]
    fn keeps_public_and_innocent_names_readable() {
        for name in [
            ".env.example",
            ".env.sample",
            ".env.template",
            ".env.dist",
            "environment.ts",
            "keys.rs",
            "id_rsa.pub",
            "package.json",
            "README.md",
            "monkey.txt",
            "keystore.md",
        ] {
            assert!(!is_sensitive_file_name(name), "{name} 应可读");
        }
    }

    #[test]
    fn denies_git_objects_tree_only() {
        assert!(is_sensitive_path(r"D:\repo\.git\objects\ab\cdef1234"));
        assert!(is_sensitive_path("D:/repo/.git/objects/pack/x.pack"));
        assert!(is_sensitive_path(r"D:\repo\.GIT\OBJECTS\ab\cd"));
        // `.git` 下别的文件、以及只是叫 objects 的普通目录都放行
        assert!(!is_sensitive_path(r"D:\repo\.git\HEAD"));
        assert!(!is_sensitive_path(r"D:\repo\src\objects\index.ts"));
    }

    #[test]
    fn denies_sensitive_names_at_any_depth() {
        assert!(is_sensitive_path(r"C:\Users\me\.ssh\id_rsa"));
        assert!(is_sensitive_path(r"D:\proj\.env"));
        assert!(is_sensitive_path(r"D:\proj\config\server.pem"));
        assert!(!is_sensitive_path(r"D:\proj\src\main.rs"));
        // 目录名里带敏感词不该误伤
        assert!(!is_sensitive_path(r"D:\env-backup\notes.txt"));
    }

    #[test]
    fn denied_message_names_the_policy() {
        let msg = denied_message(r"C:\x\.env");
        assert!(msg.contains("denylist"), "{msg}");
        assert!(msg.contains("not configurable"), "{msg}");
    }

    // ---- Win32 路径拼写审计修复：尾随点/空格与 8.3 短名 ----

    #[test]
    fn denies_trailing_dot_and_space_variants() {
        // Win32 打开 "id_rsa." / "id_rsa ." 就是 id_rsa 本体。
        for name in [
            "id_rsa.",
            "id_rsa .",
            "id_rsa.  ",
            "credentials.json.",
            "credentials.json .",
            ".env.",
            ".netrc.",
            "server.pem.",
            "server.pem .",
        ] {
            assert!(
                is_sensitive_file_name(name) || is_sensitive_path(&format!(r"C:\x\{name}")),
                "{name:?} 应被拒绝"
            );
        }
        // 整路径级：尾随点组件剥离后命中。
        assert!(is_sensitive_path(r"C:\Users\me\.ssh\id_rsa."));
        assert!(is_sensitive_path(r"D:\repo\.git .\objects\ab\cd"));
        assert!(is_sensitive_path(r"D:\proj\.env ."));
    }

    #[test]
    fn denies_short_8_3_names() {
        for name in ["CREDEN~1.JSO", "CREDEN~1", "ID_ED2~1", "ID_RSA~1", "id_rsa~"] {
            assert!(is_sensitive_file_name(name), "{name:?} 应被拒绝");
        }
        assert!(is_sensitive_path(r"C:\Users\me\.ssh\ID_ED2~1"));
        assert!(is_sensitive_path(r"D:\repo\GIT~1\objects\ab\cd"));
    }

    #[test]
    fn denies_backup_copies_of_denied_names() {
        for name in [
            "id_rsa.bak",
            "id_rsa.old",
            "id_ed25519.bak",
            "credentials.json.bak",
            ".netrc.bak",
            "server.pem.bak",
            "server.key.old",
        ] {
            assert!(is_sensitive_file_name(name), "{name:?} 应被拒绝");
        }
    }

    #[test]
    fn normalize_strips_trailing_dots_per_component_only() {
        assert_eq!(
            normalize_win32_path(r"C:\Users\me\.ssh\id_rsa."),
            r"C:\Users\me\.ssh\id_rsa"
        );
        assert_eq!(normalize_win32_path("D:/a/b./"), r"D:\a\b");
        // 中间组件的点不受影响（".git"、"main.rs" 保持原样）。
        assert_eq!(
            normalize_win32_path(r"D:\repo\.git\objects"),
            r"D:\repo\.git\objects"
        );
        // 扩展长度前缀不做剥离。
        assert_eq!(
            normalize_win32_path(r"\\?\C:\x\id_rsa."),
            r"\\?\C:\x\id_rsa."
        );
        // UNC 首缀与重复分隔符。
        assert_eq!(
            normalize_win32_path(r"\\server\share\proj\"),
            r"\\server\share\proj"
        );
        assert_eq!(normalize_win32_path(r"D:\\a\\\b"), r"D:\a\b");
    }

    #[test]
    fn keeps_innocent_short_names_readable() {
        // 无点无 ~ 的普通短名（8.3 化后也可能存在的普通文件）不误伤。
        for name in ["main.rs", "notes.txt", "monkey.txt", "keystore.md", "id_rsa.pub"] {
            assert!(!is_sensitive_file_name(name), "{name:?} 不该被拦");
        }
    }
}
