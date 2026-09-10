//! Resolver 层 (V2 Phase 2, 见 docs/architecture/02-resolver.md)
//!
//! 唯一目标: 把 Episode/解析输入标准化为 MediaResource。
//! Resolver 不负责播放、UI、mpv、进度。
//!
//! 现有解析链的收纳关系 (Phase 2 保持 command 签名不变, 内部换芯):
//! - 前端 `isDirectPlayableUrl` 扩展名判定 → [`DirectResolver`]
//! - `video.rs::resolve_spider_episode` 的 playerContent + unwrap + wrap 序列
//!   → [`SpiderResolver`] (bridge 交互经 [`SpiderPlayFetcher`] 注入, 解耦桥接)

use crate::media::{from_direct_url, from_spider_play_result, MediaResource, ResourceType};
use std::fmt;
use std::sync::Arc;

/// 解析输入: 描述"要播什么"
#[derive(Debug, Clone, PartialEq)]
pub struct ResolveInput {
    /// 源站点 key (TVBox source key / CMS key); 直链与本地文件可为空串
    pub source: String,
    /// 播放线路 flag (spider playerContent 用; 其他 resolver 为空)
    pub flag: String,
    /// 集数定位: spider 为 raw id, 直链为完整 url, 本地文件为路径
    pub episode_id: String,
    /// spider 站点类名 (去掉 csp_ 前缀后的 Java class); 非 spider 源为 None
    pub spider_class: Option<String>,
    /// 站点类型: 1/2 = CMS 直链, 3 = spider 网盘; None 按 1 处理
    pub site_type: Option<i32>,
}

impl ResolveInput {
    pub fn direct(source: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            flag: String::new(),
            episode_id: url.into(),
            spider_class: None,
            site_type: Some(1),
        }
    }

    pub fn spider(
        source: impl Into<String>,
        flag: impl Into<String>,
        episode_id: impl Into<String>,
        spider_class: impl Into<String>,
    ) -> Self {
        Self {
            source: source.into(),
            flag: flag.into(),
            episode_id: episode_id.into(),
            spider_class: Some(spider_class.into()),
            site_type: Some(3),
        }
    }

    pub fn local_file(path: impl Into<String>) -> Self {
        Self {
            source: "local".into(),
            flag: String::new(),
            episode_id: path.into(),
            spider_class: None,
            site_type: None,
        }
    }

    fn is_spider_site(&self) -> bool {
        self.site_type.unwrap_or(1) == 3 && self.spider_class.is_some()
    }
}

/// 解析错误模型 (02-resolver.md §6): 八类 + 可定位上下文
#[derive(Debug, Clone)]
pub enum ResolveError {
    /// 没有 resolver 能处理该输入
    Unsupported { source: String },
    /// 网络/桥接请求失败
    NetworkError { source: String, message: String },
    /// 响应解析失败
    ParseError { source: String, message: String },
    /// 需要登录网盘等认证
    AuthenticationRequired { source: String, message: String },
    /// 临时直链过期
    ResourceExpired { source: String, message: String },
    /// 资源本身无效 (空 url / 非法格式)
    InvalidResource { source: String, message: String },
    Timeout { source: String, message: String },
    InternalError { source: String, message: String },
}

impl ResolveError {
    fn source(&self) -> &str {
        match self {
            Self::Unsupported { source, .. }
            | Self::NetworkError { source, .. }
            | Self::ParseError { source, .. }
            | Self::AuthenticationRequired { source, .. }
            | Self::ResourceExpired { source, .. }
            | Self::InvalidResource { source, .. }
            | Self::Timeout { source, .. }
            | Self::InternalError { source, .. } => source,
        }
    }

    /// 各变体自身携带的可读信息 (不递归调用 Display)
    fn message(&self) -> String {
        match self {
            Self::Unsupported { .. } => "没有 resolver 能处理该输入".into(),
            Self::NetworkError { message, .. }
            | Self::ParseError { message, .. }
            | Self::AuthenticationRequired { message, .. }
            | Self::ResourceExpired { message, .. }
            | Self::InvalidResource { message, .. }
            | Self::Timeout { message, .. }
            | Self::InternalError { message, .. } => message.clone(),
        }
    }
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::Unsupported { .. } => "不支持",
            Self::NetworkError { .. } => "网络错误",
            Self::ParseError { .. } => "解析错误",
            Self::AuthenticationRequired { .. } => "需要登录",
            Self::ResourceExpired { .. } => "资源已过期",
            Self::InvalidResource { .. } => "资源无效",
            Self::Timeout { .. } => "超时",
            Self::InternalError { .. } => "内部错误",
        };
        write!(
            f,
            "[{}] source={}: {}",
            kind,
            self.source(),
            self.message()
        )
    }
}

