//! 夸克/UC 共用的 uop CAS 扫码基座 (同一协议族, 参数不同; 非"复制改域名"):
//! 取码 → 前端渲染 su 链接二维码 → 轮询 → service_ticket 换 cookie。
//! 域名/client_id 常量在 qrcodelogin::start/poll (host uop.quark.cn / api.open.uc.cn)。

use serde_json::Value;

use super::types::{CloudDriveError, CloudDriveType, ProviderCredential};
use crate::qrcodelogin::{self, PollOutcome};

/// 轮询结果: 与 CAS 协议状态一一对应 (Scanned ≠ Confirmed, §29)
pub(super) enum CasOutcome {
    Waiting,
    Scanned,
    Expired,
    Confirmed(ProviderCredential),
}

/// 轮询 CAS; Confirmed 时已完成 ticket → cookie 交换 (含夸克 __puus 补取)
pub(super) async fn poll(
    provider: CloudDriveType,
    session: &qrcodelogin::QrSession,
) -> Result<CasOutcome, CloudDriveError> {
    match qrcodelogin::poll(session).await {
        Ok(PollOutcome::Waiting) => Ok(CasOutcome::Waiting),
        Ok(PollOutcome::Scanned) => Ok(CasOutcome::Scanned),
        Ok(PollOutcome::Expired) => Ok(CasOutcome::Expired),
        Ok(PollOutcome::Confirmed { cookie }) => Ok(CasOutcome::Confirmed(
            ProviderCredential::from_cookie(provider, &cookie),
        )),
        Err(e) => Err(CloudDriveError::QrLoginFailed(crate::spider::trunc(&e, 120))),
    }
}

/// clouddrive API 业务码校验 (夸克/UC 同族): code==0 且 data 存在才算登录有效 (§13)。
/// 端点 = /1/clouddrive/member (实测: 未登录 HTTP 401 + code 31001; /1/clouddrive/user
/// 是 404 假端点, 2026-09-13 回归教训)。
pub(super) async fn verify_clouddrive_cookie(
    provider: CloudDriveType,
    cookie: &str,
    api_base: &'static str,
    referer: &'static str,
) -> Result<String, CloudDriveError> {
    let resp = qrcodelogin::http()
        .map_err(|e| CloudDriveError::NetworkError(e))?
        .get(format!("{api_base}/1/clouddrive/member?pr={}&fr=pc", pr_value(provider)))
        .header(reqwest::header::USER_AGENT, qrcodelogin::QUARK_CLIENT_UA)
        .header(reqwest::header::REFERER, referer)
        .header(reqwest::header::COOKIE, cookie)
        .send()
        .await
        .map_err(|e| CloudDriveError::NetworkError(crate::spider::trunc(&e.to_string(), 120)))?;
    let json: Value = resp
        .json()
        .await
        .map_err(|e| CloudDriveError::NetworkError(format!("响应解析失败: {e}")))?;
    let code = json.get("code").and_then(|v| v.as_i64());
    match code {
        Some(0) => {
            let data_empty = json.get("data").map(|d| d.is_null()).unwrap_or(true);
            if data_empty {
                return Err(CloudDriveError::CredentialInvalid("业务码 0 但缺 data".into()));
            }
            let account = ["nickname", "name", "user_id"]
                .iter()
                .find_map(|k| {
                    json.pointer(&format!("/data/{k}"))
                        .and_then(|v| {
                            v.as_str().map(String::from).or_else(|| {
                                v.as_i64().map(|n| n.to_string())
                            })
                        })
                })
                .unwrap_or_else(|| format!("{}账号", provider.display_name()));
            Ok(account)
        }
        Some(31001) | Some(31002) => {
            Err(CloudDriveError::CredentialInvalid("业务码 31001/31002 (未登录)".into()))
        }
        Some(c) => {
            let msg = json.get("message").and_then(|v| v.as_str()).unwrap_or("");
            Err(CloudDriveError::CredentialInvalid(format!(
                "业务码 {c}{}",
                if msg.is_empty() { String::new() } else { format!(": {}", crate::spider::trunc(msg, 60)) }
            )))
        }
        None => Err(CloudDriveError::CredentialInvalid("响应缺少业务码 code".into())),
    }
}

fn pr_value(provider: CloudDriveType) -> &'static str {
    match provider {
        CloudDriveType::Quark => "ucpro",
        CloudDriveType::Uc => "UCBrowser",
        CloudDriveType::Baidu => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr_values() {
        assert_eq!(pr_value(CloudDriveType::Quark), "ucpro");
        assert_eq!(pr_value(CloudDriveType::Uc), "UCBrowser");
    }
}
