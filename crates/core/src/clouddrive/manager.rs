//! CloudDriveManager (方案 §2 顶层编排)
//!
//! 职责: 认证状态机 + 凭证加密持久化 + verify_login (§6) + 桥接 cookie 推送。
//! 边界 (§45/§49): 不碰播放代理 (gateway), 不碰搜索, 前端不再看到 cookie 本体。

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use super::crypto::CredentialCrypto;
use super::provider::{CloudDriveProvider, CloudPlayFetcher};
use super::store::CredentialStore;
use super::types::{
    AuthState, AuthStatus, CheckItem, CloudDriveError, CloudDriveType, ConnectionTest,
    LoginResult, ProviderCredential, QrLoginSession, RawLoginOutcome, VerifyOutcome,
};
use super::{baidu, quark, uc};

/// 凭据下发通道 (生产实现 = 桥接 APK /setCookie; 测试注入桩)
#[async_trait]
pub trait BridgeCookieSink: Send + Sync {
    async fn push_cookie(&self, drive: &str, cookie: &str) -> Result<(), String>;
}

pub struct CloudDriveManager {
    providers: HashMap<CloudDriveType, Arc<dyn CloudDriveProvider>>,
    store: Arc<CredentialStore>,
    crypto: Arc<CredentialCrypto>,
    sink: Arc<dyn BridgeCookieSink>,
}

impl CloudDriveManager {
    /// 标准装配: 百度/夸克/UC 三个独立 Provider (§4), 共享一个播放解析通道
    pub fn new(
        fetcher: Arc<dyn CloudPlayFetcher>,
        store: Arc<CredentialStore>,
        crypto: Arc<CredentialCrypto>,
        sink: Arc<dyn BridgeCookieSink>,
    ) -> Self {
        let mut providers: HashMap<CloudDriveType, Arc<dyn CloudDriveProvider>> = HashMap::new();
        providers.insert(
            CloudDriveType::Baidu,
            Arc::new(baidu::BaiduProvider::new(fetcher.clone())),
        );
        providers.insert(
            CloudDriveType::Quark,
            Arc::new(quark::QuarkProvider::new(fetcher.clone())),
        );
        providers.insert(
            CloudDriveType::Uc,
            Arc::new(uc::UcProvider::new(fetcher)),
        );
        Self {
            providers,
            store,
            crypto,
            sink,
        }
    }

    pub fn provider(&self, drive: CloudDriveType) -> Result<&Arc<dyn CloudDriveProvider>, String> {
        self.providers
            .get(&drive)
            .ok_or_else(|| format!("网盘 {} 无 Provider", drive.as_str()))
    }

    pub fn store(&self) -> &Arc<CredentialStore> {
        &self.store
    }

    // -- 扫码登录 (§29 状态机: QrPending → Confirmed → Verifying → Authenticated) --

    pub async fn start_login(&self, drive: CloudDriveType) -> Result<QrLoginSession, String> {
        let session = self
            .provider(drive)?
            .start_qr_login()
            .await
            .map_err(|e| e.user_message(drive))?;
        if self.store.load(drive).map_err(|e| e.to_string())?.is_some() {
            self.store.set_status(drive, AuthStatus::QrPending);
        }
        log::info!("[CloudAuth] provider={} event=qr_created", drive.as_str());
        Ok(session)
    }

    /// 轮询; Confirmed 走完整链: 加密保存 → verify (§6) → 桥接推送。
    /// 前端只拿验证结论, 不再拿 cookie (§10/§39)。
    pub async fn poll_login(&self, session: &QrLoginSession) -> Result<LoginResult, String> {
        let drive = session.drive;
        let outcome = self
            .provider(drive)?
            .poll_qr_login(session)
            .await
            .map_err(|e| e.user_message(drive))?;
        match outcome {
            RawLoginOutcome::Waiting => Ok(LoginResult::Waiting),
            RawLoginOutcome::Scanned => {
                log::info!("[CloudAuth] provider={} event=qr_scanned", drive.as_str());
                Ok(LoginResult::Scanned)
            }
            RawLoginOutcome::Expired => Ok(LoginResult::Expired),
            RawLoginOutcome::Confirmed(credential) => {
                log::info!(
                    "[CloudAuth] provider={} event=login_confirmed cookie_present={}",
                    drive.as_str(),
                    !credential.cookie().unwrap_or("").is_empty()
                );
                let verify = self.confirm_credential(&credential).await;
                Ok(LoginResult::Confirmed { verify })
            }
        }
    }

