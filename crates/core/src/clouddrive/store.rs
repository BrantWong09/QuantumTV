//! 凭证持久化 (方案 §11): SQLite `cloud_credentials` 表, 凭证列只存 AES-GCM 密文。
//!
//! 复用应用主库 (src-tauri 传入共享连接), core 侧不自行决定库路径。
//! 状态列 (status/account/last_verified_at_ms) 与密文同行, 供状态查询免解密 (§39)。

use std::sync::{Arc, Mutex};

use rusqlite::Connection;

use super::types::{now_ms, AuthState, AuthStatus, CloudDriveError, CloudDriveType, ProviderCredential};

/// 密文 + 元数据 (解密前可读的部分)
#[derive(Debug, Clone)]
pub struct StoredCredential {
    pub data_enc: Vec<u8>,
    pub created_at_ms: i64,
    pub expires_at_ms: Option<i64>,
}

pub struct CredentialStore {
    conn: Arc<Mutex<Connection>>,
}

impl CredentialStore {
    pub fn from_shared(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }

    pub fn init_table(&self) -> Result<(), CloudDriveError> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS cloud_credentials (
              provider TEXT PRIMARY KEY,
              data_enc BLOB NOT NULL,
              status TEXT NOT NULL DEFAULT 'unknown',
              account TEXT,
              created_at_ms INTEGER NOT NULL,
              expires_at_ms INTEGER,
              last_verified_at_ms INTEGER,
              last_error TEXT,
              updated_at_ms INTEGER NOT NULL
            );
            "#,
        )
        .map_err(|e| CloudDriveError::StorageError(format!("建表失败: {e}")))?;
        Ok(())
    }

    /// 保存/覆盖凭证密文 (保留已有验证状态, 重新登录后由 verify 流程更新)
    pub fn upsert(
        &self,
        cred: &ProviderCredential,
        data_enc: &[u8],
    ) -> Result<(), CloudDriveError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            r#"
            INSERT INTO cloud_credentials
              (provider, data_enc, status, created_at_ms, expires_at_ms, updated_at_ms)
            VALUES (?1, ?2, 'qr_pending', ?3, ?4, ?5)
            ON CONFLICT(provider) DO UPDATE SET
              data_enc = excluded.data_enc,
              status = 'qr_pending',
              created_at_ms = excluded.created_at_ms,
              expires_at_ms = excluded.expires_at_ms,
              updated_at_ms = excluded.updated_at_ms
            "#,
            rusqlite::params![
                cred.provider.as_str(),
                data_enc,
                cred.created_at_ms,
                cred.expires_at_ms,
                now_ms(),
            ],
        )
        .map_err(|e| CloudDriveError::StorageError(format!("凭证写入失败: {e}")))?;
        Ok(())
    }

    pub fn load(&self, provider: CloudDriveType) -> Result<Option<StoredCredential>, CloudDriveError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT data_enc, created_at_ms, expires_at_ms
                 FROM cloud_credentials WHERE provider = ?1",
            )
            .map_err(|e| CloudDriveError::StorageError(format!("凭证读取失败: {e}")))?;
        let mut rows = stmt
            .query([provider.as_str()])
            .map_err(|e| CloudDriveError::StorageError(format!("凭证读取失败: {e}")))?;
        match rows.next() {
            Ok(Some(row)) => {
                let data_enc: Vec<u8> = row.get(0).map_err(|e| {
                    CloudDriveError::StorageError(format!("密文列读取失败: {e}"))
                })?;
                Ok(Some(StoredCredential {
                    data_enc,
                    created_at_ms: row.get(1).unwrap_or(0),
                    expires_at_ms: row.get(2).ok().flatten(),
                }))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(CloudDriveError::StorageError(format!("凭证读取失败: {e}"))),
        }
    }

    /// 记录验证结论 (§6): Authenticated 仅在 verify 成功时写入
    pub fn set_verify_result(
        &self,
        provider: CloudDriveType,
        status: AuthStatus,
        account: Option<&str>,
        error: Option<&str>,
    ) -> Result<(), CloudDriveError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            r#"
            UPDATE cloud_credentials
            SET status = ?2, account = ?3, last_error = ?4,
                last_verified_at_ms = ?5, updated_at_ms = ?5
            WHERE provider = ?1
            "#,
            rusqlite::params![
                provider.as_str(),
                status_name(status),
                account,
                error,
                now_ms(),
            ],
        )
        .map_err(|e| CloudDriveError::StorageError(format!("状态写入失败: {e}")))?;
        Ok(())
    }

    /// 仅置状态 (如 QrPending), 不触碰验证字段
    pub fn set_status(&self, provider: CloudDriveType, status: AuthStatus) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "UPDATE cloud_credentials SET status = ?2, updated_at_ms = ?3 WHERE provider = ?1",
            rusqlite::params![provider.as_str(), status_name(status), now_ms()],
        );
    }

    pub fn delete(&self, provider: CloudDriveType) -> Result<(), CloudDriveError> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM cloud_credentials WHERE provider = ?1", [provider.as_str()])
            .map_err(|e| CloudDriveError::StorageError(format!("凭证删除失败: {e}")))?;
        Ok(())
    }

    /// 全部网盘的状态快照 (免解密, §39)
    pub fn list_states(&self) -> Vec<AuthState> {
        let conn = self.conn.lock().unwrap();
        let Ok(mut stmt) = conn.prepare(
            "SELECT provider, status, account, created_at_ms, expires_at_ms,
                    last_verified_at_ms, last_error
             FROM cloud_credentials",
        ) else {
            return Vec::new();
        };
        let map_row = |row: &rusqlite::Row| -> rusqlite::Result<(AuthState, bool)> {
            let provider_str: String = row.get(0)?;
            let status_str: String = row.get(1)?;
            let created: i64 = row.get(3)?;
            let expires: Option<i64> = row.get(4)?;
            let verified: Option<i64> = row.get(5)?;
            let error: Option<String> = row.get(6)?;
            let (provider, known) = match CloudDriveType::parse(&provider_str) {
                Some(p) => (p, true),
                None => (CloudDriveType::Quark, false), // 未知网盘行不展示
            };
            Ok((
                AuthState {
                    provider,
                    status: parse_status(&status_str),
                    account: row.get(2)?,
                    created_at_ms: (created > 0).then_some(created),
                    expires_at_ms: expires,
                    last_verified_at_ms: verified,
                    last_error: error,
                },
                known,
            ))
        };
        let Ok(rows) = stmt.query_map([], map_row) else {
            return Vec::new();
        };
        rows.filter_map(|r| r.ok())
            .filter(|(_, known)| *known)
            .map(|(state, _)| state)
            .collect()
    }
}

