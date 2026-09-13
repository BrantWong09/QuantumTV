//! 网盘层统一类型 (方案 §3/§5/§14/§15/§17/§19-§21/§32)
//!
//! 原则: 认证状态与凭证本体分离 —— AuthState/状态查询不携带凭证,
//! 凭证只在 Provider 内部与加密存储之间流转, 不回传前端 (§39)。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::qrcodelogin::QrKind;

/// 桌面 Provider 化的网盘 (方案 §4: 每网盘独立 Provider)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CloudDriveType {
    Baidu,
    Quark,
    Uc,
}

impl CloudDriveType {
    pub const ALL: [CloudDriveType; 3] = [CloudDriveType::Baidu, CloudDriveType::Quark, CloudDriveType::Uc];

    pub fn as_str(self) -> &'static str {
        match self {
            CloudDriveType::Baidu => "baidu",
            CloudDriveType::Quark => "quark",
            CloudDriveType::Uc => "uc",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "baidu" => Some(CloudDriveType::Baidu),
            "quark" => Some(CloudDriveType::Quark),
            "uc" => Some(CloudDriveType::Uc),
            _ => None,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            CloudDriveType::Baidu => "百度网盘",
            CloudDriveType::Quark => "夸克网盘",
            CloudDriveType::Uc => "UC 网盘",
        }
    }
}

/// 认证状态 (方案 §5; QrScanned ≠ Authenticated, §29)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthStatus {
    #[default]
    Unknown,
    Unauthenticated,
    QrPending,
    Authenticated,
    Expired,
    Invalid,
    Refreshing,
}

/// 统一认证状态 (方案 §5)。凭证本体不在此结构内 (不回传前端, §39)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthState {
    pub provider: CloudDriveType,
    pub status: AuthStatus,
    /// 账号显示名 (网盘 API 返回的昵称; 非敏感字段)
    pub account: Option<String>,
    pub created_at_ms: Option<i64>,
    pub expires_at_ms: Option<i64>,
    pub last_verified_at_ms: Option<i64>,
    pub last_error: Option<String>,
}

/// Provider 自定义凭证 (方案 §15): 不强迫所有网盘适配同一 token 模型。
/// 百度/夸克/UC 当前统一为 cookie 串, 但结构开放给未来 AccessToken/Session 形态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderCredential {
    pub provider: CloudDriveType,
    pub data: serde_json::Value,
    pub created_at_ms: i64,
    pub expires_at_ms: Option<i64>,
}

impl ProviderCredential {
    pub fn from_cookie(provider: CloudDriveType, cookie: &str) -> Self {
        Self {
            provider,
            data: serde_json::json!({ "cookie": cookie }),
            created_at_ms: now_ms(),
            expires_at_ms: None,
        }
    }

    pub fn cookie(&self) -> Option<&str> {
        self.data.get("cookie").and_then(|v| v.as_str())
    }
}

/// 扫码会话 (wire 形态与既有前端契约一致: qr = {kind,data})
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QrLoginSession {
    pub drive: CloudDriveType,
    pub qr: QrKind,
    /// 夸克/UC: cas token; 百度: getqrcode sign
    pub token: String,
    /// 取码响应携带的会话 cookie (k=v)
    pub cas_cookies: Vec<String>,
}

impl QrLoginSession {
    pub(crate) fn from_legacy(s: crate::qrcodelogin::QrSession) -> Result<Self, CloudDriveError> {
        let drive = CloudDriveType::parse(&s.drive).ok_or(CloudDriveError::Unsupported)?;
        Ok(Self {
            drive,
            qr: s.qr,
            token: s.token,
            cas_cookies: s.cas_cookies,
        })
    }

    pub(crate) fn to_legacy(&self) -> crate::qrcodelogin::QrSession {
        crate::qrcodelogin::QrSession {
            drive: self.drive.as_str().to_string(),
            qr: self.qr.clone(),
            token: self.token.clone(),
            cas_cookies: self.cas_cookies.clone(),
        }
    }
}

/// 登录校验结果 (§6/§13): Confirmed 不再裸回 cookie, 前端只看验证结论
#[derive(Debug, Clone, Serialize)]
pub struct VerifyOutcome {
    pub provider: CloudDriveType,
    /// 账号 API 业务码校验通过 (Authenticated 才成立)
    pub ok: bool,
    pub account: Option<String>,
    pub message: String,
    /// cookie 是否成功推送到桥接 APK
    pub bridge_pushed: bool,
}

/// 扫码轮询结果 (§8: 前端不能只看到 success)
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", content = "data", rename_all = "lowercase")]
pub enum LoginResult {
    Waiting,
    Scanned,
    Confirmed { verify: VerifyOutcome },
    Expired,
}

/// Provider 轮询的原始产物: Confirmed 携带凭证本体, 由 Manager 完成
/// 保存(加密) → verify → bridge 推送后组装成前端可见的 LoginResult (§10)
#[derive(Debug, Clone)]
pub enum RawLoginOutcome {
    Waiting,
    Scanned,
    Expired,
    Confirmed(ProviderCredential),
}

