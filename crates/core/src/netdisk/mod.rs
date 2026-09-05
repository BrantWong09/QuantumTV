//! 网盘账号: cookie 的扫码登录 / 常驻续期 / 有效性探测
//!
//! - 夸克: uop.quark.cn CAS 扫码链 (getTokenForQrcodeLogin → 轮询 → account/info?st= 换 Set-Cookie)
//!   续期: 剥 __puus 请求 drive-pc /1/clouddrive/config, 服务端回发新 __puus (alist 同款)
//! - UC: 与夸克同协议 (alist QuarkOrUC 同一驱动), 续期引擎一致
//! - 其他网盘: 粘贴的长期字段 (BDUSS 等), 不参与自动续期

use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const DRIVES: &[&str] = &["quark", "uc", "baidu", "ali", "tianyi", "115", "yidong"];

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct CloudAccount {
    pub drive: String,
    pub cookie: String,
    /// 最近一次续期/登录的 unix 秒
    pub updated_at: u64,
    /// 账号昵称(扫码时可获得, 粘贴为空)
    #[serde(default)]
    pub nickname: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ScanSession {
    pub drive: String,
    pub token: String,
    pub qr_content: String,
}

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .no_proxy()
        .build()
        .map_err(|e| e.to_string())
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

async fn http_json(url: &str, method: reqwest::Method, headers: &[(&str, &str)], cookie: Option<&str>) -> Result<(u16, serde_json::Value, Vec<String>), String> {
    let client = http_client()?;
    let mut b = client.request(method, url);
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    if let Some(c) = cookie {
        b = b.header("Cookie", c);
    }
    let resp = b.send().await.map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let set_cookies: Vec<String> = resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok().map(|s| s.to_string()))
        .collect();
    let body = resp.text().await.map_err(|e| e.to_string())?;
    let json = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
    Ok((status, json, set_cookies))
}

const CAS_HEADERS: &[(&str, &str)] = &[
    ("User-Agent", "Mozilla/5.0 (Linux; Android 11; quark/7.0.0) AppleWebKit/537.36"),
    ("Referer", "https://uop.quark.cn/"),
];

/// 夸克扫码: 第一步, 取 token + 二维码内容
pub async fn quark_scan_start() -> Result<ScanSession, String> {
    let (status, json, _) = http_json(
        "https://uop.quark.cn/cas/ajax/getTokenForQrcodeLogin?client_id=532",
        reqwest::Method::POST, CAS_HEADERS, None).await?;
    if status != 200 {
        return Err(format!("scan start HTTP {status}"));
    }
    let token = json["data"]["members"]["token"]
        .as_str()
        .ok_or_else(|| format!("scan start 无 token: {json}"))?
        .to_string();
    Ok(ScanSession {
        drive: "quark".into(),
        // 固定短码 4_eMHBJ = 夸克官方登录入口(xiaoya 等项目统一硬编码), token 走 query 参数
        qr_content: format!(
            "https://su.quark.cn/4_eMHBJ?token={}&client_id=532&ssb=weblogin",
            urlencoding::encode(&token)
        ),
        token,
    })
}

/// 夸克扫码: 第二步, 轮询确认状态; 确认后立即换 cookie
/// 返回 (state, Option<account>): state ∈ waiting | confirmed
pub async fn quark_scan_poll(token: &str) -> Result<(String, Option<CloudAccount>), String> {
    let url = format!(
        "https://uop.quark.cn/cas/ajax/getServiceTicketByQrcodeToken?client_id=532&token={token}"
    );
    let (status, json, _) = http_json(&url, reqwest::Method::GET, CAS_HEADERS, None).await?;
    if status != 200 {
        return Err(format!("scan poll HTTP {status}"));
    }
    // 50004001 = 等待扫码
    let st = json["data"]["st"].as_str().map(|s| s.to_string());
    match st {
        None => Ok(("waiting".into(), None)),
        Some(st) => {
            let account = quark_exchange_ticket(&st).await?;
            Ok(("confirmed".into(), Some(account)))
        }
    }
}

