//! 网盘账号管理命令: 扫码登录 / cookie 存储 / 续期 / 账号状态
//!
//! 存储位置: data.json 的 config.CloudCookies = { quark: {...}, uc: {...}, ... }
//! 注入桥接: cookie 变更后写入 core 全局, 桥接 /init 的 ext 使用

use quantumtv_core::netdisk::{self, CloudAccount, ScanSession};
use serde::{Deserialize, Serialize};
use tauri::{Manager, State};

use crate::storage::StorageManager;

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct CloudCookiesConfig {
    #[serde(flatten)]
    pub accounts: std::collections::HashMap<String, CloudAccount>,
}

fn cloud_cookies_from_config(config: &serde_json::Value) -> CloudCookiesConfig {
    config
        .get("CloudCookies")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

fn persist_accounts(
    accounts: &CloudCookiesConfig,
    state: &State<'_, StorageManager>,
) -> Result<(), String> {
    let mut data = state.get_data()?;
    if !data.config.is_object() {
        data.config = serde_json::json!({});
    }
    data.config
        .as_object_mut()
        .unwrap()
        .insert("CloudCookies".to_string(), serde_json::to_value(accounts).map_err(|e| e.to_string())?);
    state.update_config(data.config)
}

/// 扫码登录: 开始会话 (返回二维码内容)
#[tauri::command]
pub async fn netdisk_scan_start(drive: String) -> Result<ScanSession, String> {
    match drive.as_str() {
        "quark" => netdisk::quark_scan_start().await,
        "uc" => Err("UC 扫码登录将在后续版本提供，请先粘贴 Cookie".into()),
        other => Err(format!("{other} 暂不支持扫码，请粘贴 Cookie")),
    }
}

#[derive(Debug, Serialize)]
pub struct ScanPollResponse {
    pub state: String,
    pub account: Option<CloudAccount>,
}

/// 扫码登录: 轮询 (确认后自动落盘并触发桥接刷新)
#[tauri::command]
pub async fn netdisk_scan_poll(
    drive: String,
    token: String,
    state: State<'_, StorageManager>,
) -> Result<ScanPollResponse, String> {
    if drive != "quark" {
        return Err(format!("{drive} 暂不支持扫码"));
    }
    let (scan_state, account) = netdisk::quark_scan_poll(&token).await?;
    let mut confirmed = ScanPollResponse { state: scan_state, account: account.clone() };
    if let Some(acc) = account {
        let mut accounts = cloud_cookies_from_config(&state.get_data()?.config);
        accounts.accounts.insert(drive.clone(), acc);
        persist_accounts(&accounts, &state)?;
        // 通知桥接刷新 ext
        quantumtv_core::bridge::mark_ext_dirty();
        confirmed.state = "confirmed".into();
    }
    Ok(confirmed)
}

/// 账号列表
#[tauri::command]
pub async fn netdisk_get_accounts(
    state: State<'_, StorageManager>,
) -> Result<Vec<CloudAccount>, String> {
    let accounts = cloud_cookies_from_config(&state.get_data()?.config);
    let mut list: Vec<CloudAccount> = accounts.accounts.into_values().collect();
    list.sort_by(|a, b| a.drive.cmp(&b.drive));
    Ok(list)
}

/// 手动粘贴 cookie
#[tauri::command]
pub async fn netdisk_save_cookie(
    drive: String,
    cookie: String,
    state: State<'_, StorageManager>,
) -> Result<(), String> {
    let cookie = cookie.trim().to_string();
    if cookie.is_empty() {
        return Err("Cookie 不能为空".into());
    }
    if !quantumtv_core::netdisk::DRIVES.contains(&drive.as_str()) {
        return Err(format!("不支持的网盘: {drive}"));
    }
    let mut accounts = cloud_cookies_from_config(&state.get_data()?.config);
    accounts.accounts.insert(
        drive.clone(),
        CloudAccount { drive, cookie, updated_at: now(), nickname: String::new() },
    );
    persist_accounts(&accounts, &state)?;
    quantumtv_core::bridge::mark_ext_dirty();
    Ok(())
}

/// 删除账号
#[tauri::command]
pub async fn netdisk_delete_account(
    drive: String,
    state: State<'_, StorageManager>,
) -> Result<(), String> {
    let mut accounts = cloud_cookies_from_config(&state.get_data()?.config);
    accounts.accounts.remove(&drive);
    persist_accounts(&accounts, &state)?;
    quantumtv_core::bridge::mark_ext_dirty();
    Ok(())
}

/// 立即续期 (夸克/UC)
#[tauri::command]
pub async fn netdisk_refresh_now(
    drive: String,
    state: State<'_, StorageManager>,
) -> Result<CloudAccount, String> {
    let mut accounts = cloud_cookies_from_config(&state.get_data()?.config);
    let acc = accounts
        .accounts
        .get(&drive)
        .cloned()
        .ok_or_else(|| format!("{drive} 未登录"))?;
    let new_cookie = netdisk::refresh_cookie(&drive, &acc.cookie).await?;
    let updated = CloudAccount { cookie: new_cookie, updated_at: now(), ..acc };
    accounts.accounts.insert(drive.clone(), updated.clone());
    persist_accounts(&accounts, &state)?;
    quantumtv_core::bridge::mark_ext_dirty();
    Ok(updated)
}

/// 有效账号的 ext JSON (注入桥接 /init), 无账号返回 None
pub fn build_ext_from_storage(state: &StorageManager) -> Option<String> {
    let accounts = cloud_cookies_from_config(&state.get_data().ok()?.config);
    if accounts.accounts.is_empty() {
        return None;
    }
    let mut obj = serde_json::Map::new();
    for (drive, acc) in &accounts.accounts {
        if acc.cookie.is_empty() {
            continue;
        }
        // token.json 生态标准字段名
        let key = match drive.as_str() {
            "quark" => "quark_cookie",
            "uc" => "uc_cookie",
            "baidu" => "baidu_cookie",
            "ali" => "ali_cookie",
            "tianyi" => "tianyi_cookie",
            "115" => "115_cookie",
            "yidong" => "yidong_cookie",
            _ => continue,
        };
        obj.insert(key.to_string(), serde_json::Value::String(acc.cookie.clone()));
    }
    if obj.is_empty() {
        return None;
    }
    serde_json::to_string(&serde_json::Value::Object(obj)).ok()
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// 续期调度: 每 90 分钟对夸克/UC 账号执行一次续期
pub async fn start_refresh_scheduler(app: tauri::AppHandle) {
    let mut last_run = std::time::Instant::now() - std::time::Duration::from_secs(90 * 60);
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(300)).await; // 5 分钟心跳
        if last_run.elapsed() < std::time::Duration::from_secs(90 * 60) {
            continue;
        }
        last_run = std::time::Instant::now();
        let storage = app.state::<StorageManager>();
        let accounts = {
            let Ok(data) = storage.get_data() else { continue };
            cloud_cookies_from_config(&data.config)
        };
        for (drive, acc) in accounts.accounts {
            if drive != "quark" && drive != "uc" {
                continue;
            }
            if let Ok(new_cookie) = netdisk::refresh_cookie(&drive, &acc.cookie).await {
                if new_cookie != acc.cookie {
                    let mut accounts = cloud_cookies_from_config(&storage.get_data().unwrap().config);
                    accounts.accounts.insert(
                        drive.clone(),
                        CloudAccount { cookie: new_cookie, updated_at: now(), ..acc.clone() },
                    );
                    let _ = persist_accounts(&accounts, &storage);
                    quantumtv_core::bridge::mark_ext_dirty();
                    log::info!("[网盘] {} cookie 已自动续期", drive);
                }
            } else {
                log::warn!("[网盘] {} cookie 续期失败(可能已登出), 保留原值", drive);
            }
        }
    }
}
