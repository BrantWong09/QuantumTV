//! 网盘账号管理命令
//!
//! 网盘登录已迁至桥接 APK 内 (CloudLoginActivity: WebView 登录 → 系统 CookieManager,
//! 混淆 spider 从 CookieManager 读取网盘登录态)。
//! 桌面端只负责「拉起 APK 登录页」，不再存储/注入网盘 cookie。

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