impl std::error::Error for ResolveError {}

impl fmt::Display for ResolveInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ResolveInput(source={}, flag={:?}, episode_id={})",
            self.source,
            self.flag,
            crate::spider::trunc(&self.episode_id, 60)
        )
    }
}

/// spider 播放结果: playerContent 的 (url, header) 原始形态 (RawPlayResult 等价物)
pub struct RawPlayResult {
    pub url: String,
    pub header: serde_json::Value,
}

/// spider 播放解析执行器抽象: 隔离 bridge HTTP 细节, 测试可注入桩
#[async_trait::async_trait]
pub trait SpiderPlayFetcher: Send + Sync {
    async fn player_content(
        &self,
        class_name: &str,
        flag: &str,
        episode_id: &str,
    ) -> Result<RawPlayResult, ResolveError>;
}

/// Resolver trait (02-resolver.md §2)
#[async_trait::async_trait]
pub trait Resolver: Send + Sync {
    fn name(&self) -> &str;

    /// 是否能处理该输入 (按顺序首个 can_resolve 的 resolver 执行)
    fn can_resolve(&self, input: &ResolveInput) -> bool;

    async fn resolve(
        &self,
        input: &ResolveInput,
    ) -> Result<MediaResource, ResolveError>;
}

// ---------------------------------------------------------------------------
// 具体实现
// ---------------------------------------------------------------------------

/// 直链解析: 输入已是可播 URL (CMS/TVBox 直链源)。
/// 收纳前端 isDirectPlayableUrl 的扩展名判定。
pub struct DirectResolver;

#[async_trait::async_trait]
impl Resolver for DirectResolver {
    fn name(&self) -> &str {
        "direct"
    }

    fn can_resolve(&self, input: &ResolveInput) -> bool {
        !input.is_spider_site()
            && (input.episode_id.starts_with("http://")
                || input.episode_id.starts_with("https://"))
    }

    async fn resolve(
        &self,
        input: &ResolveInput,
    ) -> Result<MediaResource, ResolveError> {
        if ResourceType::detect(&input.episode_id) == ResourceType::Unknown {
            return Err(ResolveError::InvalidResource {
                source: self.name().into(),
                message: format!("直链类型无法识别: {}", trunc_url(&input.episode_id)),
            });
        }
        // 网盘本地代理地址延续 proxy_required 语义 (detect 命中伪装 .mp4 但仍需 UA)
        let mut resource = from_direct_url(
            resource_id(&input.source, &input.episode_id),
            &input.episode_id,
        );
        if input.episode_id.contains("/netdisk/file.mp4?") {
            resource.proxy_required = true;
        }
        Ok(resource)
    }
}

/// Spider 网盘解析: raw id → playerContent → 真实直链 → MediaResource。
/// 收纳 video.rs::resolve_spider_episode 的 unwrap + wrap 序列 (bridge 细节由
/// fetcher 注入; 本地代理包装在 Phase 3 Gateway 落地前维持现有调用方处理)。
pub struct SpiderResolver {
    fetcher: Arc<dyn SpiderPlayFetcher>,
}

impl SpiderResolver {
    pub fn new(fetcher: Arc<dyn SpiderPlayFetcher>) -> Self {
        Self { fetcher }
    }
}

#[async_trait::async_trait]
impl Resolver for SpiderResolver {
    fn name(&self) -> &str {
        "spider"
    }

    fn can_resolve(&self, input: &ResolveInput) -> bool {
        input.is_spider_site()
    }

    async fn resolve(
        &self,
        input: &ResolveInput,
    ) -> Result<MediaResource, ResolveError> {
        let class_name = input
            .spider_class
            .as_deref()
            .ok_or_else(|| ResolveError::Unsupported {
                source: self.name().into(),
            })?;
        let raw = self
            .fetcher
            .player_content(class_name, &input.flag, &input.episode_id)
            .await?;
        if raw.url.trim().is_empty() {
            return Err(ResolveError::InvalidResource {
                source: self.name().into(),
                message: "playerContent 返回空 url (可能未登录网盘)".into(),
            });
        }
        // kaiser 本地代理包装解包: 桌面无法访问手机本机端口, 解出内层直链
        let inner = crate::spider::unwrap_local_proxy_url(&raw.url).unwrap_or(raw.url);
        Ok(from_spider_play_result(
            resource_id(&input.source, &input.episode_id),
            inner,
            &raw.header,
        ))
    }
}

/// 本地文件解析
pub struct LocalFileResolver;

#[async_trait::async_trait]
impl Resolver for LocalFileResolver {
    fn name(&self) -> &str {
        "local_file"
    }

