use serde::{Deserialize, Serialize};
use tauri::{Manager, State};

use crate::storage::StorageManager;

/// 桥接设置（持久化在 data.json 的 config.BridgeConfig）
/// serde(default) 兼容旧数据缺字段；auto_scan 缺省 true 经字段级 default 处理
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct BridgeSettingsDto {
    pub remote_url: String,
    /// 逗号/换行分隔的 adb connect 地址
    pub adb_addresses: String,
    pub auto_scan: bool,
}

impl Default for BridgeSettingsDto {
    fn default() -> Self {
        Self { remote_url: String::new(), adb_addresses: String::new(), auto_scan: true }
    }
}

#[derive(Debug, Serialize)]
pub struct BridgeStatusDto {
    pub status: String,
    pub effective_url: Option<String>,
    pub mode: String,
}

fn bridge_settings_from_config(config: &serde_json::Value) -> Option<BridgeSettingsDto> {
    config
        .get("BridgeConfig")
        .and_then(|v| serde_json::from_value::<BridgeSettingsDto>(v.clone()).ok())
}

fn validate_settings(s: &BridgeSettingsDto) -> Result<(), String> {
    let remote = s.remote_url.trim();
    if !remote.is_empty() {
        let parsed = url::Url::parse(remote).map_err(|e| format!("远程桥接地址无效: {}", e))?;
        if parsed.scheme() != "http" && parsed.scheme() != "https" {
            return Err("远程桥接地址仅支持 http/https".to_string());
        }
    }
    for addr in s.adb_addresses.split([',', '\n']).map(str::trim).filter(|a| !a.is_empty()) {
        let Some((_host, port)) = addr.rsplit_once(':') else {
            return Err(format!("adb 地址格式无效（应为 host:port）: {}", addr));
        };
        port.parse::<u16>().map_err(|_| format!("adb 端口无效: {}", addr))?;
    }
    Ok(())
}

/// UI 优先、env 兜底合并为 core 的配置 map（UI 三个键始终写入，空值表示显式清除）
fn build_bridge_map(s: &BridgeSettingsDto) -> std::collections::HashMap<String, String> {
    let mut m = std::collections::HashMap::new();
    for k in [
        "QUANTUMTV_BRIDGE_ENABLED",
        "QUANTUMTV_BRIDGE_AVD",
        "QUANTUMTV_BRIDGE_SDK",
        "QUANTUMTV_ADB_HOST_PORT",
        "QUANTUMTV_BRIDGE_URL",
        "QUANTUMTV_BRIDGE_APK",
        "QUANTUMTV_BRIDGE_REMOTE_URL",
        "QUANTUMTV_BRIDGE_ADB_ADDRESSES",
        "QUANTUMTV_BRIDGE_AUTO_SCAN",
    ] {
        if let Ok(v) = std::env::var(k) {
            if !v.is_empty() {
                m.insert(k.to_string(), v);
            }
        }
    }
    let addrs = s
        .adb_addresses
        .split([',', '\n'])
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .collect::<Vec<_>>()
        .join(",");
    m.insert("QUANTUMTV_BRIDGE_REMOTE_URL".to_string(), s.remote_url.trim().to_string());
    m.insert("QUANTUMTV_BRIDGE_ADB_ADDRESSES".to_string(), addrs);
    m.insert("QUANTUMTV_BRIDGE_AUTO_SCAN".to_string(), if s.auto_scan { "1" } else { "0" }.to_string());
    m
}

/// Starting 防重入检查 + 关闭自拉起旧模拟器 + 后台重跑瀑布
fn spawn_retry(settings: BridgeSettingsDto) -> Result<(), String> {
    if quantumtv_core::bridge::status() == quantumtv_core::bridge::BridgeStatus::Starting {
        return Err("桥接正在启动中，请稍后再试".to_string());
    }
    let cfg = quantumtv_core::bridge::BridgeConfig::from_map(&build_bridge_map(&settings));
    tauri::async_runtime::spawn(async move {
        if quantumtv_core::bridge::we_started() {
            quantumtv_core::bridge::shutdown().await;
        }
        quantumtv_core::bridge::reset_effective();
        if let Err(e) = quantumtv_core::bridge::ensure_ready_with(cfg).await {
            log::warn!("[桥接] 桥接重试失败: {}", e);
        }
    });
    Ok(())
}

fn persist_settings(settings: &BridgeSettingsDto, state: &State<'_, StorageManager>) -> Result<(), String> {
    let mut data = state.get_data()?;
    if !data.config.is_object() {
        data.config = serde_json::json!({});
    }
    data.config
        .as_object_mut()
        .unwrap()
        .insert("BridgeConfig".to_string(), serde_json::to_value(settings).map_err(|e| e.to_string())?);
    state.update_config(data.config)
}

/// 获取桥接设置
#[tauri::command]
pub async fn get_bridge_config(state: State<'_, StorageManager>) -> Result<BridgeSettingsDto, String> {
    let data = state.get_data()?;
    Ok(bridge_settings_from_config(&data.config).unwrap_or_default())
}

/// 保存桥接设置并触发后台重连
#[tauri::command]
pub async fn save_bridge_config(
    settings: BridgeSettingsDto,
    state: State<'_, StorageManager>,
) -> Result<(), String> {
    validate_settings(&settings)?;
    persist_settings(&settings, &state)?;
    spawn_retry(settings)
}

/// 桥接状态（供前端轮询）
#[tauri::command]
pub async fn get_bridge_status() -> BridgeStatusDto {
    let status = match quantumtv_core::bridge::status() {
        quantumtv_core::bridge::BridgeStatus::Idle => "idle",
        quantumtv_core::bridge::BridgeStatus::Starting => "starting",
        quantumtv_core::bridge::BridgeStatus::Ready => "ready",
        quantumtv_core::bridge::BridgeStatus::Failed => "failed",
    };
    let mode = match quantumtv_core::bridge::effective_kind() {
        quantumtv_core::bridge::EFFECTIVE_REMOTE => "remote",
        quantumtv_core::bridge::EFFECTIVE_EMULATOR => "emulator",
        quantumtv_core::bridge::EFFECTIVE_AVD => "avd",
        _ => "none",
    };
    BridgeStatusDto {
        status: status.to_string(),
        effective_url: quantumtv_core::bridge::effective_url(),
        mode: mode.to_string(),
    }
}

/// 手动重试（不改动已保存配置）
#[tauri::command]
pub async fn retry_bridge(state: State<'_, StorageManager>) -> Result<(), String> {
    let settings = {
        let data = state.get_data()?;
        bridge_settings_from_config(&data.config).unwrap_or_default()
    };
    spawn_retry(settings)
}

/// 应用启动钩子: UI 配置存在则 UI 优先合并 env，否则纯 env（向后兼容）
pub async fn startup_ensure(handle: tauri::AppHandle) {
    let settings = {
        let storage = handle.state::<StorageManager>();
        storage.get_data().ok().and_then(|d| bridge_settings_from_config(&d.config))
    };
    let cfg = match settings {
        Some(s) => quantumtv_core::bridge::BridgeConfig::from_map(&build_bridge_map(&s)),
        None => quantumtv_core::bridge::BridgeConfig::from_env(),
    };
    if let Err(e) = quantumtv_core::bridge::ensure_ready_with(cfg).await {
        log::warn!("[桥接] 后台拉起失败: {}", e);
    }
}
