//! QuarkProvider (方案 Phase 2): uop CAS 扫码 + clouddrive user 业务码验证 +
//! spider 播放解析标准化为 PlayResource。

use async_trait::async_trait;
use std::sync::Arc;

use super::cas;
use super::provider::{resolve_via_spider, CloudDriveProvider, CloudPlayFetcher};
use super::types::{
    CloudDriveCapabilities, CloudDriveError, CloudDriveType, PlayResource, ProviderCredential,
    QrLoginSession, RawLoginOutcome, ShareResource,
};
use crate::qrcodelogin;

const API_BASE: &str = "https://drive-pc.quark.cn";
const REFERER: &str = "https://pan.quark.cn/";

pub struct QuarkProvider {
    fetcher: Arc<dyn CloudPlayFetcher>,
}

impl QuarkProvider {
    pub fn new(fetcher: Arc<dyn CloudPlayFetcher>) -> Self {
        Self { fetcher }
    }
}

#[async_trait]
impl CloudDriveProvider for QuarkProvider {
    fn id(&self) -> CloudDriveType {
        CloudDriveType::Quark
    }

    fn capabilities(&self) -> CloudDriveCapabilities {
        // 夸克播放链路依赖 __puus 轮换 (spider 侧), 标记 requires_refresh (§17)
        CloudDriveCapabilities { requires_refresh: true, ..Default::default() }
    }

    async fn start_qr_login(&self) -> Result<QrLoginSession, CloudDriveError> {
        let session = qrcodelogin::start("quark")
            .await
            .map_err(CloudDriveError::QrLoginFailed)?;
        QrLoginSession::from_legacy(session)
    }

    async fn poll_qr_login(
        &self,
        session: &QrLoginSession,
    ) -> Result<RawLoginOutcome, CloudDriveError> {
        match cas::poll(CloudDriveType::Quark, &session.to_legacy()).await? {
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
        cas::verify_clouddrive_cookie(CloudDriveType::Quark, cookie, API_BASE, REFERER).await
    }

    fn parse_share(&self, url: &str) -> Result<ShareResource, CloudDriveError> {
        parse_quark_share(url)
    }

    async fn resolve_play_url(
        &self,
        class: &str,
        flag: &str,
        episode_id: &str,
    ) -> Result<PlayResource, CloudDriveError> {
        resolve_via_spider(&self.fetcher, CloudDriveType::Quark, class, flag, episode_id).await
    }

    fn sample_share_url(&self) -> &'static str {
        "https://pan.quark.cn/s/0a1b2c3d4e5f"
    }
}

/// pan.quark.cn/s/<id>(#/list/... 可选)
pub fn parse_quark_share(url: &str) -> Result<ShareResource, CloudDriveError> {
    let re = regex::Regex::new(r"pan\.quark\.cn/s/([0-9a-zA-Z]+)").unwrap();
    let caps = re.captures(url).ok_or(CloudDriveError::ShareNotFound)?;
    Ok(ShareResource {
        provider: CloudDriveType::Quark,
        share_url: url.trim().to_string(),
        share_id: caps.get(1).unwrap().as_str().to_string(),
        passcode: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quark_share_url() {
        let s = parse_quark_share("https://pan.quark.cn/s/0a1b2c3d4e5f#/list/share").unwrap();
        assert_eq!(s.provider, CloudDriveType::Quark);
        assert_eq!(s.share_id, "0a1b2c3d4e5f");
        assert!(parse_quark_share("https://pan.baidu.com/s/x").is_err());
    }
}