/// 夸克扫码: 第三步, st → account/info → Set-Cookie 全套
async fn quark_exchange_ticket(st: &str) -> Result<CloudAccount, String> {
    let url = format!("https://pan.quark.cn/account/info?st={st}&lw=scan");
    let (status, _, set_cookies) = http_json(&url, reqwest::Method::GET, &[
        ("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/126"),
        ("Referer", "https://pan.quark.cn/"),
    ], None).await?;
    if status != 200 {
        return Err(format!("ticket exchange HTTP {status}"));
    }
    let cookie = merge_cookies("", &set_cookies);
    if !cookie.contains("__puus") {
        return Err("换 cookie 失败: 响应缺 __puus".into());
    }
    Ok(CloudAccount {
        drive: "quark".into(),
        cookie,
        updated_at: now_secs(),
        nickname: String::new(),
    })
}

/// 把 Set-Cookie 列表合并进现有 cookie 串 (按 cookie 名覆盖)
pub fn merge_cookies(existing: &str, set_cookies: &[String]) -> String {
    let mut map: Vec<(String, String)> = existing
        .split(';')
        .filter_map(|p| {
            let p = p.trim();
            p.split_once('=').map(|(k, v)| (k.to_string(), v.to_string()))
        })
        .collect();
    for sc in set_cookies {
        let pair = sc.split(';').next().unwrap_or("");
        if let Some((k, v)) = pair.trim().split_once('=') {
            let k = k.trim();
            if k.is_empty() {
                continue;
            }
            if let Some(e) = map.iter_mut().find(|(ek, _)| ek == k) {
                e.1 = v.to_string();
            } else {
                map.push((k.to_string(), v.to_string()));
            }
        }
    }
    map.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// 续期 (夸克/UC): 剥 __puus 请求 /1/clouddrive/config, 服务端回发新 __puus
/// 返回续期后的完整 cookie; 无 __puus 下发说明身份字段已失效
pub async fn refresh_cookie(drive: &str, cookie: &str) -> Result<String, String> {
    let host = match drive {
        "quark" => "https://drive-pc.quark.cn",
        "uc" => "https://pc-api.uc.cn",
        _ => return Err(format!("{drive} 不支持自动续期")),
    };
    let core: Vec<&str> = cookie
        .split(';')
        .filter(|p| !p.trim().starts_with("__puus="))
        .collect();
    let core_cookie = core.join("; ");
    let (status, _, set_cookies) = http_json(
        &format!("{host}/1/clouddrive/config?fetch_share=true"),
        reqwest::Method::GET,
        &[
            ("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/126"),
            ("Referer", if drive == "quark" { "https://pan.quark.cn/" } else { "https://drive.uc.cn/" }),
        ],
        Some(&core_cookie),
    ).await?;
    // 服务端在缺 __puus 时回发新 __puus; 200/401 都可能带 Set-Cookie
    let merged = merge_cookies(&core_cookie, &set_cookies);
    if !merged.contains("__puus") {
        return Err(format!("{drive} 续期失败: HTTP {status} 无 __puus 下发 (身份字段可能已失效)"));
    }
    Ok(merged)
}

/// 有效性探测: member 接口 200 且 code==0 即有效
pub async fn probe_valid(drive: &str, cookie: &str) -> Result<bool, String> {
    let host = match drive {
        "quark" => "https://drive-pc.quark.cn",
        "uc" => "https://pc-api.uc.cn",
        _ => return Err(format!("{drive} 暂不支持在线探测")),
    };
    let (status, json, _) = http_json(
        &format!("{host}/1/clouddrive/member?fr=pc"),
        reqwest::Method::GET,
        &[
            ("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/126"),
            ("Referer", if drive == "quark" { "https://pan.quark.cn/" } else { "https://drive.uc.cn/" }),
        ],
        Some(cookie),
    ).await?;
    Ok(status == 200 && json["code"].as_i64() == Some(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_cookies_overrides_and_keeps() {
        let old = "a=1; __puus=old; b=2";
        let sc = vec!["__puus=new; Path=/; HttpOnly".to_string()];
        let merged = merge_cookies(old, &sc);
        assert!(merged.contains("a=1"));
        assert!(merged.contains("b=2"));
        assert!(merged.contains("__puus=new"));
        assert!(!merged.contains("__puus=old"));
    }

    #[tokio::test]
    #[ignore = "需要外网, 手动验证"]
    async fn scan_start_returns_token() {
        let s = quark_scan_start().await.unwrap();
        assert!(s.qr_content.contains("su.quark.cn"));
    }

    #[test]
    fn drives_list_contains_major() {
        for d in ["quark", "uc", "baidu", "ali"] {
            assert!(DRIVES.contains(&d));
        }
    }
}
