//! 网盘扫码登录 (桌面端驱动, 原生扫码 API)
//!
//! 夸克/UC: uop CAS (token → 前端渲染 su 链接二维码 → 轮询 → service_ticket 换 cookie)
//! 百度: passport getqrcode (官方 PNG 二维码 → unicast 轮询 → v 换 BDUSS)
//! Cookie 由调用方经桥接 /setCookie 写入 APK CookieManager (wex spider 原生读取通道)。
//! 参考: gaozhangmin/boxplayer, nuu987/tvbox-auxiliary, xiaoya-alist, DecryptLogin。

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// 本期支持扫码的网盘
pub const SUPPORTED: &[&str] = &["quark", "uc", "baidu"];

pub(crate) mod baidu;

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";
const QUARK_CLIENT_UA: &str =
    "quark-cloud-drive/2.5.20 Chrome/100.0.4896.160 Electron/18.3.5.4 Safari/537.36 Channel/pckk_other_ch";

/// 换 cookie 阶段可能 302, 必须禁跟随才能读到首响 Set-Cookie
pub(super) fn http() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(15))
        .no_proxy()
        .build()
        .map_err(|e| format!("http client: {e}"))
}

/// 取二维码 (扫码入口)
pub async fn start(drive: &str) -> Result<QrSession, String> {
    match drive {
        "quark" => start_cas("quark", "532").await,
        "uc" => start_cas("uc", "381").await,
        "baidu" => baidu::start().await,
        other => Err(format!("网盘 {other} 暂不支持扫码登录 (支持: quark/uc/baidu)")),
    }
}

/// 轮询扫码状态 (Confirmed 时已完成 cookie 交换)
pub async fn poll(session: &QrSession) -> Result<PollOutcome, String> {
    match session.drive.as_str() {
        "quark" | "uc" => poll_cas(session).await,
        "baidu" => baidu::poll(session).await,
        other => Err(format!("网盘 {other} 暂不支持扫码登录")),
    }
}

/// 二维码负载: Text = 前端 qrcode 包自绘; PngBase64 = 网盘官方 PNG
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum QrKind {
    Text(String),
    PngBase64(String),
}

/// 扫码会话 (前端 poll 时原样回传)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QrSession {
    pub drive: String,
    pub qr: QrKind,
    /// 夸克/UC: cas token; 百度: getqrcode sign
    pub token: String,
    /// 取码响应携带的会话 cookie (k=v), 换 cookie 时并入请求与最终串
    pub cas_cookies: Vec<String>,
}

/// 轮询结果; Confirmed 表示 cookie 交换已完成
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", content = "data", rename_all = "lowercase")]
pub enum PollOutcome {
    Waiting,
    Scanned,
    Confirmed { cookie: String },
    Expired,
}