fn status_name(s: AuthStatus) -> &'static str {
    match s {
        AuthStatus::Unknown => "unknown",
        AuthStatus::Unauthenticated => "unauthenticated",
        AuthStatus::QrPending => "qr_pending",
        AuthStatus::Authenticated => "authenticated",
        AuthStatus::Expired => "expired",
        AuthStatus::Invalid => "invalid",
        AuthStatus::Refreshing => "refreshing",
    }
}

fn parse_status(s: &str) -> AuthStatus {
    match s {
        "unauthenticated" => AuthStatus::Unauthenticated,
        "qr_pending" => AuthStatus::QrPending,
        "authenticated" => AuthStatus::Authenticated,
        "expired" => AuthStatus::Expired,
        "invalid" => AuthStatus::Invalid,
        "refreshing" => AuthStatus::Refreshing,
        _ => AuthStatus::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_store() -> CredentialStore {
        let conn = Connection::open_in_memory().unwrap();
        let store = CredentialStore::from_shared(Arc::new(Mutex::new(conn)));
        store.init_table().unwrap();
        store
    }

    #[test]
    fn upsert_load_delete_roundtrip() {
        let store = memory_store();
        let cred = ProviderCredential::from_cookie(CloudDriveType::Quark, "k=v");
        store.upsert(&cred, &[1, 2, 3, 4]).unwrap();

        let stored = store.load(CloudDriveType::Quark).unwrap().unwrap();
        assert_eq!(stored.data_enc, vec![1, 2, 3, 4]);
        assert!(stored.created_at_ms > 0);

        // 覆盖写
        store.upsert(&cred, &[9]).unwrap();
        assert_eq!(store.load(CloudDriveType::Quark).unwrap().unwrap().data_enc, vec![9]);

        store.delete(CloudDriveType::Quark).unwrap();
        assert!(store.load(CloudDriveType::Quark).unwrap().is_none());
        store.delete(CloudDriveType::Quark).unwrap(); // 幂等
    }

    #[test]
    fn verify_result_updates_status_only() {
        let store = memory_store();
        let cred = ProviderCredential::from_cookie(CloudDriveType::Baidu, "BDUSS=x");
        store.upsert(&cred, &[1]).unwrap();

        store.set_verify_result(CloudDriveType::Baidu, AuthStatus::Authenticated, Some("nick"), None)
            .unwrap();
        let states = store.list_states();
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].status, AuthStatus::Authenticated);
        assert_eq!(states[0].account.as_deref(), Some("nick"));
        assert!(states[0].last_verified_at_ms.is_some());

        store.set_verify_result(CloudDriveType::Baidu, AuthStatus::Invalid, None, Some("业务码 4"))
            .unwrap();
        let states = store.list_states();
        assert_eq!(states[0].status, AuthStatus::Invalid);
        assert_eq!(states[0].last_error.as_deref(), Some("业务码 4"));
    }

    #[test]
    fn list_states_skips_unknown_provider_rows() {
        let store = memory_store();
        let conn = store.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO cloud_credentials (provider, data_enc, created_at_ms, updated_at_ms)
             VALUES ('ali', x'00', 1, 1)",
            [],
        )
        .unwrap();
        drop(conn);
        assert!(store.list_states().is_empty());
    }

    #[test]
    fn missing_row_is_unauthenticated_view() {
        let store = memory_store();
        assert!(store.load(CloudDriveType::Uc).unwrap().is_none());
        // 状态查询对缺失行报 Unauthenticated 由 manager 组装, 这里只验证 list 不含
        assert!(store.list_states().is_empty());
    }
}
