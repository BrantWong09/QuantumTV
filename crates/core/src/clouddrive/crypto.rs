//! 凭证加密 (方案 §11/§12): AES-256-GCM, 密钥由本机随机生成并落盘。
//!
//! 禁止明文 cookie 进 SQLite (§12); blob 形态 = nonce(12B) || ciphertext || tag(16B)。
//! 实现用 ring (已在 rustls 依赖闭包内, 经审计实现, 不手搓密码学)。

use std::path::{Path, PathBuf};

use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use ring::rand::{SecureRandom, SystemRandom};

use super::types::CloudDriveError;

const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;
const KEY_FILE: &str = "cloud_cred.key";

#[derive(Clone)]
pub struct CredentialCrypto {
    key: Vec<u8>,
}

/// §39: 严禁密钥进日志
impl std::fmt::Debug for CredentialCrypto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialCrypto")
            .field("key", &format_args!("present({}B)", self.key.len()))
            .finish()
    }
}

impl CredentialCrypto {
    /// 加载本机密钥; 不存在则生成 32B 随机密钥写入 `<dir>/cloud_cred.key`。
    pub fn load_or_init(dir: &Path) -> Result<Self, CloudDriveError> {
        std::fs::create_dir_all(dir).map_err(|e| {
            CloudDriveError::StorageError(format!("创建凭证密钥目录失败: {e}"))
        })?;
        let path = key_path(dir);
        if path.is_file() {
            let bytes = std::fs::read(&path)
                .map_err(|e| CloudDriveError::StorageError(format!("读取密钥失败: {e}")))?;
            if bytes.len() == KEY_LEN {
                return Ok(Self { key: bytes });
            }
            // 密钥损坏 (长度不对): 重新生成; 旧凭证将无法解密, 由调用方按 Invalid 处理
            log::warn!("[CloudAuth] 凭证密钥长度异常 ({}B), 重新生成", bytes.len());
        }
        let key = generate_key()?;
        write_key_file(&path, &key)?;
        log::info!("[CloudAuth] 已生成本机凭证密钥: {}", path.display());
        Ok(Self { key })
    }

    pub fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>, CloudDriveError> {
        let mut in_out = plaintext.to_vec();
        let nonce_bytes = self.seal(&mut in_out)?;
        let mut out = Vec::with_capacity(NONCE_LEN + in_out.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&in_out);
        Ok(out)
    }

    pub fn decrypt(&self, blob: &[u8]) -> Result<Vec<u8>, CloudDriveError> {
        if blob.len() < NONCE_LEN + TAG_LEN {
            return Err(CloudDriveError::StorageError("密文长度异常".into()));
        }
        let (nonce_bytes, ct) = blob.split_at(NONCE_LEN);
        let key = self.aead_key()?;
        let mut in_out = ct.to_vec();
        let opened = key
            .open_in_place(
                nonce_bytes
                    .try_into()
                    .map(|arr: [u8; NONCE_LEN]| Nonce::assume_unique_for_key(arr))
                    .map_err(|_| CloudDriveError::StorageError("nonce 非法".into()))?,
                Aad::empty(),
                &mut in_out,
            )
            .map_err(|_| CloudDriveError::StorageError("解密失败 (密钥不匹配或数据损坏)".into()))?;
        Ok(opened.to_vec())
    }

    fn seal(&self, in_out: &mut Vec<u8>) -> Result<[u8; NONCE_LEN], CloudDriveError> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        SystemRandom::new()
            .fill(&mut nonce_bytes)
            .map_err(|_| CloudDriveError::StorageError("随机数生成失败".into()))?;
        let key = self.aead_key()?;
        key.seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce_bytes),
            Aad::empty(),
            in_out,
        )
        .map_err(|_| CloudDriveError::StorageError("加密失败".into()))?;
        Ok(nonce_bytes)
    }

    fn aead_key(&self) -> Result<LessSafeKey, CloudDriveError> {
        let unbound = UnboundKey::new(&AES_256_GCM, &self.key)
            .map_err(|_| CloudDriveError::StorageError("密钥非法".into()))?;
        Ok(LessSafeKey::new(unbound))
    }
}

fn generate_key() -> Result<Vec<u8>, CloudDriveError> {
    let mut key = vec![0u8; KEY_LEN];
    SystemRandom::new()
        .fill(&mut key)
        .map_err(|_| CloudDriveError::StorageError("随机数生成失败".into()))?;
    Ok(key)
}

fn key_path(dir: &Path) -> PathBuf {
    dir.join(KEY_FILE)
}

fn write_key_file(path: &Path, key: &[u8]) -> Result<(), CloudDriveError> {
    std::fs::write(path, key)
        .map_err(|e| CloudDriveError::StorageError(format!("写入密钥失败: {e}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "qtv_cred_test_{tag}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn roundtrip_and_tamper_detection() {
        let crypto = CredentialCrypto::load_or_init(&temp_dir("roundtrip")).unwrap();
        let plain = b"__puus=secret; _UP_28A_52_=381; BDUSS=xxx";
        let blob = crypto.encrypt(plain).unwrap();
        // blob = nonce || ct || tag, 不含明文
        assert!(blob.len() == 12 + plain.len() + 16);
        assert!(!window_contains(&blob, plain));
        assert_eq!(crypto.decrypt(&blob).unwrap(), plain.to_vec());

        // 篡改任一密文字节 → 解密失败
        let mut tampered = blob.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0xFF;
        assert!(crypto.decrypt(&tampered).is_err());
    }

    #[test]
    fn key_persists_and_loads() {
        let dir = temp_dir("persist");
        let c1 = CredentialCrypto::load_or_init(&dir).unwrap();
        let blob = c1.encrypt(b"cookie-data").unwrap();
        let c2 = CredentialCrypto::load_or_init(&dir).unwrap();
        assert_eq!(c2.decrypt(&blob).unwrap(), b"cookie-data".to_vec());
        // 密钥文件恰为 32B
        assert_eq!(
            std::fs::metadata(key_path(&dir)).unwrap().len(),
            KEY_LEN as u64
        );
    }

    #[test]
    fn wrong_key_cannot_decrypt() {
        let d1 = temp_dir("key1");
        let d2 = temp_dir("key2");
        let c1 = CredentialCrypto::load_or_init(&d1).unwrap();
        let c2 = CredentialCrypto::load_or_init(&d2).unwrap();
        let blob = c1.encrypt(b"x").unwrap();
        assert!(c2.decrypt(&blob).is_err());
    }

    fn window_contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|w| w == needle)
    }
}