fn millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// cas 取码响应 → token; 兼容 status=200 (nuu987) 与 2000000 (xiaoya) 两种口径
pub(crate) fn parse_cas_token(json: &serde_json::Value) -> Option<String> {
    let st = json.get("status").and_then(|v| v.as_i64());
    if !matches!(st, Some(200) | Some(2_000_000)) {
        return None;
    }
    json.pointer("/data/members/token")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

pub(crate) enum CasPoll {
    Waiting,
    Scanned,
    Expired,
    /// service_ticket
    Confirmed(String),
}

/// cas 轮询解析: 优先 data.members.status 字符串, 兜底顶层数字 status
pub(crate) fn parse_cas_poll(json: &serde_json::Value) -> CasPoll {
    let members = json.pointer("/data/members");
    if let Some(s) = members.and_then(|m| m.get("status")).and_then(|v| v.as_str()) {
        match s {
            "CONFIRMED" => {
                if let Some(t) = members
                    .and_then(|m| m.get("service_ticket"))
                    .and_then(|v| v.as_str())
                {
                    return CasPoll::Confirmed(t.to_string());
                }
            }
            "SCANED" => return CasPoll::Scanned,
            "EXPIRED" => return CasPoll::Expired,
            _ => {}
        }
    }
    match json.get("status").and_then(|v| v.as_i64()) {
        Some(2_000_000) => {
            match json.pointer("/data/members/service_ticket").and_then(|v| v.as_str()) {
                Some(t) => CasPoll::Confirmed(t.to_string()),
                None => CasPoll::Waiting,
            }
        }
        Some(50004002) | Some(50004003) | Some(50004004) => CasPoll::Expired,
        _ => CasPoll::Waiting,
    }
}

pub(crate) enum BaiduPoll {
    Waiting,
    Scanned,
    Expired,
    /// 一次性换票 token v
    Confirmed(String),
}

/// 百度 unicast 解析: errno 1=等待 0=有消息(-1/-2 过期); channel_v 是内嵌 JSON 字符串需二次解析
pub(crate) fn parse_baidu_poll(json: &serde_json::Value) -> BaiduPoll {
    match json.get("errno").and_then(|v| v.as_i64()) {
        Some(1) => BaiduPoll::Waiting,
        Some(-1) | Some(-2) => BaiduPoll::Expired,
        Some(0) => {
            let cv = json.get("channel_v").and_then(|v| v.as_str()).unwrap_or("");
            let cv: serde_json::Value = serde_json::from_str(cv).unwrap_or(serde_json::Value::Null);
            match cv.get("status").and_then(|v| v.as_i64()) {
                Some(0) => match cv.get("v").and_then(|v| v.as_str()) {
                    Some(v) if !v.is_empty() => BaiduPoll::Confirmed(v.to_string()),
                    _ => BaiduPoll::Waiting,
                },
                Some(1) => BaiduPoll::Scanned,
                _ => BaiduPoll::Waiting,
            }
        }
        _ => BaiduPoll::Waiting,
    }
}

/// "k=v; Path=/; HttpOnly" → "k=v"; 无 '=' 返回 None
pub(crate) fn set_cookie_kv(set_cookie: &str) -> Option<String> {
    let first = set_cookie.split(';').next()?.trim();
    if first.is_empty() || !first.contains('=') {
        return None;
    }
    Some(first.to_string())
}

/// 合并 cookie 对: 同 key 后者覆盖前者, 输出 "k=v; k2=v2"
pub(crate) fn build_cookie(base: &[String], extra: Vec<String>) -> String {
    let mut out: Vec<String> = Vec::new();
    for p in base.iter().chain(extra.iter()) {
        let key = p.split('=').next().unwrap_or("");
        let prefix = format!("{key}=");
        out.retain(|x: &String| !x.starts_with(&prefix));
        out.push(p.clone());
    }
    out.join("; ")
}

async fn start_cas(drive: &str, client_id: &str) -> Result<QrSession, String> {
    let rid = uuid::Uuid::new_v4();
    let host = if drive == "quark" { "uop.quark.cn" } else { "api.open.uc.cn" };
    let url = format!(
        "https://{host}/cas/ajax/getTokenForQrcodeLogin?client_id={client_id}&v=1.2&request_id={rid}"
    );
    let resp = http()?
        .get(&url)
        .header(reqwest::header::USER_AGENT, UA)
        .send()
        .await
        .map_err(|e| format!("取码失败: {e}"))?;
    let cas_cookies: Vec<String> = resp
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(set_cookie_kv)
        .collect();
    let json: serde_json::Value = resp.json().await.map_err(|e| format!("取码解析失败: {e}"))?;
    let token = parse_cas_token(&json).ok_or_else(|| format!("取码失败: {json}"))?;
    Ok(QrSession {
        drive: drive.to_string(),
        qr: QrKind::Text(format!(
            "https://su.quark.cn/4_eMHBJ?token={token}&client_id={client_id}&ssb=weblogin"
        )),
        token,
        cas_cookies,
    })
}

async fn poll_cas(session: &QrSession) -> Result<PollOutcome, String> {
    let client_id = if session.drive == "quark" { "532" } else { "381" };
    let host = if session.drive == "quark" { "uop.quark.cn" } else { "api.open.uc.cn" };
    let rid = uuid::Uuid::new_v4();
    let url = format!(
        "https://{host}/cas/ajax/getServiceTicketByQrcodeToken?client_id={client_id}&v=1.2&token={}&request_id={rid}",
        session.token
    );
    let json: serde_json::Value = http()?
        .get(&url)
        .header(reqwest::header::USER_AGENT, UA)
        .send()
        .await
        .map_err(|e| format!("轮询失败: {e}"))?
        .json()
        .await
        .map_err(|e| format!("轮询解析失败: {e}"))?;
    match parse_cas_poll(&json) {
        CasPoll::Waiting => Ok(PollOutcome::Waiting),
        CasPoll::Scanned => Ok(PollOutcome::Scanned),
        CasPoll::Expired => Ok(PollOutcome::Expired),
        CasPoll::Confirmed(ticket) => {
            let cookie = exchange_cas_cookie(&session.drive, &session.cas_cookies, &ticket).await?;
            Ok(PollOutcome::Confirmed { cookie })
        }
    }
}

/// service_ticket → cookie; 夸克再补 __puus (spider 播放链路轮换所需)
async fn exchange_cas_cookie(
    drive: &str,
    cas_cookies: &[String],
    ticket: &str,
) -> Result<String, String> {
    let client = http()?;
    let info_host = if drive == "quark" { "pan.quark.cn" } else { "drive.uc.cn" };
    let url = format!("https://{info_host}/account/info?st={ticket}&lw=scan");
    let resp = client
        .get(&url)
        .header(reqwest::header::USER_AGENT, UA)
        .header(reqwest::header::COOKIE, &cas_cookies.join("; "))
        .send()
        .await
        .map_err(|e| format!("换 cookie 失败: {e}"))?;
    let mut pairs: Vec<String> = resp
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(set_cookie_kv)
        .collect();

    if drive == "quark" {
        let merged = build_cookie(cas_cookies, pairs.clone());
        let resp2 = client
            .get("https://drive-pc.quark.cn/1/clouddrive/config?pr=ucpro&fr=pc&uc_param_str=")
            .header(reqwest::header::USER_AGENT, QUARK_CLIENT_UA)
            .header(reqwest::header::REFERER, "https://pan.quark.cn/")
            .header(reqwest::header::COOKIE, &merged)
            .send()
            .await
            .map_err(|e| format!("补 __puus 失败: {e}"))?;
        let puus: Vec<String> = resp2
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .filter_map(set_cookie_kv)
            .filter(|p| p.starts_with("__puus="))
            .collect();
        pairs.extend(puus);
    }

    let cookie = build_cookie(cas_cookies, pairs);
    if cookie.is_empty() {
        return Err("登录确认成功但未取到 cookie (Set-Cookie 为空), 请重试".to_string());
    }
    Ok(cookie)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cas_token_accepts_both_status_shapes() {
        assert_eq!(
            parse_cas_token(&json!({"status":200,"data":{"members":{"token":"t1"}}})).as_deref(),
            Some("t1")
        );
        assert_eq!(
            parse_cas_token(&json!({"status":2000000,"data":{"members":{"token":"t2"}}})).as_deref(),
            Some("t2")
        );
        assert_eq!(parse_cas_token(&json!({"status":500,"message":"err"})), None);
        assert_eq!(parse_cas_token(&json!({"status":200,"data":{"members":{}}})), None);
    }

    #[test]
    fn cas_poll_member_status_string_wins() {
        let j = json!({"status":2000000,"data":{"members":{"status":"SCANED"}}});
        assert!(matches!(parse_cas_poll(&j), CasPoll::Scanned));
        let j = json!({"status":2000000,"data":{"members":{"status":"EXPIRED"}}});
        assert!(matches!(parse_cas_poll(&j), CasPoll::Expired));
        let j = json!({"status":2000000,"data":{"members":{"status":"CONFIRMED","service_ticket":"st1"}}});
        assert!(matches!(parse_cas_poll(&j), CasPoll::Confirmed(ref t) if t == "st1"));
    }

    #[test]
    fn cas_poll_top_level_fallback() {
        // xiaoya 口径: 顶层 2000000 + service_ticket, 无 members.status 字符串
        let j = json!({"status":2000000,"data":{"members":{"service_ticket":"st2"}}});
        assert!(matches!(parse_cas_poll(&j), CasPoll::Confirmed(ref t) if t == "st2"));
        // 等待扫码
        let j = json!({"status":50004001,"message":"query result is empty"});
        assert!(matches!(parse_cas_poll(&j), CasPoll::Waiting));
        // 过期三连
        for code in [50004002i64, 50004003, 50004004] {
            let j = json!({"status":code});
            assert!(matches!(parse_cas_poll(&j), CasPoll::Expired), "code={code}");
        }
    }

    #[test]
    fn baidu_poll_status_mapping() {
        assert!(matches!(parse_baidu_poll(&json!({"errno":1})), BaiduPoll::Waiting));
        assert!(matches!(parse_baidu_poll(&json!({"errno":-1})), BaiduPoll::Expired));
        assert!(matches!(parse_baidu_poll(&json!({"errno":-2})), BaiduPoll::Expired));
        let j = json!({"errno":0,"channel_v":"{\"status\":0,\"v\":\"tok123\"}"});
        assert!(matches!(parse_baidu_poll(&j), BaiduPoll::Confirmed(ref v) if v == "tok123"));
        let j = json!({"errno":0,"channel_v":"{\"status\":1}"});
        assert!(matches!(parse_baidu_poll(&j), BaiduPoll::Scanned));
        // 已扫但 v 为空 → 继续等
        let j = json!({"errno":0,"channel_v":"{\"status\":0,\"v\":\"\"}"});
        assert!(matches!(parse_baidu_poll(&j), BaiduPoll::Waiting));
        // channel_v 非 JSON (中间态) → 等待
        let j = json!({"errno":0,"channel_v":"garbage"});
        assert!(matches!(parse_baidu_poll(&j), BaiduPoll::Waiting));
    }

    #[test]
    fn set_cookie_kv_strips_attributes() {
        assert_eq!(set_cookie_kv("BDUSS=abc123; Path=/; HttpOnly").as_deref(), Some("BDUSS=abc123"));
        assert_eq!(set_cookie_kv("__puus=xyz").as_deref(), Some("__puus=xyz"));
        assert_eq!(set_cookie_kv("invalid"), None);
        assert_eq!(set_cookie_kv(""), None);
    }

    #[test]
    fn build_cookie_later_wins_per_key() {
        let base = vec!["__pus=old".to_string(), "__kp=kp1".to_string()];
        let extra = vec![
            "__pus=new".to_string(),
            "__puus=pu1".to_string(),
            "__puus=pu2".to_string(),
        ];
        assert_eq!(build_cookie(&base, extra), "__kp=kp1; __pus=new; __puus=pu2");
        assert_eq!(build_cookie(&[], vec![]), "");
    }
}
