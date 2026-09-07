//! 百度网盘扫码登录: passport getqrcode → unicast 轮询 → qrbdusslogin 换 BDUSS
//! 参考: riowang88/tvbox-source-aggregator, CharlesPikachu/DecryptLogin, BaiduPCS-Rust

use base64::Engine;
use std::time::Duration;

use super::{parse_baidu_poll, set_cookie_kv, PollOutcome, QrKind, QrSession};

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .no_proxy()
        .build()
        .map_err(|e| format!("http client: {e}"))
}

fn gid() -> String {
    uuid::Uuid::new_v4().simple().to_string().to_uppercase()
}

fn millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

pub(super) async fn start() -> Result<QrSession, String> {
    let url = format!(
        "https://passport.baidu.com/v2/api/getqrcode?lp=pc&qrloginfrom=pc&gid={}&apiver=v3&tt={}&tpl=netdisk",
        gid(),
        millis()
    );
    let json: serde_json::Value = client()?
        .get(&url)
        .header(reqwest::header::USER_AGENT, UA)
        .send()
        .await
        .map_err(|e| format!("取码失败: {e}"))?
        .json()
        .await
        .map_err(|e| format!("取码解析失败: {e}"))?;
    let sign = json
        .get("sign")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("取码失败: {json}"))?
        .to_string();

    // 官方 PNG (内容为 wappass 确认页 URL), 转 base64 给前端 <img>
    let img_url = format!("https://passport.baidu.com/v2/api/qrcode?sign={sign}&lp=pc");
    let png = client()?
        .get(&img_url)
        .header(reqwest::header::USER_AGENT, UA)
        .send()
        .await
        .map_err(|e| format!("二维码图获取失败: {e}"))?
        .bytes()
        .await
        .map_err(|e| format!("二维码图读取失败: {e}"))?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png);

    Ok(QrSession {
        drive: "baidu".to_string(),
        qr: QrKind::PngBase64(b64),
        token: sign,
        cas_cookies: Vec::new(),
    })
}

pub(super) async fn poll(session: &QrSession) -> Result<PollOutcome, String> {
    let tt = millis();
    let url = format!(
        "https://passport.baidu.com/channel/unicast?channel_id={}&tpl=netdisk&gid={}&apiver=v3&tt={tt}&_={tt}",
        session.token,
        gid()
    );
    let json: serde_json::Value = client()?
        .get(&url)
        .header(reqwest::header::USER_AGENT, UA)
        .send()
        .await
        .map_err(|e| format!("轮询失败: {e}"))?
        .json()
        .await
        .map_err(|e| format!("轮询解析失败: {e}"))?;

    match parse_baidu_poll(&json) {
        super::BaiduPoll::Waiting => Ok(PollOutcome::Waiting),
        super::BaiduPoll::Scanned => Ok(PollOutcome::Scanned),
        super::BaiduPoll::Expired => Ok(PollOutcome::Expired),
        super::BaiduPoll::Confirmed(v) => {
            let login_url = format!(
                "https://passport.baidu.com/v3/login/main/qrbdusslogin?v={tt}&bduss={v}&loginVersion=v4&qrcode=1&tpl=netdisk&apiver=v3&tt={tt}"
            );
            let resp = client()?
                .get(&login_url)
                .header(reqwest::header::USER_AGENT, UA)
                .send()
                .await
                .map_err(|e| format!("换 cookie 失败: {e}"))?;
            let cookie: Vec<String> = resp
                .headers()
                .get_all(reqwest::header::SET_COOKIE)
                .iter()
                .filter_map(|h| h.to_str().ok())
                .filter_map(set_cookie_kv)
                .filter(|p| {
                    p.starts_with("BDUSS=") || p.starts_with("STOKEN=") || p.starts_with("PTOKEN=")
                })
                .collect();
            if cookie.is_empty() {
                return Err("登录确认成功但未取到 BDUSS (Set-Cookie 为空), 请重试".to_string());
            }
            Ok(PollOutcome::Confirmed { cookie: cookie.join("; ") })
        }
    }
}
