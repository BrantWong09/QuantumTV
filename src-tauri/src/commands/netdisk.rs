//! 网盘账号管理命令
//!
//! 夸克/UC/百度: 桌面端原生扫码 API (qrcodelogin) → cookie 推送桥接 /setCookie。
//! 其余网盘: 桥接 APK 内 WebView 登录 (CloudLoginActivity → CookieManager)。
//! 混淆 spider 从 CookieManager 读取网盘登录态。

use quantumtv_core::qrcodelogin::{self, PollOutcome, QrSession};
use tauri::State;

use crate::storage::StorageManager;

/// 桌面端跳转到 APK 内网盘登录页
#[tauri::command]
pub async fn netdisk_launch_login(
    drive: String,
    storage: State<'_, StorageManager>,
) -> Result<(), String> {
    if !quantumtv_core::netdisk::DRIVES.contains(&drive.as_str()) {
        return Err(format!("不支持的网盘: {drive}"));
    }
    let data = storage.get_data()?;
    let bridge_cfg = crate::commands::bridge::bridge_config_from_data(&data);
    quantumtv_core::bridge::launch_bridge_activity(&bridge_cfg, "drive", &drive).await
}

/// 取扫码二维码 (桌面端渲染)
#[tauri::command]
pub async fn cloud_login_start(drive: String) -> Result<QrSession, String> {
    qrcodelogin::start(&drive).await
}

/// 轮询扫码状态; Confirmed 时已完成 cookie 交换并推送给桥接
#[tauri::command]
pub async fn cloud_login_poll(session: QrSession) -> Result<PollOutcome, String> {
    match qrcodelogin::poll(&session).await? {
        PollOutcome::Confirmed { cookie } => {
            push_cookie_to_bridge(&session.drive, &cookie).await?;
            Ok(PollOutcome::Confirmed { cookie })
        }
        other => Ok(other),
    }
}

/// cookie → 桥接 APK: CookieManager + 落盘 + 重建 spider
async fn push_cookie_to_bridge(drive: &str, cookie: &str) -> Result<(), String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drives_static_list() {
        for d in ["quark", "uc", "baidu", "ali", "115", "tianyi"] {
            assert!(quantumtv_core::netdisk::DRIVES.contains(&d));
        }
    }
}
