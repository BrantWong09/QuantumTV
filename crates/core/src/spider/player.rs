//! 网盘播放解析: spider 集数 id → playerContent 二次解析 → 真实直链
//!
//! wex 全系站点是网盘资源模式: detailContent 返回的"集数"是网盘分享 id(非 http),
//! 播放前必须经 playerContent(flag, id) 解析。未登录网盘时解析结果为空 → 前端给出登录提示。

use serde_json::Value;

use super::bridge_post_with;

use crate::resolver::{RawPlayResult, ResolveError, SpiderPlayFetcher};

/// 把 spider 错误字符串归类为 Resolver 错误模型。
/// 桥接透传的 Java 异常 (InvocationTargetException/JSONException/bridge error)
/// 几乎都是 spider 内部调网盘 API 失败 (未登录网盘/cookie 失效) → 认证类。
/// 注意: 会话层超时/断连也走 "bridge error: bridge_timeout" 形态, 必须先于
/// 泛化认证分支判定, 否则超时会被谎报成"需要登录网盘"。
pub(crate) fn classify_spider_error(class_name: &str, flag: &str, err: &str) -> ResolveError {
    let source = "spider".to_string();
    let short = short_err(err);
    if err.contains("bridge_timeout") || err.contains("timed out") || err.contains("timeout")
        || err.contains("超时")
    {
        return ResolveError::Timeout {
            source,
            message: short,
        };
    }
    if err.contains("bridge_disconnected") || err.contains("bridge_send_failed")
        || err.contains("bridge_not_connected")
    {
        return ResolveError::NetworkError {
            source,
            message: "桥接连接已断开, 请确认手机/模拟器桥接在线后重试".into(),
        };
    }
    // Worker 独立进程故障 (§36): kill/restart/disable/melt-open → "执行环境故障", 不是登录问题
    if err.contains("worker_killed") || err.contains("worker_restarting")
        || err.contains("worker_disabled") || err.contains("worker_unavailable")
        || err.contains("source_circuit_open") || err.contains("bridge_circuit_open")
    {
        return ResolveError::NetworkError {
            source,
            message: "桥接执行环境故障(正在恢复), 请稍后重试".into(),
        };
    }
    if err.contains("InvocationTargetException")
        || err.contains("JSONException")
        || err.contains("bridge error")
    {
        return ResolveError::AuthenticationRequired {
            source,
            message: format!(
                "{} (原因: {})",
                netdisk_login_hint_for(class_name, flag),
                short
            ),
        };
    }
    ResolveError::NetworkError {
        source,
        message: short,
    }
}

/// [`SpiderPlayFetcher`] 的桥接实现: 经 Android Bridge /playerContent 拉取播放结果。
/// 这是 SpiderResolver 在生产环境的执行器 (test 用桩注入)。
pub struct BridgeSpiderPlayFetcher {
    pub bridge_url: String,
}

#[async_trait::async_trait]
impl SpiderPlayFetcher for BridgeSpiderPlayFetcher {
    async fn player_content(
        &self,
        class_name: &str,
        flag: &str,
        episode_id: &str,
    ) -> Result<RawPlayResult, ResolveError> {
        resolve_spider_episode_raw(class_name, flag, episode_id, &self.bridge_url).await
    }
}

