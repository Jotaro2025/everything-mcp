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
//! 只作用于**读正文**（`read_file`，将来的 grep 同理）：搜索结果不隐藏这些
//! 文件 —— 文件名不是秘密，正文才是；把索引结果悄悄过滤掉反而会让用户
//! 「搜了却没有」，破坏本工具作为索引镜像的可信度。

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
];

/// 后缀黑名单（密钥库 / 证书私钥）。
const DENIED_SUFFIXES: &[&str] = &[".pem", ".key", ".p12", ".pfx", ".jks", ".keystore", ".ppk"];

/// `.env.*` 里属于「说明配置而非保存配置」的变体，放行。
const ENV_TEMPLATE_SUFFIXES: &[&str] = &[".example", ".sample", ".template", ".dist"];

/// 文件名是否命中黑名单。
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
    DENIED_SUFFIXES
        .iter()
        .any(|suffix| lower.len() >= suffix.len() && lower.ends_with(suffix))
}

/// 路径是否命中黑名单：文件名命中，或路径里存在 `.git\objects` 这一段。
///
/// git 对象是压缩二进制，读出来没意义，但里面的 blob 可能含任何被删过的
/// 敏感内容，所以整棵树拒绝。
pub fn is_sensitive_path(path: &str) -> bool {
    let normalized = path.replace('/', "\\");
    let name = normalized
        .rsplit('\\')
        .next()
        .unwrap_or(normalized.as_str());
    if is_sensitive_file_name(name) {
        return true;
    }
    // 逐段扫描，找相邻的 `.git` + `objects`。
    let parts: Vec<&str> = normalized.split('\\').collect();
    parts
        .windows(2)
        .any(|w| w[0].eq_ignore_ascii_case(".git") && w[1].eq_ignore_ascii_case("objects"))
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
}
