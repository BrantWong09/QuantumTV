//! CloudDriveProvider trait (方案 §3) 与播放解析执行器抽象 (§34-§36)
//!
//! 分层约束: 认证 ≠ 分享解析 ≠ 播放解析 ≠ 播放代理 (§49)。
//! share→file→dlink 的领域解析在 Android wex spider 内 (ADR 0005 D2),
//! Provider 通过 [`CloudPlayFetcher`] (生产实现 = bridge /playerContent) 取回
//! 原始直链后标准化为 [`PlayResource`]。

use async_trait::async_trait;
use std::sync::Arc;

use super::types::{
    CloudDriveCapabilities, CloudDriveError, CloudDriveType, PlayResource, ProviderCredential,
    QrLoginSession, RawLoginOutcome, ShareResource,
};

/// 播放解析执行器: Provider 经它触达真实解析链 (桌面不重复实现网盘 API, §45)
#[async_trait]
pub trait CloudPlayFetcher: Send + Sync {
    async fn player_content(
        &self,
        class: &str,
        flag: &str,
        episode_id: &str,
    ) -> Result<crate::resolver::RawPlayResult, String>;
}

/// 生产实现: 每次调用解析当前生效桥接地址 (隧道/远程可切换, 不能启动时固化)
pub struct BridgeCloudPlayFetcher;

#[async_trait]
impl CloudPlayFetcher for BridgeCloudPlayFetcher {
    async fn player_content(
        &self,
        class: &str,
        flag: &str,
        episode_id: &str,
    ) -> Result<crate::resolver::RawPlayResult, String> {
        let bridge_url = crate::bridge::effective_url().ok_or("桥接未就绪")?;
        let fetcher = crate::spider::BridgeSpiderPlayFetcher { bridge_url };
        use crate::resolver::SpiderPlayFetcher;
        fetcher
            .player_content(class, flag, episode_id)
            .await
            .map_err(|e| e.to_string())
    }
}

/// 网盘 Provider (方案 §3): 每网盘独立实现, 严禁复制改域名的假实现 (§45)
#[async_trait]
pub trait CloudDriveProvider: Send + Sync {
    fn id(&self) -> CloudDriveType;

    fn capabilities(&self) -> CloudDriveCapabilities;

    /// 取扫码二维码
    async fn start_qr_login(&self) -> Result<QrLoginSession, CloudDriveError>;

    /// 轮询扫码状态; Confirmed 携带凭证本体 (尚未保存/验证)
    async fn poll_qr_login(
        &self,
        session: &QrLoginSession,
    ) -> Result<RawLoginOutcome, CloudDriveError>;

    /// §6/§13: 用凭证调账号信息接口, 必须校验业务码 (HTTP 200 ≠ 登录成功);
    /// 成功返回账号显示名
    async fn verify_credential(
        &self,
        credential: &ProviderCredential,
    ) -> Result<String, CloudDriveError>;

    /// §18: 分享 URL → ShareResource (纯解析, 不触网)
    fn parse_share(&self, url: &str) -> Result<ShareResource, CloudDriveError>;

    /// 播放解析: 原始集数 id → PlayResource (带请求头, §21)
    async fn resolve_play_url(
        &self,
        class: &str,
        flag: &str,
        episode_id: &str,
    ) -> Result<PlayResource, CloudDriveError>;

    /// "测试连接"可用的示例分享链接 (仅用于 parse_share 自检)
    fn sample_share_url(&self) -> &'static str;
}

/// 三个 Provider 共用的播放解析: spider 原始结果 → 解包 kaiser → PlayResource
pub(crate) async fn resolve_via_spider(
    fetcher: &Arc<dyn CloudPlayFetcher>,
    provider: CloudDriveType,
    class: &str,
    flag: &str,
    episode_id: &str,
) -> Result<PlayResource, CloudDriveError> {
    log::info!(
        "[CloudPlayback] provider={} file_id={} event=resolve_start",
        provider.as_str(),
        crate::spider::trunc(episode_id, 40)
    );
    let raw = fetcher
        .player_content(class, flag, episode_id)
        .await
        .map_err(|e| CloudDriveError::PlaybackResolveFailed(crate::spider::trunc(&e, 160)))?;
    if raw.url.trim().is_empty() {
        // wex 口径: 空 url 基本等价于该网盘未登录/登录态失效
        return Err(CloudDriveError::NotLoggedIn);
    }
    // kaiser 本地代理包装解包: 桌面无法访问模拟器本机端口
    let inner = crate::spider::unwrap_local_proxy_url(&raw.url).unwrap_or(raw.url);
    let resource = crate::media::from_spider_play_result(
        format!("{}+{}", provider.as_str(), crate::spider::trunc(episode_id, 40)),
        inner,
        &raw.header,
    );
    let play = PlayResource::from_media_resource(&resource);
    log::info!(
        "[CloudPlayback] provider={} event=resolve_success expires_at={:?} url_host={}",
        provider.as_str(),
        play.expires_at_ms,
        url_host(&play.url),
    );
    Ok(play)
}

/// 日志脱敏 (§39): 只报 host, 不报完整 URL (query 常含签名)
pub(crate) fn url_host(url: &str) -> String {
    url.strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .and_then(|rest| rest.split('/').next())
        .unwrap_or("?")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_host_never_leaks_query() {
        assert_eq!(
            url_host("https://d.pcs.baidu.com/file/abc?sign=SECRET"),
            "d.pcs.baidu.com"
        );
        assert_eq!(url_host("not-a-url"), "?");
    }
}
