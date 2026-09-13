//! UcProvider (方案 Phase 3): uop CAS 扫码 (client_id=381, api.open.uc.cn) +
//! clouddrive user 业务码验证 (§13: 不能只看 HTTP 200) + spider 播放解析标准化。

use async_trait::async_trait;
use std::sync::Arc;

use super::cas;
use super::provider::{resolve_via_spider, CloudDriveProvider, CloudPlayFetcher};
use super::types::{
    CloudDriveCapabilities, CloudDriveError, CloudDriveType, PlayResource, ProviderCredential,
    QrLoginSession, RawLoginOutcome, ShareResource,
};
use crate::qrcodelogin;

const API_BASE: &str = "https://pc-api.uc.cn";
const REFERER: &str = "https://drive.uc.cn/";

pub struct UcProvider {
    fetcher: Arc<dyn CloudPlayFetcher>,
}

impl UcProvider {
    pub fn new(fetcher: Arc<dyn CloudPlayFetcher>) -> Self {
        Self { fetcher }
    }
}

#[async_trait]
impl CloudDriveProvider for UcProvider {
    fn id(&self) -> CloudDriveType {
        CloudDriveType::Uc
    }

    fn capabilities(&self) -> CloudDriveCapabilities {
        CloudDriveCapabilities::default()
    }

    async fn start_qr_login(&self) -> Result<QrLoginSession, CloudDriveError> {
        let session = qrcodelogin::start("uc")
            .await
            .map_err(CloudDriveError::QrLoginFailed)?;
        QrLoginSession::from_legacy(session)
    }

    async fn poll_qr_login(
        &self,
        session: &QrLoginSession,
    ) -> Result<RawLoginOutcome, CloudDriveError> {
        match cas::poll(CloudDriveType::Uc, &session.to_legacy()).await? {
            cas::CasOutcome::Waiting => Ok(RawLoginOutcome::Waiting),
            cas::CasOutcome::Scanned => Ok(RawLoginOutcome::Scanned),
            cas::CasOutcome::Expired => Ok(RawLoginOutcome::Expired),
            cas::CasOutcome::Confirmed(cred) => Ok(RawLoginOutcome::Confirmed(cred)),
        }
    }

    async fn verify_credential(
        &self,
        credential: &ProviderCredential,
    ) -> Result<String, CloudDriveError> {
        let cookie = credential
            .cookie()
            .filter(|c| !c.is_empty())
            .ok_or(CloudDriveError::NotLoggedIn)?;
        cas::verify_clouddrive_cookie(CloudDriveType::Uc, cookie, API_BASE, REFERER).await
    }

    fn parse_share(&self, url: &str) -> Result<ShareResource, CloudDriveError> {
        parse_uc_share(url)
    }

    async fn resolve_play_url(
        &self,
        class: &str,
        flag: &str,
        episode_id: &str,
    ) -> Result<PlayResource, CloudDriveError> {
        resolve_via_spider(&self.fetcher, CloudDriveType::Uc, class, flag, episode_id).await
    }

    fn sample_share_url(&self) -> &'static str {
        "https://drive.uc.cn/s/0a1b2c3d4e5f"
    }
}

/// drive.uc.cn/s/<id> (+可选 提取码 参数)
pub fn parse_uc_share(url: &str) -> Result<ShareResource, CloudDriveError> {
    let re = regex::Regex::new(r"drive\.uc\.cn/s/([0-9a-zA-Z]+)").unwrap();
    let caps = re.captures(url).ok_or(CloudDriveError::ShareNotFound)?;
    let passcode = regex::Regex::new(r"[?&]pwd=([0-9A-Za-z]+)")
        .unwrap()
        .captures(url)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string());
    Ok(ShareResource {
        provider: CloudDriveType::Uc,
        share_url: url.trim().to_string(),
        share_id: caps.get(1).unwrap().as_str().to_string(),
        passcode,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_uc_share_url() {
        let s = parse_uc_share("https://drive.uc.cn/s/0a1b2c3d4e5f?public=1").unwrap();
        assert_eq!(s.provider, CloudDriveType::Uc);
        assert_eq!(s.share_id, "0a1b2c3d4e5f");
        assert!(parse_uc_share("https://pan.quark.cn/s/x").is_err());
    }
}
