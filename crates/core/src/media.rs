//! 统一媒体资源模型 (V2 Phase 1, 见 docs/architecture/01-core-model.md)
//!
//! MediaResource 是所有播放入口的唯一资源描述: 与 Source 无关、与播放器无关。
//! 本阶段只新增模型与转换入口, 播放器与 UI 不接线 (07-migration.md Phase 1 约束)。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 资源类型: 决定播放路径选择 (直连 / 经 Gateway / 播放器协议)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceType {
    /// 直接视频文件 (mp4/flv/mkv/...)
    File,
    /// 普通 HTTP 流 (无法进一步归类的 url)
    Http,
    /// HLS (m3u8/m3u)
    Hls,
    /// DASH (mpd)
    Dash,
    /// 本地文件
    LocalFile,
    /// 无法识别
    Unknown,
}

impl ResourceType {
    /// 按 URL 特征推断资源类型 (spider 直链化后无扩展名的 url 兜底为 Http)
    pub fn detect(url: &str) -> ResourceType {
        if url.starts_with("file://") {
            return ResourceType::LocalFile;
        }
        if !url.starts_with("http://") && !url.starts_with("https://") {
            // 网盘 raw id 等非 url 输入在转换层兜底, 这里不猜
            return ResourceType::Unknown;
        }
        // 去掉 query 后按扩展名判定 (本地代理伪装 /netdisk/file.mp4?url=.. 命中 mp4)
        let path = url.split(['?', '#']).next().unwrap_or("");
        let lower = path.to_lowercase();
        if lower.ends_with(".m3u8") || lower.ends_with(".m3u") {
            ResourceType::Hls
        } else if lower.ends_with(".mpd") {
            ResourceType::Dash
        } else if ["mp4", "mkv", "flv", "avi", "mov", "webm", "ts", "wmv"]
            .iter()
            .any(|ext| lower.ends_with(&format!(".{ext}")))
        {
            ResourceType::File
        } else {
            ResourceType::Http
        }
    }

    /// 该类型资源是否应经 PlaybackGateway 访问
    /// (带访问要求的 http 流, 由 proxy_required 显式判定, 此处只给类型维度的默认值)
    pub fn defaults_to_proxy(&self) -> bool {
        matches!(self, ResourceType::Unknown)
    }
}

/// 字幕资源
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtitleResource {
    pub url: String,
    pub language: Option<String>,
    pub format: Option<String>,
}

/// 媒体元数据 (展示用, 不承载播放状态)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MediaMetadata {
    pub title: Option<String>,
    pub episode: Option<String>,
    pub duration: Option<f64>,
}

/// 统一媒体资源: 所有播放路径的标准化终点 (01-core-model.md §1)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaResource {
    pub id: String,
    pub url: String,
    pub resource_type: ResourceType,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub cookies: HashMap<String, String>,
    pub user_agent: Option<String>,
    pub referer: Option<String>,
    /// 是否必须经本地 PlaybackGateway 播放
    /// (播放器无法带请求头/需要隐藏直链/UA 校验严格等场景)
    pub proxy_required: bool,
    #[serde(default)]
    pub subtitles: Vec<SubtitleResource>,
    #[serde(default)]
    pub metadata: MediaMetadata,
}

impl MediaResource {
    /// 基础构造: url + 类型, 其余字段默认
    pub fn new(id: impl Into<String>, url: impl Into<String>, resource_type: ResourceType) -> Self {
        Self {
            id: id.into(),
            url: url.into(),
            resource_type,
            headers: HashMap::new(),
            cookies: HashMap::new(),
            user_agent: None,
            referer: None,
            proxy_required: false,
            subtitles: Vec::new(),
            metadata: MediaMetadata::default(),
        }
    }

    /// 从 spider playerContent 的 header JSON 解析请求头。
    /// wex 系 header 有两种形态 (对象 / JSON 字符串), 与 spider::header_user_agent
    /// 同源的宽松解析; User-Agent / Referer / Cookie 提升为专属字段, 其余进 headers。
    pub fn from_header_json(mut self, header: &serde_json::Value) -> Self {
        let obj = match header {
            serde_json::Value::String(s) => serde_json::from_str::<serde_json::Value>(s)
                .unwrap_or(serde_json::Value::Null),
            other => other.clone(),
        };
        let Some(map) = obj.as_object() else {
            return self;
        };
        for (k, v) in map {
            let Some(val) = v.as_str() else { continue };
            let val = val.trim().to_string();
            if val.is_empty() {
                continue;
            }
            if k.eq_ignore_ascii_case("User-Agent") {
                self.user_agent = Some(val);
            } else if k.eq_ignore_ascii_case("Referer") {
                self.referer = Some(val);
            } else if k.eq_ignore_ascii_case("Cookie") {
                self.cookies = parse_cookie_header(&val);
            } else {
                self.headers.insert(k.clone(), val);
            }
        }
        self
    }