    fn can_resolve(&self, input: &ResolveInput) -> bool {
        input.episode_id.starts_with("file://")
    }

    async fn resolve(
        &self,
        input: &ResolveInput,
    ) -> Result<MediaResource, ResolveError> {
        Ok(crate::media::from_local_file(
            resource_id(&input.source, &input.episode_id),
            &input.episode_id,
        ))
    }
}

/// 网盘解析 (Phase 2 骨架): 网盘播放当前经 SpiderResolver 完成 (wex spider
/// 的 playerContent 即网盘解析)。独立类型为 Phase 3+ 直连网盘 API 留位,
/// 现阶段不抢 can_resolve。
pub struct NetdiskResolver;

#[async_trait::async_trait]
impl Resolver for NetdiskResolver {
    fn name(&self) -> &str {
        "netdisk"
    }

    fn can_resolve(&self, _input: &ResolveInput) -> bool {
        // 直连网盘 API 解析尚未实现, 保持 false (02-resolver.md 的扩展位)
        false
    }

    async fn resolve(
        &self,
        _input: &ResolveInput,
    ) -> Result<MediaResource, ResolveError> {
        Err(ResolveError::Unsupported {
            source: self.name().into(),
        })
    }
}

// ---------------------------------------------------------------------------
// Manager
// ---------------------------------------------------------------------------

/// 按注册顺序派发的 Resolver 管理器
pub struct ResolverManager {
    resolvers: Vec<Arc<dyn Resolver>>,
}

impl ResolverManager {
    /// 默认链: direct → spider → local_file → netdisk(扩展位)
    pub fn with_defaults(fetcher: Arc<dyn SpiderPlayFetcher>) -> Self {
        Self {
            resolvers: vec![
                Arc::new(DirectResolver),
                Arc::new(SpiderResolver::new(fetcher)),
                Arc::new(LocalFileResolver),
                Arc::new(NetdiskResolver),
            ],
        }
    }

    pub fn register(&mut self, resolver: Arc<dyn Resolver>) {
        self.resolvers.push(resolver);
    }

    pub async fn resolve(
        &self,
        input: &ResolveInput,
    ) -> Result<MediaResource, ResolveError> {
        let resolver = self
            .resolvers
            .iter()
            .find(|r| r.can_resolve(input))
            .ok_or_else(|| ResolveError::Unsupported {
                source: input.source.clone(),
            })?;
        log::info!(
            "[Resolver] {} 接手: {}",
            resolver.name(),
            input
        );
        resolver.resolve(input).await
    }
}

/// 资源 id: source + 集数定位 (够 Gateway/日志定位, 不含敏感直链)
fn resource_id(source: &str, episode_id: &str) -> String {
    format!("{source}+{}", crate::spider::trunc(episode_id, 40))
}

