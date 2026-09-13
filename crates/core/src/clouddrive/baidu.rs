//! BaiduProvider (方案 §4 Phase 1): 复用既有 qrcodelogin 扫码实现 (保证百度不回归),
//! 新增 §6 verify_credential (xpan uinfo 业务码校验) 与 PlayResource 标准化。

use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

use super::provider::{resolve_via_spider, CloudDriveProvider, CloudPlayFetcher};
use super::types::{
    CloudDriveCapabilities, CloudDriveError, CloudDriveType, PlayResource, ProviderCredential,
    QrLoginSession, RawLoginOutcome, ShareResource,
};
use crate::qrcodelogin;

pub struct BaiduProvider {
    fetcher: Arc<dyn CloudPlayFetcher>,
}

impl BaiduProvider {
    pub fn new(fetcher: Arc<dyn CloudPlayFetcher>) -> Self {
        Self { fetcher }
    }
}

#[async_trait]
impl CloudDriveProvider for BaiduProvider {
    fn id(&self) -> CloudDriveType {
        CloudDriveType::Baidu
    }

    fn capabilities(&self) -> CloudDriveCapabilities {
        CloudDriveCapabilities::default()
    }

    async fn start_qr_login(&self) -> Result<QrLoginSession, CloudDriveError> {
        let session = qrcodelogin::start("baidu")
            .await
            .map_err(|e| CloudDriveError::QrLoginFailed(e))?;
        QrLoginSession::from_legacy(session)
    }

    async fn poll_qr_login(
        &self,
        session: &QrLoginSession,
    ) -> Result<RawLoginOutcome, CloudDriveError> {
        match qrcodelogin::poll(&session.to_legacy()).await {
            Ok(qrcodelogin::PollOutcome::Waiting) => Ok(RawLoginOutcome::Waiting),
            Ok(qrcodelogin::PollOutcome::Scanned) => Ok(RawLoginOutcome::Scanned),
            Ok(qrcodelogin::PollOutcome::Expired) => Ok(RawLoginOutcome::Expired),
            Ok(qrcodelogin::PollOutcome::Confirmed { cookie }) => {
                Ok(RawLoginOutcome::Confirmed(ProviderCredential::from_cookie(
                    CloudDriveType::Baidu,
                    &cookie,
                )))
            }
            Err(e) => {
                if e.contains("过期") || e.contains("expired") {
                    Ok(RawLoginOutcome::Expired)
                } else {
                    Err(CloudDriveError::QrLoginFailed(crate::spider::trunc(&e, 120)))
                }
            }
        }
    }

    /// 百度验证: xpan uinfo 只需 BDUSS, errno==0 即有效 (HTTP 200 不算数, §13)
    async fn verify_credential(
        &self,
        credential: &ProviderCredential,
    ) -> Result<String, CloudDriveError> {
        let cookie = credential
            .cookie()
            .filter(|c| !c.is_empty())
            .ok_or(CloudDriveError::NotLoggedIn)?;
        let resp = qrcodelogin::http()
            .map_err(|e| CloudDriveError::NetworkError(e))?
            .get("https://pan.baidu.com/rest/2.0/xpan/nas?method=uinfo")
            .header(reqwest::header::USER_AGENT, qrcodelogin::BROWSER_UA)
            .header(reqwest::header::COOKIE, cookie)
            .send()
            .await
            .map_err(|e| CloudDriveError::NetworkError(crate::spider::trunc(&e.to_string(), 120)))?;
        let json: Value = resp
            .json()
            .await
            .map_err(|e| CloudDriveError::NetworkError(format!("响应解析失败: {e}")))?;
        match json.get("errno").and_then(|v| v.as_i64()) {
            Some(0) => {
                let account = ["baidu_name", "netdisk_name"]
                    .iter()
                    .find_map(|k| json.get(k).and_then(|v| v.as_str()).map(String::from))
                    .unwrap_or_else(|| "百度账号".to_string());
                Ok(account)
            }
            Some(-6) => Err(CloudDriveError::CredentialInvalid("登录态失效 (errno -6)".into())),
            Some(code) => Err(CloudDriveError::CredentialInvalid(format!("errno {code}"))),
            None => Err(CloudDriveError::CredentialInvalid(
                "响应缺少 errno".into(),
            )),
        }
    }

    fn parse_share(&self, url: &str) -> Result<ShareResource, CloudDriveError> {
        parse_baidu_share(url)
    }

    async fn resolve_play_url(
        &self,
        class: &str,
        flag: &str,
        episode_id: &str,
    ) -> Result<PlayResource, CloudDriveError> {
        resolve_via_spider(&self.fetcher, CloudDriveType::Baidu, class, flag, episode_id).await
    }

    fn sample_share_url(&self) -> &'static str {
        "https://pan.baidu.com/s/1AbCdEfGhIjK?pwd=0000"
    }
}

/// pan.baidu.com/s/<id> (+可选 pwd=提取码)
pub fn parse_baidu_share(url: &str) -> Result<ShareResource, CloudDriveError> {
    let re = regex::Regex::new(r"pan\.baidu\.com/s/([0-9A-Za-z_-]+)").unwrap();
    let caps = re
        .captures(url)
        .ok_or(CloudDriveError::ShareNotFound)?;
    let passcode = regex::Regex::new(r"[?&]pwd=([0-9A-Za-z]+)")
        .unwrap()
        .captures(url)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string());
    Ok(ShareResource {
        provider: CloudDriveType::Baidu,
        share_url: url.trim().to_string(),
        share_id: caps.get(1).unwrap().as_str().to_string(),
        passcode,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_baidu_share_extracts_id_and_pwd() {
        let s = parse_baidu_share("https://pan.baidu.com/s/1AbC_d-9?pwd=ab12").unwrap();
        assert_eq!(s.provider, CloudDriveType::Baidu);
        assert_eq!(s.share_id, "1AbC_d-9");
        assert_eq!(s.passcode.as_deref(), Some("ab12"));
        assert!(parse_baidu_share("https://pan.quark.cn/s/abc").is_err());
        assert!(parse_baidu_share("not a share").is_err());
    }
}
