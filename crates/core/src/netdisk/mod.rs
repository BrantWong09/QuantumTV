//! 网盘账号模块(桌面端精简版)
//!
//! 网盘登录已整体迁至桥接 APK 内完成 (CloudLoginActivity: WebView 登录 → Android
//! CookieManager)。wex 混淆 spider 从 CookieManager 直接读取网盘登录态。
//! 桌面端不再存储 cookie / 扫码 / 注入 ext, 此模块仅保留网盘枚举用于约束参数。

use serde::{Deserialize, Serialize};

/// 支持的网盘 key 列表 (与 APK 内 CloudLoginActivity 的 DRIVE_KEYS 对应)
pub const DRIVES: &[&str] = &["quark", "uc", "baidu", "ali", "tianyi", "115", "yidong"];

/// 网盘账号信息 (由 APK 侧持有登录态; 此结构为将来 /health 回传预留)
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct CloudAccount {
    pub drive: String,
    pub nickname: String,
    pub logged_in: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drives_list_contains_major() {
        for d in ["quark", "uc", "baidu", "ali", "tianyi", "115", "yidong"] {
            assert!(DRIVES.contains(&d));
        }
    }
}