    /// 登录确认后的强制验证链 (§6/§11/§12): 保存(加密) → 账号 API 校验 → 推送桥接。
    /// 网络故障与凭证无效区分处理: 凭证无效不推送; 网络故障已推送但未验证。
    async fn confirm_credential(&self, credential: &ProviderCredential) -> VerifyOutcome {
        let drive = credential.provider;
        let enc = match self.credential_json(credential) {
            Ok(v) => v,
            Err(e) => return self.verify_fail(drive, format!("凭证序列化失败: {e}"), false),
        };
        if let Err(e) = self.store.upsert(credential, &enc) {
            return self.verify_fail(drive, format!("凭证保存失败: {}", e.user_message(drive)), false);
        }
        log::info!("[CloudAuth] provider={} event=credential_saved", drive.as_str());
        let provider = match self.provider(drive) {
            Ok(p) => p.clone(),
            Err(e) => return self.verify_fail(drive, e, false),
        };
        match provider.verify_credential(credential).await {
            Ok(account) => {
                log::info!("[CloudAuth] provider={} event=verify_success", drive.as_str());
                let _ = self
                    .store
                    .set_verify_result(drive, AuthStatus::Authenticated, Some(&account), None);
                let (pushed, push_msg) = self.push_to_bridge(drive, credential).await;
                let message = if pushed {
                    format!("验证通过: {account}")
                } else {
                    format!("验证通过, 但推送桥接失败: {push_msg}")
                };
                VerifyOutcome {
                    provider: drive,
                    ok: pushed,
                    account: Some(account),
                    message,
                    bridge_pushed: pushed,
                }
            }
            Err(CloudDriveError::CredentialInvalid(reason)) => {
                log::warn!(
                    "[CloudAuth] provider={} event=verify_failed reason=invalid",
                    drive.as_str()
                );
                let _ = self.store.set_verify_result(
                    drive,
                    AuthStatus::Invalid,
                    None,
                    Some(&reason),
                );
                // 凭证被判定无效: 不推送, 避免用坏 cookie 污染 APK 内既有登录态
                VerifyOutcome {
                    provider: drive,
                    ok: false,
                    account: None,
                    message: format!("扫码确认成功, 但账号验证未通过 ({reason}); 未写入桥接, 请重新扫码"),
                    bridge_pushed: false,
                }
            }
            Err(e) => {
                // 网络类失败: 凭证本身未必无效, 推送后允许"测试连接"补验 (§33 语义)
                log::warn!(
                    "[CloudAuth] provider={} event=verify_network_error err={}",
                    drive.as_str(),
                    crate::spider::trunc(&e.to_string(), 120)
                );
                let _ = self
                    .store
                    .set_verify_result(drive, AuthStatus::Unknown, None, Some(&e.to_string()));
                let (pushed, _) = self.push_to_bridge(drive, credential).await;
                VerifyOutcome {
                    provider: drive,
                    ok: false,
                    account: None,
                    message: format!(
                        "登录验证请求失败 ({}); {}",
                        crate::spider::trunc(&e.user_message(drive), 80),
                        if pushed { "cookie 已写入桥接, 可稍后用「测试连接」复查" } else { "cookie 未写入桥接" }
                    ),
                    bridge_pushed: pushed,
                }
            }
        }
    }

    fn verify_fail(&self, drive: CloudDriveType, message: String, pushed: bool) -> VerifyOutcome {
        let _ = self
            .store
            .set_verify_result(drive, AuthStatus::Invalid, None, Some(&message));
        VerifyOutcome {
            provider: drive,
            ok: false,
            account: None,
            message,
            bridge_pushed: pushed,
        }
    }

    async fn push_to_bridge(&self, drive: CloudDriveType, credential: &ProviderCredential) -> (bool, String) {
        let Some(cookie) = credential.cookie().filter(|c| !c.is_empty()) else {
            return (false, "凭证缺少 cookie".to_string());
        };
        match self.sink.push_cookie(drive.as_str(), cookie).await {
            Ok(()) => (true, String::new()),
            Err(e) => {
                log::warn!("[CloudAuth] provider={} bridge_push_failed err={}", drive.as_str(), crate::spider::trunc(&e, 120));
                (false, e)
            }
        }
    }