/// 原始形态解析: 返回 (url, header), 不做登录提示包装 (错误归类为 ResolveError)。
/// [`resolve_spider_episode`] 是它的用户可读文案包装, 两者共用同一 bridge 调用。
async fn resolve_spider_episode_raw(
    class_name: &str,
    flag: &str,
    id: &str,
    bridge_url: &str,
) -> Result<RawPlayResult, ResolveError> {
    log::info!(
        "[播放解析] playerContent class={} flag={:?} id={}",
        class_name,
        flag,
        super::trunc(id, 80)
    );
    let body = serde_json::json!({
        "class": class_name,
        "flag": flag,
        "id": id,
    });
    let data = bridge_post_with(bridge_url, "/playerContent", &body, false, 120)
        .await
        .map_err(|e| {
            log::warn!("[播放解析] playerContent 失败: {}", super::trunc(&e, 300));
            classify_spider_error(class_name, flag, &e)
        })?;
    let obj: Value = serde_json::from_str(&data).map_err(|e| ResolveError::ParseError {
        source: "spider".into(),
        message: format!(
            "playerContent 响应解析失败: {e}, body: {}",
            &data[..data.len().min(120)]
        ),
    })?;
    let url = obj["url"].as_str().unwrap_or("").trim().to_string();
    if url.is_empty() {
        log::warn!(
            "[播放解析] playerContent 返回空 url (可能未登录对应网盘): header={}",
            obj["header"]
        );
        return Err(ResolveError::AuthenticationRequired {
            source: "spider".into(),
            message: netdisk_login_hint(class_name),
        });
    }
    // 直链可能带敏感签名, 只打印前缀和长度
    log::info!(
        "[播放解析] 解析成功: url={} ({} bytes) header={}",
        super::trunc(&url, 100),
        url.len(),
        obj["header"]
    );
    Ok(RawPlayResult {
        url,
        header: obj["header"].clone(),
    })
}

/// 解析单集: 返回 (直链, header 对象)
/// Err 携带用户可读原因(如"需要登录夸克网盘")
pub async fn resolve_spider_episode(
    class_name: &str,
    flag: &str,
    id: &str,
    bridge_url: &str,
) -> Result<(String, Value), String> {
    let raw = resolve_spider_episode_raw(class_name, flag, id, bridge_url)
        .await
        .map_err(|e| e.to_string())?;
    Ok((raw.url, raw.header))
}

/// 识别 spider 返回的"本地代理包装"直链 (wex 系 kaiser: http://127.0.0.1:8096/kaiser?url=<内层>)
/// 解出内层真实直链。播放器在桌面端无法访问手机/模拟器本机的代理端口,
/// 但 MuMu 模拟器与宿主同出口 IP, 百度 PCS 的 dlink 又按 IP 绑定 → 内层直链桌面可直接用。
pub fn unwrap_local_proxy_url(url: &str) -> Option<String> {
    let lower = url.to_lowercase();
    if !(lower.starts_with("http://127.0.0.1") || lower.starts_with("http://localhost")) {
        return None;
    }
    // kaiser 形态: /kaiser?url=<urlencoded 内层直链>
    let inner = url
        .split_once("url=")
        .map(|(_, rest)| rest)
        .and_then(|rest| rest.split(['&', '#']).next())
        .filter(|s| !s.is_empty())?;
    match urldecode(inner) {
        decoded if decoded.starts_with("http://") || decoded.starts_with("https://") => {
            log::info!("[播放解析] 本地代理包装识别: 内层直链 host={}", host_of(&decoded));
            Some(decoded)
        }
        _ => None,
    }
}