/// 分享资源 (§18: 播放不依赖"原始分享 URL", 先结构化)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareResource {
    pub provider: CloudDriveType,
    pub share_url: String,
    pub share_id: String,
    pub passcode: Option<String>,
}

/// 统一文件模型 (方案 §19)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudFile {
    pub provider: CloudDriveType,
    pub file_id: String,
    pub name: String,
    pub size: Option<u64>,
    pub mime_type: Option<String>,
    pub is_video: bool,
    pub parent_id: Option<String>,
}

/// 统一播放资源 (方案 §20/§21): 播放器不需要理解网盘; headers 由 Provider 决定
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayResource {
    pub url: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub cookies: HashMap<String, String>,
    pub user_agent: Option<String>,
    pub referer: Option<String>,
    pub range_supported: bool,
    pub expires_at_ms: Option<i64>,
}

impl PlayResource {
    pub fn from_media_resource(resource: &crate::media::MediaResource) -> Self {
        Self {
            url: resource.url.clone(),
            headers: resource.headers.clone(),
            cookies: resource.cookies.clone(),
            user_agent: resource.user_agent.clone(),
            referer: resource.referer.clone(),
            range_supported: true,
            expires_at_ms: None,
        }
    }
}

/// 能力声明 (方案 §17): 前端据此展示"播放能力", 测试连接据此裁剪检查项
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CloudDriveCapabilities {
    pub login: bool,
    pub qr_login: bool,
    pub share_access: bool,
    pub file_list: bool,
    pub direct_url: bool,
    pub streaming: bool,
    pub range: bool,
    pub requires_cookie: bool,
    pub requires_refresh: bool,
}

impl Default for CloudDriveCapabilities {
    fn default() -> Self {
        Self {
            login: true,
            qr_login: true,
            share_access: true,
            // wex spider 单次 playerContent 完成 share→file→dlink, 桌面无独立文件列表 (ADR 0005 D2)
            file_list: false,
            direct_url: true,
            streaming: true,
            range: true,
            requires_cookie: true,
            requires_refresh: false,
        }
    }
}

/// 结构化错误码 (方案 §32); user_message 给出 §33 的用户可读文案
#[derive(Debug, Clone)]
pub enum CloudDriveError {
    NotLoggedIn,
    CredentialExpired,
    CredentialInvalid(String),
    QrExpired,
    QrCancelled,
    QrLoginFailed(String),
    ShareNotFound,
    FileNotFound,
    PermissionDenied,
    PlaybackUrlExpired,
    PlaybackResolveFailed(String),
    RateLimited,
    NetworkError(String),
    StorageError(String),
    Unsupported,
}

impl CloudDriveError {
    pub fn user_message(&self, provider: CloudDriveType) -> String {
        let name = provider.display_name();
        match self {
            CloudDriveError::NotLoggedIn => format!("{name}尚未登录, 请到 管理 → 网盘账号 扫码登录"),
            CloudDriveError::CredentialExpired => format!("{name}登录已过期, 请重新扫码"),
            CloudDriveError::CredentialInvalid(reason) => {
                format!("{name}登录状态无效, 请重新登录 ({reason})")
            }
            CloudDriveError::QrExpired => format!("{name}二维码已过期, 请刷新重试"),
            CloudDriveError::QrCancelled => format!("{name}登录已取消"),
            CloudDriveError::QrLoginFailed(reason) => format!("{name}扫码登录失败: {reason}"),
            CloudDriveError::ShareNotFound => format!("{name}分享不存在或已失效"),
            CloudDriveError::FileNotFound => format!("{name}文件不存在"),
            CloudDriveError::PermissionDenied => format!("{name}无访问权限 (可能需要会员或转存)"),
            CloudDriveError::PlaybackUrlExpired => {
                format!("{name}文件解析成功, 但播放地址已失效, 请重试播放")
            }
            CloudDriveError::PlaybackResolveFailed(reason) => {
                format!("{name}播放解析失败: {reason}")
            }
            CloudDriveError::RateLimited => format!("{name}请求过于频繁, 请稍后重试"),
            CloudDriveError::NetworkError(reason) => format!("{name}网络请求失败: {reason}"),
            CloudDriveError::StorageError(reason) => format!("{name}凭证存储异常: {reason}"),
            CloudDriveError::Unsupported => format!("{name}暂不支持该操作"),
        }
    }
}

impl std::fmt::Display for CloudDriveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // 独立于具体网盘的场合 (如存储层) 用通用文案
            CloudDriveError::StorageError(r) => write!(f, "凭证存储异常: {r}"),
            CloudDriveError::Unsupported => write!(f, "暂不支持该操作"),
            other => write!(f, "{}", other.user_message(CloudDriveType::Quark)),
        }
    }
}