fn trunc_url(url: &str) -> String {
    crate::spider::trunc(url, 80)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    /// 桩 fetcher: 返回预设结果或错误
    struct StubFetcher {
        result: Mutex<Option<Result<RawPlayResult, ResolveError>>>,
    }

    impl StubFetcher {
        fn ok(url: &str, ua: &str) -> Self {
            Self {
                result: Mutex::new(Some(Ok(RawPlayResult {
                    url: url.into(),
                    header: json!({"User-Agent": ua}),
                }))),
            }
        }

        fn login_required() -> Self {
            Self {
                result: Mutex::new(Some(Err(ResolveError::AuthenticationRequired {
                    source: "spider".into(),
                    message: "该源需要登录夸克网盘".into(),
                }))),
            }
        }

        fn empty_url() -> Self {
            Self {
                result: Mutex::new(Some(Ok(RawPlayResult {
                    url: String::new(),
                    header: serde_json::Value::Null,
                }))),
            }
        }
    }

    #[async_trait::async_trait]
    impl SpiderPlayFetcher for StubFetcher {
        async fn player_content(
            &self,
            _class: &str,
            _flag: &str,
            _id: &str,
        ) -> Result<RawPlayResult, ResolveError> {
            self.result
                .lock()
                .unwrap()
                .take()
                .expect("stub called once")
        }
    }

    #[tokio::test]
    async fn direct_resolver_plain_mp4() {
        let manager = ResolverManager::with_defaults(Arc::new(StubFetcher::empty_url()));
        let r = manager
            .resolve(&ResolveInput::direct("cms1", "https://cdn.x.com/v/a.mp4"))
            .await
            .unwrap();
        assert_eq!(r.resource_type, ResourceType::File);
        assert!(!r.proxy_required);
        assert!(r.user_agent.is_none());
    }

    #[tokio::test]
    async fn direct_resolver_hls() {
        let manager = ResolverManager::with_defaults(Arc::new(StubFetcher::empty_url()));
        let r = manager
            .resolve(&ResolveInput::direct(
                "cms2",
                "https://cdn.x.com/hls/index.m3u8",
            ))
            .await
            .unwrap();
        assert_eq!(r.resource_type, ResourceType::Hls);
    }

    #[tokio::test]
    async fn direct_resolver_rejects_raw_id() {
        // 非 spider 源但输入是网盘 raw id (非 url): DirectResolver 不接, 无 resolver
        // 能处理 → Unsupported (旧链路下这形态只出现在 site_type=3, 此为防御)
        let manager = ResolverManager::with_defaults(Arc::new(StubFetcher::empty_url()));
        let err = manager
            .resolve(&ResolveInput::direct("cms3", "pan_baidu_12345"))
            .await
            .unwrap_err();
        assert!(matches!(err, ResolveError::Unsupported { .. }));
    }

    #[tokio::test]
    async fn spider_resolver_full_chain() {
        // playerContent 返回 kaiser 包装地址 + UA: 解包内层直链, UA 进专属字段
        let manager = ResolverManager::with_defaults(Arc::new(StubFetcher::ok(
            "http://127.0.0.1:8096/kaiser?url=https%3A%2F%2Fd.pcs.baidu.com%2Ffile%2Fabc",
            "netdisk;P2SP;2.2.91.136",
        )));
        let r = manager
            .resolve(&ResolveInput::spider("baidu1", "", "pan_123", "wexbaidu"))
            .await
            .unwrap();
        assert_eq!(r.url, "https://d.pcs.baidu.com/file/abc");
        assert_eq!(r.user_agent.as_deref(), Some("netdisk;P2SP;2.2.91.136"));
        assert!(r.proxy_required);
        assert_eq!(r.id, "baidu1+pan_123");
    }

    #[tokio::test]
    async fn spider_resolver_plain_direct_link() {
        // playerContent 直接返回可播直链 (无包装): 原样转换
        let manager = ResolverManager::with_defaults(Arc::new(StubFetcher::ok(
            "https://pan.quark.cn/s/abc/file.mkv",
            "quark-ua",
        )));
        let r = manager
            .resolve(&ResolveInput::spider("quark1", "1", "raw_9", "wexquark"))
            .await
            .unwrap();
        assert_eq!(r.resource_type, ResourceType::File);
        assert_eq!(r.user_agent.as_deref(), Some("quark-ua"));
    }

    #[tokio::test]
    async fn spider_resolver_login_required_maps_error() {
        let manager =
            ResolverManager::with_defaults(Arc::new(StubFetcher::login_required()));
        let err = manager
            .resolve(&ResolveInput::spider("baidu2", "", "pan_x", "wexbaidu"))
            .await
            .unwrap_err();
        assert!(matches!(err, ResolveError::AuthenticationRequired { .. }));
        assert!(err.to_string().contains("需要登录"));
    }

    #[tokio::test]
    async fn spider_resolver_empty_url_is_invalid() {
        let manager = ResolverManager::with_defaults(Arc::new(StubFetcher::empty_url()));
        let err = manager
            .resolve(&ResolveInput::spider("uc1", "", "raw_1", "wexuc"))
            .await
            .unwrap_err();
        assert!(matches!(err, ResolveError::InvalidResource { .. }));
    }

    #[tokio::test]
    async fn same_episode_id_resolves_consistently() {
        // 02-resolver.md 验收: 不同 resolver 产出同一形态的 MediaResource
        let manager = ResolverManager::with_defaults(Arc::new(StubFetcher::ok(
            "https://x.com/a.mp4",
            "ua",
        )));
        let spider_r = manager
            .resolve(&ResolveInput::spider("s1", "", "ep1", "csp1"))
            .await
            .unwrap();
        let direct_r = manager
            .resolve(&ResolveInput::direct("s2", "https://x.com/a.mp4"))
            .await
            .unwrap();
        assert_eq!(spider_r.resource_type, direct_r.resource_type);
        assert_eq!(spider_r.url, direct_r.url);
    }

    #[tokio::test]
    async fn spider_route_prefers_spider_resolver() {
        // site_type=3 即使 episode_id 恰好是 http 开头, 也走 spider (raw id 语义)
        let manager = ResolverManager::with_defaults(Arc::new(StubFetcher::ok(
            "https://x.com/real.mp4",
            "",
        )));
        let input = ResolveInput {
            source: "s1".into(),
            flag: String::new(),
            episode_id: "https://looks-like-url.com/id".into(),
            spider_class: Some("wex".into()),
            site_type: Some(3),
        };
        let r = manager.resolve(&input).await.unwrap();
        assert_eq!(r.url, "https://x.com/real.mp4");
    }
}