/// 提取 spider 返回 header 中的 User-Agent (百度 PCS 校验严格, 必须原样携带)
pub fn header_user_agent(header: &Value) -> Option<String> {
    // wex 系 playerContent 的 header 字段有两种形态:
    //   对象: {"User-Agent": "..."}
    //   字符串: "{\"User-Agent\": \"...\"}"  (日志实证 wex 返回的是这种, serde 原样透传)
    let obj = match header {
        Value::String(s) => serde_json::from_str::<Value>(s).unwrap_or(Value::Null),
        other => other.clone(),
    };
    obj.as_object()?
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("User-Agent"))
        .and_then(|(_, v)| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn host_of(url: &str) -> String {
    url.split("//")
        .nth(1)
        .and_then(|rest| rest.split(['/', ':']).next())
        .unwrap_or("")
        .to_string()
}

/// 最小 percent-decoding (kaiser 的 url= 参数只含 %XX 转义; 大小写十六进制都接受)
fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unwrap_kaiser_url() {
        let inner = "https://d.pcs.baidu.com/file/abc?fid=1&rt=sh";
        let wrapped = format!(
            "http://127.0.0.1:8096/kaiser?url={}",
            "https%3A%2F%2Fd.pcs.baidu.com%2Ffile%2Fabc%3Ffid%3D1%26rt%3Dsh"
        );
        assert_eq!(unwrap_local_proxy_url(&wrapped).as_deref(), Some(inner));
    }

    #[test]
    fn unwrap_rejects_direct_urls() {
        assert!(unwrap_local_proxy_url("https://d.pcs.baidu.com/file/x").is_none());
        assert!(unwrap_local_proxy_url("http://127.0.0.1:8096/kaiser?url=").is_none());
        assert!(unwrap_local_proxy_url("").is_none());
    }

    #[test]
    fn unwrap_accepts_localhost_form() {
        let wrapped = "http://localhost:8096/kaiser?url=http%3A%2F%2Fx.com%2Fa.mp4";
        assert_eq!(
            unwrap_local_proxy_url(wrapped).as_deref(),
            Some("http://x.com/a.mp4")
        );
    }

    #[test]
    fn user_agent_extracted_case_insensitive() {
        assert_eq!(
            header_user_agent(&json!({"User-Agent": "  ExoPlayer "})).as_deref(),
            Some("ExoPlayer")
        );
        assert_eq!(
            header_user_agent(&json!({"user-agent": "abc"})).as_deref(),
            Some("abc")
        );
        assert!(header_user_agent(&json!({"Referer": "x"})).is_none());
        assert!(header_user_agent(&Value::Null).is_none());
        // wex 实际形态: header 是 JSON 字符串(转义), 而非对象
        let string_form = json!(r#"{"User-Agent":"com.android.chrome/131.0.6778.200"}"#);
        assert_eq!(
            header_user_agent(&string_form).as_deref(),
            Some("com.android.chrome/131.0.6778.200")
        );
        assert!(header_user_agent(&json!("not json")).is_none());
    }
}

/// 依据 spider 类名推断需要登录的网盘, 生成用户提示
pub fn netdisk_login_hint(class_name: &str) -> String {
    let lower = class_name.to_lowercase();
    if lower.contains("quark") || lower.contains("kuake") {
        "该源需要登录夸克网盘: 请到 管理 → 网盘账号 扫码登录".into()
    } else if lower.contains("ali") {
        "该源需要登录阿里云盘: 请到 管理 → 网盘账号 粘贴 Cookie".into()
    } else if lower.contains("baidu") {
        "该源需要登录百度网盘: 请到 管理 → 网盘账号 粘贴 Cookie".into()
    } else if lower.contains("uc") {
        "该源需要登录 UC 网盘: 请到 管理 → 网盘账号 粘贴 Cookie".into()
    } else {
        // wex 全系默认是夸克/UC 系网盘资源
        "该源为网盘资源, 需要登录网盘账号: 请到 管理 → 网盘账号 扫码/粘贴登录".into()
    }
}

/// flag(线路名)比类名更准确: 它直接就是网盘名(如 "夸克网盘"/"UC网盘"/"百度原画")
pub fn netdisk_login_hint_for(class_name: &str, flag: &str) -> String {
    let flag_lower = flag.to_lowercase();
    let where_to = "请到 管理 → 网盘账号 扫码/粘贴登录";
    if flag_lower.contains("quark") || flag.contains("夸克") {
        format!("该线路需要登录夸克网盘: {where_to}")
    } else if flag_lower.contains("uc") || flag.contains("UC网盘") {
        format!("该线路需要登录 UC 网盘: {where_to}")
    } else if flag.contains("百度") {
        format!("该线路需要登录百度网盘: {where_to}")
    } else if flag.contains("阿里") {
        format!("该线路需要登录阿里云盘: {where_to}")
    } else if flag.contains("天翼") {
        format!("该线路需要登录天翼云盘: {where_to}")
    } else if flag.contains("115") {
        format!("该线路需要登录 115 网盘: {where_to}")
    } else if flag.contains("迅雷") {
        format!("该线路需要登录迅雷云盘: {where_to}")
    }
    // flag 无信息时退回按类名推断
    else {
        netdisk_login_hint(class_name)
    }
}

/// 把桥接透传的长异常串压成一行要点
fn short_err(e: &str) -> String {
    let trimmed = e.trim();
    let end = trimmed.char_indices().nth(120).map(|(i, _)| i).unwrap_or(trimmed.len());
    format!("{}…", &trimmed[..end])
}

#[cfg(test)]
mod hint_tests {
    use super::{
        classify_spider_error, netdisk_login_hint, netdisk_login_hint_for,
    };
    use crate::resolver::ResolveError;

    #[test]
    fn login_hint_by_class_name() {
        assert!(netdisk_login_hint("WexquarkGuard").contains("夸克"));
        assert!(netdisk_login_hint("WexzhizhenGuard").contains("网盘"));
        assert!(netdisk_login_hint("WexAliSomethingGuard").contains("阿里"));
    }

    #[test]
    fn login_hint_prefers_flag_over_class_name() {
        // flag 是线路名(网盘名), 比类名更准确
        assert!(netdisk_login_hint_for("WexmuouggGuard", "夸克网盘").contains("夸克"));
        assert!(netdisk_login_hint_for("WexmuouggGuard", "UC网盘").contains("UC"));
        assert!(netdisk_login_hint_for("WexWoBaiduPanGuard", "百度原画#01").contains("百度"));
        assert!(netdisk_login_hint_for("WexWoXunLeiPanGuard", "迅雷原画").contains("迅雷"));
        // flag 无信息时退回按类名推断
        assert!(netdisk_login_hint_for("WexquarkGuard", "").contains("夸克"));
    }

    #[test]
    fn bridge_timeout_classifies_as_timeout_not_auth() {
        // 会话层 504 串形如 "bridge error: bridge_timeout" — 必须归 Timeout,
        // 不能谎报"需要登录夸克网盘" (回归: 2026-09-12 wex 设备超时被误分类)
        let e = classify_spider_error(
            "WexmuouggGuard",
            "夸克原画",
            "bridge error: bridge_timeout",
        );
        assert!(
            matches!(e, ResolveError::Timeout { .. }),
            "timeout 被误分类为: {e:?}"
        );
    }

    #[test]
    fn bridge_disconnect_classifies_as_network() {
        let e = classify_spider_error(
            "WexmuouggGuard",
            "夸克原画",
            "bridge error: bridge_disconnected",
        );
        assert!(matches!(e, ResolveError::NetworkError { .. }), "got: {e:?}");
    }

    #[test]
    fn real_spider_exception_still_classifies_as_auth() {
        let e = classify_spider_error(
            "WexmuouggGuard",
            "夸克原画",
            "bridge error: java.lang.reflect.InvocationTargetException",
        );
        assert!(
            matches!(e, ResolveError::AuthenticationRequired { .. }),
            "got: {e:?}"
        );
    }

    #[test]
    fn worker_family_classifies_as_network_not_auth() {
        for err in [
            "bridge error: worker_killed",
            "bridge error: worker_restarting",
            "bridge error: worker_disabled",
            "bridge error: worker_unavailable",
            "bridge error: source_circuit_open",
            "bridge error: bridge_circuit_open",
        ] {
            let r = classify_spider_error("WexmuouggGuard", "夸克原画", err);
            assert!(
                matches!(r, ResolveError::NetworkError { ref message, .. } if !message.contains("需要登录")),
                "{err} -> {r:?}"
            );
        }
    }
}