    fn credential_json(&self, credential: &ProviderCredential) -> Result<Vec<u8>, String> {
        let json = serde_json::to_vec(credential).map_err(|e| e.to_string())?;
        self.crypto.encrypt(&json).map_err(|e| e.to_string())
    }

    fn load_credential(&self, drive: CloudDriveType) -> Result<Option<ProviderCredential>, String> {
        let Some(stored) = self.store.load(drive).map_err(|e| e.to_string())? else {
            return Ok(None);
        };
        let plain = self.crypto.decrypt(&stored.data_enc).map_err(|e| e.to_string())?;
        let cred: ProviderCredential = serde_json::from_slice(&plain).map_err(|e| e.to_string())?;
        Ok(Some(cred))
    }

    // -- 状态查询 (§30) --

    pub fn states(&self) -> Vec<AuthState> {
        let stored: HashMap<CloudDriveType, AuthState> = self
            .store
            .list_states()
            .into_iter()
            .map(|s| (s.provider, s))
            .collect();
        CloudDriveType::ALL
            .into_iter()
            .map(|drive| match stored.get(&drive) {
                Some(s) => s.clone(),
                None => AuthState {
                    provider: drive,
                    status: AuthStatus::Unauthenticated,
                    account: None,
                    created_at_ms: None,
                    expires_at_ms: None,
                    last_verified_at_ms: None,
                    last_error: None,
                },
            })
            .collect()
    }

    pub fn logout(&self, drive: CloudDriveType) -> Result<(), String> {
        self.store.delete(drive).map_err(|e| e.to_string())
    }

    // -- 测试连接 (§31): 分步输出, 不只给布尔 --

    pub async fn test_connection(
        &self,
        drive: CloudDriveType,
        probe: Option<(&str, &str, &str)>,
    ) -> Result<ConnectionTest, String> {
        let provider = self.provider(drive)?.clone();
        let mut items: Vec<CheckItem> = Vec::new();

        // ① 登录验证 (账号 API 业务码)
        let credential = match self.load_credential(drive) {
            Ok(Some(c)) => Some(c),
            Ok(None) => None,
            Err(e) => {
                let _ = self
                    .store
                    .set_verify_result(drive, AuthStatus::Invalid, None, Some(&e));
                None
            }
        };
        let verified = match &credential {
            Some(cred) => match provider.verify_credential(cred).await {
                Ok(account) => {
                    let _ = self.store.set_verify_result(
                        drive,
                        AuthStatus::Authenticated,
                        Some(&account),
                        None,
                    );
                    items.push(CheckItem {
                        name: "登录有效".into(),
                        ok: true,
                        detail: account,
                    });
                    true
                }
                Err(e) => {
                    let status = match &e {
                        CloudDriveError::CredentialInvalid(_) => AuthStatus::Invalid,
                        CloudDriveError::CredentialExpired => AuthStatus::Expired,
                        _ => AuthStatus::Unknown,
                    };
                    let _ = self
                        .store
                        .set_verify_result(drive, status, None, Some(&e.user_message(drive)));
                    items.push(CheckItem {
                        name: "登录有效".into(),
                        ok: false,
                        detail: e.user_message(drive),
                    });
                    false
                }
            },
            None => {
                items.push(CheckItem {
                    name: "登录有效".into(),
                    ok: false,
                    detail: "本机无凭证, 请先扫码登录".into(),
                });
                false
            }
        };

        // ② 分享解析 (纯本地, 不依赖登录)
        let share = provider.parse_share(provider.sample_share_url());
        match share {
            Ok(s) => items.push(CheckItem {
                name: "分享解析".into(),
                ok: true,
                detail: format!("share_id={}", s.share_id),
            }),
            Err(e) => items.push(CheckItem {
                name: "分享解析".into(),
                ok: false,
                detail: e.user_message(drive),
            }),
        }

        // ③ 播放解析 (需真实集数 id; 前端可选传入探测目标)
        match probe {
            Some((class, flag, episode)) if verified => {
                match provider.resolve_play_url(class, flag, episode).await {
                    Ok(p) => items.push(CheckItem {
                        name: "播放解析".into(),
                        ok: true,
                        detail: format!("host={}", super::provider::url_host(&p.url)),
                    }),
                    Err(e) => items.push(CheckItem {
                        name: "播放解析".into(),
                        ok: false,
                        detail: e.user_message(drive),
                    }),
                }
            }
            Some((_, _, _)) => items.push(CheckItem {
                name: "播放解析".into(),
                ok: false,
                detail: "登录验证未通过, 跳过".into(),
            }),
            None => items.push(CheckItem {
                name: "播放解析".into(),
                ok: true,
                detail: "未提供测试集数, 跳过 (播放时自动验证)".into(),
            }),
        }

        let ok = items.iter().all(|i| i.ok);
        Ok(ConnectionTest {
            provider: drive,
            ok,
            items,
        })
    }