/// "测试连接"单项检查 (方案 §31)
#[derive(Debug, Clone, Serialize)]
pub struct CheckItem {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

/// "测试连接"结果 (方案 §31): 分步呈现, 不只给一个布尔
#[derive(Debug, Clone, Serialize)]
pub struct ConnectionTest {
    pub provider: CloudDriveType,
    pub ok: bool,
    pub items: Vec<CheckItem>,
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 识别 (spider 类名, 线路 flag) 属于哪个网盘。
/// wex 单个类可同时服务多网盘 (如 WexmuouggGuard), flag(线路名)比类名更准确,
/// 判定顺序与 spider::netdisk_login_hint_for 保持一致。
pub fn detect_provider(class: &str, flag: &str) -> Option<CloudDriveType> {
    let flag_lower = flag.to_lowercase();
    let class_lower = class.to_lowercase();
    if flag_lower.contains("quark") || flag.contains("夸克") {
        Some(CloudDriveType::Quark)
    } else if flag_lower.contains("uc") || flag.contains("UC网盘") {
        Some(CloudDriveType::Uc)
    } else if flag.contains("百度") {
        Some(CloudDriveType::Baidu)
    } else if class_lower.contains("quark") || class_lower.contains("kuake") {
        Some(CloudDriveType::Quark)
    } else if class_lower.contains("baidu") {
        Some(CloudDriveType::Baidu)
    } else if class_lower.contains("uc") {
        Some(CloudDriveType::Uc)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_type_parse_and_serde() {
        assert_eq!(CloudDriveType::parse("quark"), Some(CloudDriveType::Quark));
        assert_eq!(CloudDriveType::parse(" UC "), Some(CloudDriveType::Uc));
        assert_eq!(CloudDriveType::parse("baidu"), Some(CloudDriveType::Baidu));
        assert_eq!(CloudDriveType::parse("115"), None);
        assert_eq!(
            serde_json::to_string(&CloudDriveType::Baidu).unwrap(),
            "\"baidu\""
        );
    }

    #[test]
    fn credential_cookie_roundtrip() {
        let cred = ProviderCredential::from_cookie(CloudDriveType::Quark, "__puus=abc; k=v");
        assert_eq!(cred.cookie(), Some("__puus=abc; k=v"));
        assert_eq!(cred.provider, CloudDriveType::Quark);
    }

    #[test]
    fn qr_session_wire_shape_unchanged() {
        // 前端契约: drive 是小写字符串, qr 是 {kind,data}
        let s = QrLoginSession {
            drive: CloudDriveType::Uc,
            qr: QrKind::Text("https://su.uc.cn/x".into()),
            token: "tk".into(),
            cas_cookies: vec!["a=1".into()],
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["drive"], "uc");
        assert_eq!(json["qr"]["kind"], "Text");
        let back: QrLoginSession = serde_json::from_value(json).unwrap();
        assert_eq!(back.drive, CloudDriveType::Uc);
    }

    #[test]
    fn legacy_session_conversion() {
        let legacy = crate::qrcodelogin::QrSession {
            drive: "baidu".into(),
            qr: QrKind::PngBase64("img".into()),
            token: "sign".into(),
            cas_cookies: vec![],
        };
        let converted = QrLoginSession::from_legacy(legacy).unwrap();
        assert_eq!(converted.drive, CloudDriveType::Baidu);
        let back = converted.to_legacy();
        assert_eq!(back.drive, "baidu");
    }

    #[test]
    fn login_result_confirmed_hides_cookie() {
        // §24/§39: 确认结果不含 cookie 本体
        let r = LoginResult::Confirmed {
            verify: VerifyOutcome {
                provider: CloudDriveType::Quark,
                ok: true,
                account: Some("nick".into()),
                message: "验证通过".into(),
                bridge_pushed: true,
            },
        };
        let text = serde_json::to_string(&r).unwrap();
        assert!(text.contains("\"status\":\"confirmed\""));
        assert!(!text.contains("cookie"));
        assert!(text.contains("nick"));
    }

    #[test]
    fn detect_provider_flag_wins_over_class() {
        assert_eq!(
            detect_provider("WexmuouggGuard", "夸克网盘"),
            Some(CloudDriveType::Quark)
        );
        assert_eq!(
            detect_provider("WexmuouggGuard", "UC网盘"),
            Some(CloudDriveType::Uc)
        );
        assert_eq!(
            detect_provider("WexWoBaiduPanGuard", "百度原画#01"),
            Some(CloudDriveType::Baidu)
        );
        assert_eq!(detect_provider("WexquarkGuard", ""), Some(CloudDriveType::Quark));
        assert_eq!(detect_provider("WexAliSomethingGuard", ""), None);
        assert_eq!(detect_provider("UnknownGuard", "其他"), None);
    }

    #[test]
    fn error_user_messages_follow_plan() {
        let e = CloudDriveError::CredentialExpired;
        assert_eq!(e.user_message(CloudDriveType::Quark), "夸克网盘登录已过期, 请重新扫码");
        let e = CloudDriveError::CredentialInvalid("业务码 4".into());
        assert!(e.user_message(CloudDriveType::Uc).contains("UC 网盘登录状态无效"));
        let e = CloudDriveError::PlaybackUrlExpired;
        assert!(e.user_message(CloudDriveType::Uc).contains("播放地址已失效"));
    }
}
