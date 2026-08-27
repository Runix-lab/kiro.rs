//! 含密文件的落盘权限。
//!
//! `credentials.json` 存的是上游 refresh token，`config.json` 存的是
//! `adminApiKey` / `apiKey`。默认 umask 会把它们落成 `0644` —— 同机任何用户可读。
//! 生产那台是**共享机**（同时跑着别的服务），644 等于把凭据摊开给同机所有进程。
//!
//! 全局准则原话：「密钥/凭证：绝不写文件/commit/日志，仅服务器 chmod 600 或 Keychain」。
//! 落文件这件事已经无法避免（进程重启要读回来），那至少把权限收到 0600。

use std::path::Path;

/// 把文件权限收到 `0600`（仅属主可读写）。
///
/// 非 Unix 平台是 no-op：Windows 的 ACL 模型不同，硬套 mode 位没有意义，
/// 而这个项目的生产环境是 Linux。
pub fn restrict_permissions(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn restricts_to_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let p = std::env::temp_dir().join(format!("kiro_secret_test_{}", std::process::id()));
        std::fs::write(&p, b"{}").unwrap();
        // 先故意放宽，确认函数真的收紧了而不是碰巧
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        restrict_permissions(&p).unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "含密文件必须是 0600，实得 {:o}", mode);
        let _ = std::fs::remove_file(&p);
    }

    /// 文件不存在时应报错而不是静默通过 —— 静默通过会让调用方
    /// 以为"权限已收紧"，而实际上什么都没发生。
    #[cfg(unix)]
    #[test]
    fn missing_file_is_an_error_not_a_silent_pass() {
        let p = std::env::temp_dir().join("kiro_secret_test_does_not_exist");
        let _ = std::fs::remove_file(&p);
        assert!(restrict_permissions(&p).is_err());
    }
}
