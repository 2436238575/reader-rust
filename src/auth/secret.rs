use crate::util::crypto::random_string;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const SECRET_FILE: &str = "jwt_secret";

/// 解析 JWT 签名密钥。
///
/// 优先使用 `JWT_SECRET`；未配置时生成 48 位随机密钥并持久化到
/// `<STORAGE_DIR>/jwt_secret`，使进程重启后已签发的令牌继续有效。
///
/// 刻意不提供硬编码默认值：默认密钥一旦进入仓库，任何人都能伪造任意用户的令牌。
pub async fn resolve_jwt_secret(
    configured: Option<&str>,
    storage_dir: &str,
) -> anyhow::Result<Arc<Vec<u8>>> {
    if let Some(secret) = configured.map(str::trim).filter(|value| !value.is_empty()) {
        return Ok(Arc::new(secret.as_bytes().to_vec()));
    }

    let path = secret_path(storage_dir);
    if let Ok(existing) = tokio::fs::read_to_string(&path).await {
        let existing = existing.trim();
        if !existing.is_empty() {
            return Ok(Arc::new(existing.as_bytes().to_vec()));
        }
    }

    let generated = random_string(48);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&path, &generated).await?;
    restrict_permissions(&path);
    tracing::warn!(
        path = %path.display(),
        "JWT_SECRET 未配置，已生成随机签名密钥；如需多实例共享登录态请显式配置"
    );
    Ok(Arc::new(generated.into_bytes()))
}

fn secret_path(storage_dir: &str) -> PathBuf {
    Path::new(storage_dir).join(SECRET_FILE)
}

/// 密钥文件仅属主可读写；失败只记录不中断启动（例如只读挂载）。
#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        tracing::warn!(error = %e, "无法收紧 jwt_secret 文件权限");
    }
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_storage() -> PathBuf {
        std::env::temp_dir().join(format!("reader-rust-jwt-{}", random_string(8)))
    }

    #[tokio::test]
    async fn uses_configured_secret_verbatim_and_writes_no_file() {
        let dir = temp_storage();
        let secret = resolve_jwt_secret(Some("explicit-secret"), &dir.to_string_lossy())
            .await
            .unwrap();
        assert_eq!(secret.as_slice(), b"explicit-secret");
        assert!(!dir.join(SECRET_FILE).exists());
        // 纯空白视为未配置
        let generated = resolve_jwt_secret(Some("   "), &dir.to_string_lossy())
            .await
            .unwrap();
        assert_ne!(generated.as_slice(), b"   ");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn generates_and_persists_secret_across_calls() {
        let dir = temp_storage();
        let dir_str = dir.to_string_lossy().to_string();

        let first = resolve_jwt_secret(None, &dir_str).await.unwrap();
        assert!(dir.join(SECRET_FILE).exists());
        assert_eq!(first.len(), 48);

        // 第二次解析必须读到同一个密钥，否则重启会让所有令牌失效
        let second = resolve_jwt_secret(None, &dir_str).await.unwrap();
        assert_eq!(first.as_slice(), second.as_slice());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