    // -- 重启恢复 (验收场景 A): 恢复凭证 → 验证 → 重推桥接 --

    /// 应用启动 (桥接就绪后) 调用: 本机已存凭证逐个 verify + push,
    /// 使 APK 内 CookieManager 与持久化凭证对齐 (worker 进程隔离后必须桌面重推)。
    pub async fn restore_and_push(&self) -> Vec<(CloudDriveType, VerifyOutcome)> {
        let mut out = Vec::new();
        for drive in CloudDriveType::ALL {
            let cred = match self.load_credential(drive) {
                Ok(Some(c)) => c,
                Ok(None) => continue,
                Err(e) => {
                    // 密钥更换/数据损坏 → 旧凭证不可用, 明确置 Invalid 促重扫
                    log::warn!("[CloudAuth] provider={} event=restore_decrypt_failed", drive.as_str());
                    let _ = self
                        .store
                        .set_verify_result(drive, AuthStatus::Invalid, None, Some(&e));
                    continue;
                }
            };
            let provider = match self.provider(drive) {
                Ok(p) => p.clone(),
                Err(_) => continue,
            };
            match provider.verify_credential(&cred).await {
                Ok(account) => {
                    let _ = self.store.set_verify_result(
                        drive,
                        AuthStatus::Authenticated,
                        Some(&account),
                        None,
                    );
                    let (pushed, _) = self.push_to_bridge(drive, &cred).await;
                    log::info!(
                        "[CloudAuth] provider={} event=restore_done verified=true pushed={}",
                        drive.as_str(),
                        pushed
                    );
                    out.push((
                        drive,
                        VerifyOutcome {
                            provider: drive,
                            ok: true,
                            account: Some(account),
                            message: "已恢复登录态".into(),
                            bridge_pushed: pushed,
                        },
                    ));
                }
                Err(CloudDriveError::CredentialInvalid(reason)) => {
                    let _ = self.store.set_verify_result(
                        drive,
                        AuthStatus::Expired,
                        None,
                        Some(&reason),
                    );
                    log::info!(
                        "[CloudAuth] provider={} event=restore_done verified=false",
                        drive.as_str()
                    );
                    out.push((
                        drive,
                        VerifyOutcome {
                            provider: drive,
                            ok: false,
                            account: None,
                            message: CloudDriveError::CredentialExpired.user_message(drive),
                            bridge_pushed: false,
                        },
                    ));
                }
                Err(e) => {
                    // 验证请求本身失败 (无网络/桥接离线): 保留原状态, 不误伤
                    log::warn!(
                        "[CloudAuth] provider={} event=restore_verify_network_error",
                        drive.as_str()
                    );
                    out.push((
                        drive,
                        VerifyOutcome {
                            provider: drive,
                            ok: false,
                            account: None,
                            message: e.user_message(drive),
                            bridge_pushed: false,
                        },
                    ));
                }
            }
        }
        out
    }

    // -- 播放链复用 (§18/§20): Manager 是唯一网盘播放入口 --