    /// 标记必须经 Gateway 播放 (网盘直链等播放器无法直接访问的资源)
    pub fn with_proxy_required(mut self, required: bool) -> Self {
        self.proxy_required = required;
        self
    }

    pub fn with_metadata(mut self, metadata: MediaMetadata) -> Self {
        self.metadata = metadata;
        self
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.metadata.title = Some(title.into());
        self
    }

    pub fn with_episode(mut self, episode: impl Into<String>) -> Self {
        self.metadata.episode = Some(episode.into());
        self
    }
}

/// Cookie 请求头字符串 → 键值对 ("a=1; b=2" → {"a":"1","b":"2"})
pub fn parse_cookie_header(cookie: &str) -> HashMap<String, String> {
    cookie
        .split(';')
        .filter_map(|pair| pair.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .filter(|(k, v)| !k.is_empty() && !v.is_empty())
        .collect()
}

// ---------------------------------------------------------------------------
// 转换入口 (Phase 1: 只建立转换, 不改播放器/UI 接线)
// ---------------------------------------------------------------------------

/// spider playerContent 原始结果 (url + header JSON) → MediaResource。
///
/// 对应现有 `resolve_spider_episode` 的解析链输出:
/// - header 中 UA/Referer/Cookie 解析进专属字段 (Phase 3 Gateway 接管前,
///   proxy_required=true 表示该资源需要本地代理补请求头才能播)
/// - id 由调用方携带 (source+episode 定位), 此处透传
pub fn from_spider_play_result(
    id: impl Into<String>,
    url: impl Into<String>,
    header: &serde_json::Value,
) -> MediaResource {
    let url = url.into();
    let resource_type = ResourceType::detect(&url);
    MediaResource::new(id, url, resource_type)
        .from_header_json(header)
        // 解析出的直链通常带签名/UA 校验, 默认要求经 Gateway;
        // Phase 3 由 Gateway 按 session 实际需要收紧
        .with_proxy_required(true)
}

/// 已可直接播放的 http(s) URL → MediaResource (TVBox 直链/CMS m3u8 等)。
/// 无 header 要求 → mpv/播放器可直连, 不强制走 Gateway。
pub fn from_direct_url(id: impl Into<String>, url: impl Into<String>) -> MediaResource {
    let url = url.into();
    let resource_type = ResourceType::detect(&url);
    MediaResource::new(id, url, resource_type)
}

/// 本地文件 → MediaResource
pub fn from_local_file(id: impl Into<String>, path: impl Into<String>) -> MediaResource {
    MediaResource::new(id, path.into(), ResourceType::LocalFile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detect_mp4() {
        assert_eq!(
            ResourceType::detect("https://x.com/v/a.mp4"),
            ResourceType::File
        );
        assert_eq!(
            ResourceType::detect("https://x.com/v/a.mp4?sign=abc"),
            ResourceType::File
        );
    }

    #[test]
    fn detect_hls_and_dash() {
        assert_eq!(
            ResourceType::detect("https://x.com/v/index.m3u8"),
            ResourceType::Hls
        );
        assert_eq!(
            ResourceType::detect("https://x.com/v/index.M3U8?token=1"),
            ResourceType::Hls
        );
        assert_eq!(
            ResourceType::detect("https://x.com/v/index.mpd"),
            ResourceType::Dash
        );
    }

    #[test]
    fn detect_proxy_disguised_url_is_file() {
        // 本地网盘代理伪装扩展名: /netdisk/file.mp4?url=.. 必须命中 File 分支
        assert_eq!(
            ResourceType::detect("http://127.0.0.1:45678/netdisk/file.mp4?url=https%3A%2F%2Fd.pcs.baidu.com%2Ffile%2Fa"),
            ResourceType::File
        );
    }

    #[test]
    fn detect_local_and_unknown() {
        assert_eq!(
            ResourceType::detect("file:///C:/media/a.mkv"),
            ResourceType::LocalFile
        );
        // 网盘 raw id (非 url)
        assert_eq!(ResourceType::detect("pan_baidu_12345"), ResourceType::Unknown);
        assert_eq!(
            ResourceType::detect("https://x.com/play?id=1"),
            ResourceType::Http
        );
    }

    #[test]
    fn header_json_object_form() {
        let r = MediaResource::new("e1", "https://x.com/a.mp4", ResourceType::File)
            .from_header_json(&json!({
                "User-Agent": "okhttp/4.9",
                "Referer": "https://x.com/",
                "Cookie": "a=1; b=2",
                "X-Custom": "v1"
            }));
        assert_eq!(r.user_agent.as_deref(), Some("okhttp/4.9"));
        assert_eq!(r.referer.as_deref(), Some("https://x.com/"));
        assert_eq!(r.cookies.get("a").map(String::as_str), Some("1"));
        assert_eq!(r.headers.get("X-Custom").map(String::as_str), Some("v1"));
        // User-Agent 不重复留在 headers
        assert!(!r.headers.contains_key("User-Agent"));
    }

    #[test]
    fn header_json_string_form() {
        // wex 实测返回 JSON 字符串形态
        let r = MediaResource::new("e1", "https://x.com/a.mp4", ResourceType::File)
            .from_header_json(&json!("{\"User-Agent\": \" ExoPlayer \"}"));
        assert_eq!(r.user_agent.as_deref(), Some("ExoPlayer"));
    }

    #[test]
    fn header_json_invalid_is_ignored() {
        let r = MediaResource::new("e1", "https://x.com/a.mp4", ResourceType::File)
            .from_header_json(&json!("not json"));
        assert!(r.user_agent.is_none());
        assert!(r.headers.is_empty());

        let r = MediaResource::new("e1", "https://x.com/a.mp4", ResourceType::File)
            .from_header_json(&serde_json::Value::Null);
        assert!(r.user_agent.is_none());
    }

    #[test]
    fn cookie_header_parsing() {
        let cookies = parse_cookie_header("a=1; b = 2 ; ; c=");
        assert_eq!(cookies.get("a").map(String::as_str), Some("1"));
        assert_eq!(cookies.get("b").map(String::as_str), Some("2"));
        assert!(!cookies.contains_key("c"));
        assert_eq!(cookies.len(), 2);
    }

    #[test]
    fn media_resource_serializes_with_defaults() {
        // TypeScript 侧接口要求 headers/cookies/subtitles/metadata 可省略:
        // serde default 字段序列化时不丢, 反序列化缺失时可还原
        let r = MediaResource::new("e1", "https://x.com/a.mp4", ResourceType::File);
        let json = serde_json::to_string(&r).unwrap();
        let back: MediaResource = serde_json::from_str(&json).unwrap();
        assert_eq!(back.resource_type, ResourceType::File);
        assert!(back.headers.is_empty());
        assert!(!back.proxy_required);
    }

    #[test]
    fn resource_type_snake_case() {
        // TS 侧 resourceType: 'file' | 'hls' | ... 与 Rust 保持一致
        assert_eq!(
            serde_json::to_string(&ResourceType::Hls).unwrap(),
            "\"hls\""
        );
    }

    #[test]
    fn spider_play_result_with_ua() {
        // 网盘直链: 百度严格校验完整 Android UA
        let r = from_spider_play_result(
            "baidu+ep1",
            "https://d.pcs.baidu.com/file/abc?bkt=en-x",
            &json!({"User-Agent": "netdisk;P2SP;2.2.91.136"}),
        );
        assert_eq!(r.resource_type, ResourceType::Http); // 无扩展名 → Http
        assert_eq!(
            r.user_agent.as_deref(),
            Some("netdisk;P2SP;2.2.91.136")
        );
        assert!(r.proxy_required);
    }

    #[test]
    fn spider_play_result_with_local_proxy_disguise() {
        // 经本地代理包装后的地址 (resolve_spider_episode 下发的实际形态)
        let r = from_spider_play_result(
            "quark+ep2",
            "http://127.0.0.1:45678/netdisk/file.mp4?url=https%3A%2F%2Fpan.quark.cn%2Fs%2Fa",
            &serde_json::Value::Null,
        );
        assert_eq!(r.resource_type, ResourceType::File);
        assert!(r.proxy_required);
    }

    #[test]
    fn spider_play_result_keeps_custom_headers() {
        // header 里除 UA/Referer/Cookie 外的自定义头必须保留 (Phase 3 Gateway 转发)
        let r = from_spider_play_result(
            "src+ep1",
            "https://x.com/a.mp4",
            &json!({"User-Agent": "ua1", "X-Token": "t", "Cookie": "k=v"}),
        );
        assert_eq!(r.headers.get("X-Token").map(String::as_str), Some("t"));
        assert_eq!(r.cookies.get("k").map(String::as_str), Some("v"));
    }

    #[test]
    fn direct_url_no_proxy() {
        // CMS/TVBox 普通 m3u8: 无 header 要求, mpv 直连
        let r = from_direct_url("cms+ep1", "https://cdn.x.com/hls/index.m3u8");
        assert_eq!(r.resource_type, ResourceType::Hls);
        assert!(!r.proxy_required);
        assert!(r.user_agent.is_none());
        assert!(r.cookies.is_empty());
    }

    #[test]
    fn direct_url_with_referer_style_query() {
        let r = from_direct_url("cms+ep2", "https://x.com/v/movie.mp4");
        assert_eq!(r.resource_type, ResourceType::File);
        assert!(!r.proxy_required);
    }

    #[test]
    fn local_file_resource() {
        let r = from_local_file("local+1", "C:\\media\\a.mkv");
        assert_eq!(r.resource_type, ResourceType::LocalFile);
    }

    #[test]
    fn conversion_preserves_metadata() {
        let r = from_direct_url("e1", "https://x.com/a.mp4")
            .with_title("庆余年")
            .with_episode("第1集");
        assert_eq!(r.metadata.title.as_deref(), Some("庆余年"));
        assert_eq!(r.metadata.episode.as_deref(), Some("第1集"));
    }
}
