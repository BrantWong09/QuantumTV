//! 网盘 Provider 层 Tauri 接线 (ADR 0005)
//!
//! CloudDriveManager 常驻 State: 扫码/验证/凭证加密存储/桥接推送/状态查询/测试连接。
//! 前端只见 AuthState 与 VerifyOutcome, 永远看不到 cookie 本体 (§10/§39)。

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use tauri::{AppHandle, Manager, State};

use quantumtv_core::clouddrive::{
    AuthState, BridgeCookieSink, BridgeCloudPlayFetcher, CloudDriveManager, CloudDriveType,
    CredentialCrypto, CredentialStore, ConnectionTest, LoginResult, QrLoginSession,
};

/// 生产实现: cookie → 桥接 APK /setCookie (CookieManager + 落盘 + 广播 worker)
struct BridgeCookiePusher;

impl BridgeCookiePusher {
    async fn push_inner(drive: &str, cookie: &str) -> Result<(), String> {
        let Some(url) = quantumtv_core::bridge::effective_url() else {
            return Err("桥接未就绪, 请稍后重试".to_string());
        };
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .no_proxy()
            .build()
            .map_err(|e| format!("http client: {e}"))?;
        let resp = client
            .post(format!("{url}/setCookie"))
            .json(&serde_json::json!({ "drive": drive, "cookie": cookie }))
            .send()
            .await
            .map_err(|e| format!("推送桥接失败: {e}"))?;
        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("推送桥接响应异常: {e}"))?;
        if json.get("code").and_then(|v| v.as_i64()) == Some(200) {
            Ok(())
        } else {
            Err(format!("桥接 /setCookie 失败: {json}"))
        }
    }
}

#[async_trait::async_trait]
impl BridgeCookieSink for BridgeCookiePusher {
    async fn push_cookie(&self, drive: &str, cookie: &str) -> Result<(), String> {
        Self::push_inner(drive, cookie).await
    }
}

pub struct CloudDriveState(pub Arc<CloudDriveManager>);

/// 应用启动时构建 (共享主库连接 + app_data 目录密钥)
pub fn init_state(app_data_dir: &Path, conn: Arc<Mutex<Connection>>) -> Result<CloudDriveState, String> {
    let crypto = Arc::new(CredentialCrypto::load_or_init(app_data_dir).map_err(|e| e.to_string())?);
    let store = Arc::new(CredentialStore::from_shared(conn));
    store.init_table().map_err(|e| e.to_string())?;
    let manager = Arc::new(CloudDriveManager::new(
        Arc::new(BridgeCloudPlayFetcher),
        store,
        crypto,
        Arc::new(BridgeCookiePusher),
    ));
    Ok(CloudDriveState(manager))
}

fn parse_drive(drive: &str) -> Result<CloudDriveType, String> {
    CloudDriveType::parse(drive).ok_or_else(|| format!("不支持扫码登录的网盘: {drive}"))
}

/// 取扫码二维码 (桌面端渲染)
#[tauri::command]
pub async fn cloud_login_start(
    drive: String,
    state: State<'_, CloudDriveState>,
) -> Result<QrLoginSession, String> {
    state.0.start_login(parse_drive(&drive)?).await
}

/// 轮询扫码状态; Confirmed 时已完成 保存→verify→桥接推送, 只回验证结论
#[tauri::command]
pub async fn cloud_login_poll(
    session: QrLoginSession,
    state: State<'_, CloudDriveState>,
) -> Result<LoginResult, String> {
    state.0.poll_login(&session).await
}

/// 全部网盘的认证状态 (§30, 免解密)
#[tauri::command]
pub async fn cloud_login_states(
    state: State<'_, CloudDriveState>,
) -> Result<Vec<AuthState>, String> {
    Ok(state.0.states())
}

/// 删除本机凭证 (下次播放需重新扫码)
#[tauri::command]
pub async fn cloud_login_logout(drive: String, state: State<'_, CloudDriveState>) -> Result<(), String> {
    state.0.logout(parse_drive(&drive)?)
}

/// 测试连接 (§31): 登录验证 → 分享解析 → (可选) 播放解析, 分步结果
#[tauri::command]
pub async fn cloud_login_test(
    drive: String,
    probe: Option<(String, String, String)>,
    state: State<'_, CloudDriveState>,
) -> Result<ConnectionTest, String> {
    let d = parse_drive(&drive)?;
    let p = probe.as_ref().map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str()));
    state.0.test_connection(d, p).await
}

/// 桥接就绪后的凭证恢复 (验收场景 A): 解密 → verify → 重推 cookie 到 APK。
/// 桥接瀑布 (隧道/远程/模拟器) 可能持续数秒, 这里带超时等待再推送。
pub fn spawn_restore(handle: &AppHandle) {
    let Some(state) = handle.try_state::<CloudDriveState>() else {
        return;
    };
    let manager = state.0.clone();
    tauri::async_runtime::spawn(async move {
        for _ in 0..30 {
            if quantumtv_core::bridge::effective_url().is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
        if quantumtv_core::bridge::effective_url().is_none() {
            log::info!("[CloudAuth] 桥接未就绪, 跳过启动恢复 (登录态仍在 APK 侧文件)");
            return;
        }
        let results = manager.restore_and_push().await;
        for (drive, verify) in &results {
            log::info!(
                "[CloudAuth] provider={} event=restored verified={} pushed={}",
                drive.as_str(),
                verify.ok,
                verify.bridge_pushed
            );
        }
    });
}