    pub async fn resolve_play_url(
        &self,
        drive: CloudDriveType,
        class: &str,
        flag: &str,
        episode_id: &str,
    ) -> Result<super::types::PlayResource, CloudDriveError> {
        let provider = self
            .provider(drive)
            .map_err(|_| CloudDriveError::Unsupported)?
            .clone();
        provider.resolve_play_url(class, flag, episode_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clouddrive::types::{CloudDriveCapabilities, PlayResource, ShareResource};
    use crate::resolver::RawPlayResult;

    struct StubFetcher;

    #[async_trait]
    impl CloudPlayFetcher for StubFetcher {
        async fn player_content(
            &self,
            _class: &str,
            _flag: &str,
            _episode_id: &str,
        ) -> Result<RawPlayResult, String> {
            Ok(RawPlayResult {
                url: "http://127.0.0.1:8096/kaiser?url=https%3A%2F%2Fcdn.x.com%2Fv.mp4".into(),
                header: serde_json::json!({"User-Agent": "stub-ua"}),
            })
        }
    }

    struct RecordingSink {
        pushed: std::sync::Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl BridgeCookieSink for RecordingSink {
        async fn push_cookie(&self, drive: &str, cookie: &str) -> Result<(), String> {
            self.pushed.lock().unwrap().push((drive.into(), cookie.into()));
            Ok(())
        }
    }

    /// 可编程验证结果的假 Provider (verify 通过/失败分支都覆盖)
    struct FakeProvider {
        drive: CloudDriveType,
        verify_ok: bool,
    }

    #[async_trait]
    impl CloudDriveProvider for FakeProvider {
        fn id(&self) -> CloudDriveType {
            self.drive
        }
        fn capabilities(&self) -> CloudDriveCapabilities {
            CloudDriveCapabilities::default()
        }
        async fn start_qr_login(&self) -> Result<QrLoginSession, CloudDriveError> {
            Err(CloudDriveError::Unsupported)
        }
        async fn poll_qr_login(
            &self,
            _s: &QrLoginSession,
        ) -> Result<RawLoginOutcome, CloudDriveError> {
            Ok(RawLoginOutcome::Confirmed(ProviderCredential::from_cookie(
                self.drive,
                "k=v",
            )))
        }
        async fn verify_credential(
            &self,
            _c: &ProviderCredential,
        ) -> Result<String, CloudDriveError> {
            if self.verify_ok {
                Ok("测试账号".into())
            } else {
                Err(CloudDriveError::CredentialInvalid("业务码 31001".into()))
            }
        }
        fn parse_share(&self, _url: &str) -> Result<ShareResource, CloudDriveError> {
            Err(CloudDriveError::ShareNotFound)
        }
        async fn resolve_play_url(
            &self,
            _class: &str,
            _flag: &str,
            ep: &str,
        ) -> Result<PlayResource, CloudDriveError> {
            Ok(PlayResource {
                url: format!("https://cdn.x.com/{ep}"),
                headers: HashMap::new(),
                cookies: HashMap::new(),
                user_agent: None,
                referer: None,
                range_supported: true,
                expires_at_ms: None,
            })
        }
        fn sample_share_url(&self) -> &'static str {
            "https://example.com/s/stub"
        }
    }

    fn temp_crypto(tag: &str) -> Arc<CredentialCrypto> {
        // 唯一后缀: 并行测试共享目录会让 remove_dir_all 竞态删掉他人密钥
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "qtv_mgr_test_{tag}_{}_{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Arc::new(CredentialCrypto::load_or_init(&dir).unwrap())
    }

    fn test_manager(verify_ok: bool) -> (CloudDriveManager, Arc<RecordingSink>) {
        let conn = Arc::new(std::sync::Mutex::new(
            rusqlite::Connection::open_in_memory().unwrap(),
        ));
        let store = Arc::new(CredentialStore::from_shared(conn));
        store.init_table().unwrap();
        let sink = Arc::new(RecordingSink {
            pushed: std::sync::Mutex::new(Vec::new()),
        });
        let mut providers: HashMap<CloudDriveType, Arc<dyn CloudDriveProvider>> = HashMap::new();
        for d in CloudDriveType::ALL {
            providers.insert(d, Arc::new(FakeProvider { drive: d, verify_ok }));
        }
        let mgr = CloudDriveManager {
            providers,
            store,
            crypto: temp_crypto(if verify_ok { "ok" } else { "bad" }),
            sink: sink.clone(),
        };
        (mgr, sink)
    }

    #[tokio::test]
    async fn confirmed_login_saves_verifies_and_pushes() {
        let (mgr, sink) = test_manager(true);
        let session = QrLoginSession {
            drive: CloudDriveType::Quark,
            qr: crate::qrcodelogin::QrKind::Text("x".into()),
            token: "t".into(),
            cas_cookies: vec![],
        };
        let r = mgr.poll_login(&session).await.unwrap();
        let LoginResult::Confirmed { verify } = r else {
            panic!("expected confirmed: {r:?}");
        };
        assert!(verify.ok);
        assert!(verify.bridge_pushed);
        assert_eq!(verify.account.as_deref(), Some("测试账号"));
        // §24: 前端可见结构不含 cookie
        assert!(!serde_json::to_string(&verify).unwrap().contains("k=v"));
        assert_eq!(sink.pushed.lock().unwrap().len(), 1);
        // 状态落库 Authenticated
        let states = mgr.states();
        let quark = states.iter().find(|s| s.provider == CloudDriveType::Quark).unwrap();
        assert_eq!(quark.status, AuthStatus::Authenticated);
    }

    #[tokio::test]
    async fn verify_failure_blocks_bridge_push() {
        // §6: 登录确认 ≠ Authenticated — 验证失败既不落 Authenticated 也不推送
        let (mgr, sink) = test_manager(false);
        let session = QrLoginSession {
            drive: CloudDriveType::Uc,
            qr: crate::qrcodelogin::QrKind::Text("x".into()),
            token: "t".into(),
            cas_cookies: vec![],
        };
        let r = mgr.poll_login(&session).await.unwrap();
        let LoginResult::Confirmed { verify } = r else {
            panic!("expected confirmed");
        };
        assert!(!verify.ok);
        assert!(!verify.bridge_pushed);
        assert!(sink.pushed.lock().unwrap().is_empty());
        let states = mgr.states();
        let u = states.iter().find(|s| s.provider == CloudDriveType::Uc).unwrap();
        assert_eq!(u.status, AuthStatus::Invalid);
    }

    #[test]
    fn states_cover_all_drives_even_without_credentials() {
        let (mgr, _) = test_manager(true);
        let states = mgr.states();
        assert_eq!(states.len(), 3);
        assert!(states.iter().all(|s| s.status == AuthStatus::Unauthenticated));
    }

    #[tokio::test]
    async fn restore_roundtrip_decrypt_and_push() {
        // 场景 A: 登录保存 → (模拟重启) restore 从 SQLite 解密重推
        let (mgr, sink) = test_manager(true);
        let cred = ProviderCredential::from_cookie(CloudDriveType::Baidu, "BDUSS=x");
        let enc = mgr.credential_json(&cred).unwrap();
        mgr.store.upsert(&cred, &enc).unwrap();
        let results = mgr.restore_and_push().await;
        assert_eq!(results.len(), 1);
        assert!(results[0].1.ok);
        assert_eq!(sink.pushed.lock().unwrap()[0].0, "baidu");
    }

    #[tokio::test]
    async fn logout_removes_credential() {
        let (mgr, _) = test_manager(true);
        let cred = ProviderCredential::from_cookie(CloudDriveType::Quark, "k=v");
        let enc = mgr.credential_json(&cred).unwrap();
        mgr.store.upsert(&cred, &enc).unwrap();
        mgr.logout(CloudDriveType::Quark).unwrap();
        assert!(mgr.load_credential(CloudDriveType::Quark).unwrap().is_none());
    }

    #[tokio::test]
    async fn resolve_via_spider_unwraps_kaiser_into_play_resource() {
        // §18/§20/§21: kaiser 包装地址 → 内层直链 + 请求头的标准 PlayResource
        let fetcher: Arc<dyn CloudPlayFetcher> = Arc::new(StubFetcher);
        let play = crate::clouddrive::provider::resolve_via_spider(
            &fetcher,
            CloudDriveType::Quark,
            "WexquarkGuard",
            "夸克网盘",
            "pan_123",
        )
        .await
        .unwrap();
        assert_eq!(play.url, "https://cdn.x.com/v.mp4");
        assert_eq!(play.user_agent.as_deref(), Some("stub-ua"));
        assert!(play.range_supported);
    }
}
