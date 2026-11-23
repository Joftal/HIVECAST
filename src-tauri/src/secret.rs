//! 静态凭据加密：AES-256-GCM + 本地密钥文件。
//!
//! 为什么不是 Keychain / DPAPI：v1 的分发形态是 ad-hoc 签名（没有 Developer ID），
//! macOS 会把「谁在访问钥匙串」绑定到二进制签名上，每次重新打包都会重新弹一次授权框，
//! 开发期每次 rebuild 都弹，体验比明文更差；Windows 侧 DPAPI 只能加密不能跨机迁移。
//! 折中方案：密钥放在数据目录下的 0600 文件里，Cookie 用 AES-256-GCM 加密后落库。
//!
//! 威胁模型（写清楚，别给人虚假安全感）：
//! - 防：把 hivecast.db 拷走 / 云同步 / 备份时泄露 Cookie；
//! - 不防：同一用户下的恶意进程（它能读密钥文件，也就能解密）。
//! 真要做到后者，需要系统钥匙串 + 签名，属于 v1.1 的事。

use std::path::Path;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;

use crate::error::AppError;

/// 密文前缀：用来区分「新写入的密文」和「历史明文」。
pub const PREFIX: &str = "enc:v1:";

pub struct Secret {
    cipher: Aes256Gcm,
}

impl Secret {
    /// 读密钥文件；不存在就生成一个（0600）。文件被截断/换行时另存为 .bak 再重建。
    pub fn load_or_create(path: &Path) -> Result<Self, AppError> {
        let key = match std::fs::read(path) {
            Ok(bytes) if bytes.len() == 32 => bytes,
            Ok(_) => {
                let backup = path.with_extension("bin.bak");
                let _ = std::fs::rename(path, &backup);
                tracing::warn!(path = %backup.display(), "凭据密钥长度不对，已备份并重新生成");
                fresh_key(path)?
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => fresh_key(path)?,
            Err(err) => return Err(io_err(path, err)),
        };
        let cipher = Aes256Gcm::new_from_slice(&key)
            .map_err(|_| AppError::User("凭据密钥初始化失败".into()))?;
        Ok(Self { cipher })
    }

    /// 加密：返回 enc:v1:<base64(nonce || 密文)>。
    pub fn seal(&self, plain: &str) -> Result<String, AppError> {
        let nonce_bytes: [u8; 12] = rand::random();
        let nonce = Nonce::from(nonce_bytes);
        let bytes = self
            .cipher
            .encrypt(&nonce, plain.as_bytes())
            .map_err(|_| AppError::User("凭据加密失败".into()))?;
        let mut packed = nonce.to_vec();
        packed.extend_from_slice(&bytes);
        Ok(format!(
            "{PREFIX}{}",
            base64::engine::general_purpose::STANDARD.encode(packed)
        ))
    }

    /// 解密。没有前缀的一律当历史明文原样返回（迁移期兼容）。
    pub fn open(&self, stored: &str) -> Option<String> {
        let Some(body) = stored.strip_prefix(PREFIX) else {
            return Some(stored.to_string());
        };
        let packed = base64::engine::general_purpose::STANDARD
            .decode(body)
            .ok()?;
        if packed.len() <= 12 {
            return None;
        }
        let (nonce, cipher) = packed.split_at(12);
        let nonce = Nonce::<aes_gcm::aead::consts::U12>::try_from(nonce).ok()?;
        let plain = self.cipher.decrypt(&nonce, cipher).ok()?;
        String::from_utf8(plain).ok()
    }

    pub fn is_sealed(stored: &str) -> bool {
        stored.starts_with(PREFIX)
    }
}

fn io_err(path: &Path, err: std::io::Error) -> AppError {
    AppError::Path(format!("{}: {err}", path.display()))
}

fn fresh_key(path: &Path) -> Result<Vec<u8>, AppError> {
    let key: [u8; 32] = rand::random();
    let bytes = key.to_vec();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| io_err(parent, err))?;
    }
    std::fs::write(path, &bytes).map_err(|err| io_err(path, err))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个用例一个独立密钥文件：测试是并行跑的，共用一个文件会互相删。
    fn temp_secret(tag: &str) -> (Secret, std::path::PathBuf) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!("hivecast-secret-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!(
            "secret-{tag}-{}.bin",
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_file(&path);
        let secret = Secret::load_or_create(&path).expect("密钥生成失败");
        (secret, path)
    }

    #[test]
    fn roundtrips_and_hides_plaintext() {
        let (secret, _path) = temp_secret("roundtrip");
        let sealed = secret.seal("SESSDATA=abc; bili_jct=def").expect("加密失败");
        assert!(sealed.starts_with(PREFIX));
        assert!(!sealed.contains("SESSDATA"));
        assert_eq!(
            secret.open(&sealed).as_deref(),
            Some("SESSDATA=abc; bili_jct=def")
        );
    }

    #[test]
    fn passes_legacy_plaintext_through() {
        let (secret, _path) = temp_secret("legacy");
        assert_eq!(secret.open("a=1").as_deref(), Some("a=1"));
        assert!(!Secret::is_sealed("a=1"));
    }

    #[test]
    fn rejects_tampered_ciphertext() {
        let (secret, _path) = temp_secret("tamper");
        let sealed = secret.seal("a=1").expect("加密失败");
        let mut bytes = sealed.into_bytes();
        let last = bytes.len() - 2;
        bytes[last] = if bytes[last] == b'A' { b'B' } else { b'A' };
        let broken = String::from_utf8(bytes).expect("非法 utf8");
        assert!(secret.open(&broken).is_none());
    }

    #[test]
    fn key_file_is_private_on_unix() {
        let (_secret, path) = temp_secret("perm");
        assert_eq!(std::fs::read(&path).expect("读不到密钥").len(), 32);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path)
                .expect("无元数据")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn same_key_survives_reload() {
        let (secret, path) = temp_secret("reload");
        let sealed = secret.seal("cookie").expect("加密失败");
        let again = Secret::load_or_create(&path).expect("重新加载失败");
        assert_eq!(again.open(&sealed).as_deref(), Some("cookie"));
    }
}
