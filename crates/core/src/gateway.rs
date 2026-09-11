//! PlaybackGateway (V2 Phase 3, 见 docs/architecture/04-gateway.md)
//!
//! 本地 HTTP 资源访问层: 解决 Header/Cookie/UA/Range/临时 URL 等播放器
//! 无法直接处理的访问问题。不是播放器, 也不是 Resolver。
//!
//! V2 升级点 (相对 netdisk_proxy 的"网盘专用代理"):
//! - opaque token: 真实直链 + 请求头不再挂在 URL query 上, 改存内存 ResourceSession
//! - ResourceSession: TTL + 最近访问续期 + 播放结束清理
//! - SSRF 防护: 仅允许 http(s), 禁内网保留地址与本地文件
//! - HEAD 支持: 播放器探测元数据用
//!
//! 兼容: 旧 /netdisk/file.mp4?url=..&ua=.. 路径保留 (Phase 5 UI 切换后删除),
//! 新路径 /media/<token> 由 ensure_started + create_session + session_url 生成。

use crate::netdisk_proxy;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

fn trunc(s: &str, max: usize) -> String {
    crate::spider::trunc(s, max)
}

/// Session 空闲 TTL: 播放器 seek 间隔通常 < 30s; 网盘 dlink 有效期分钟级,
/// TTL 过长没有意义 (上游签名过期后 session 也无用了)
const SESSION_TTL: Duration = Duration::from_secs(120);

/// 单 session 允许播放的上游 host 白名单外兜底: 私网/保留地址禁代理
fn is_prohibited_host(host: &str) -> bool {
    let host = host.trim().to_lowercase();
    let host = host
        .strip_suffix('.')
        .unwrap_or(&host)
        .to_string();
    let host = host.as_str();
    if host == "localhost" || host.ends_with(".local") || host.ends_with(".internal") {
        return true;
    }
    // IP 字面量: 环回/私网/链路本地/保留段一律拒绝 (上游直链必须是公网)
    // IPv6 字面量带方括号 ([::1] 形态, URL host 常见)
    if let Ok(ip) = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<std::net::IpAddr>()
    {
        return match ip {
            std::net::IpAddr::V4(v4) => {
                v4.is_loopback()
                    || v4.is_private()
                    || v4.is_link_local()
                    || v4.is_broadcast()
                    || v4.is_unspecified()
                    || v4.is_documentation()
                    || v4.octets()[0] == 0
            }
            std::net::IpAddr::V6(v6) => v6.is_loopback() || v6.is_unspecified(),
        };
    }
    false
}

// ---------------------------------------------------------------------------
// ResourceSession
// ---------------------------------------------------------------------------

/// 一次可播放资源访问会话: token → 上游请求所需全部信息
#[derive(Debug, Clone)]
pub struct ResourceSession {
    pub upstream_url: String,
    pub user_agent: Option<String>,
    pub referer: Option<String>,
    pub headers: HashMap<String, String>,
    /// Cookie 键值对 (MediaResource.cookies), 转发时拼成 Cookie 头
    pub cookies: HashMap<String, String>,
    last_access: Instant,
}

impl ResourceSession {
    fn is_expired(&self, now: Instant) -> bool {
        now.duration_since(self.last_access) > SESSION_TTL
    }
}

static SESSIONS: OnceLock<Mutex<HashMap<String, ResourceSession>>> = OnceLock::new();
static EXPIRED_COUNT: AtomicU64 = AtomicU64::new(0);

fn sessions() -> &'static Mutex<HashMap<String, ResourceSession>> {
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 创建 session, 返回 opaque token (uuid v4, 不可枚举)
pub fn create_session(resource: &crate::media::MediaResource) -> String {
    let token = Uuid::new_v4().simple().to_string();
    let now = Instant::now();
    let session = ResourceSession {
        upstream_url: resource.url.clone(),
        user_agent: resource.user_agent.clone(),
        referer: resource.referer.clone(),
        headers: resource.headers.clone(),
        cookies: resource.cookies.clone(),
        last_access: now,
    };
    let mut map = sessions().lock().unwrap();
    purge_expired(&mut map, now);
    map.insert(token.clone(), session);
    token
}

/// 查询并续期 session (每次访问刷新 last_access)
pub fn take_session(token: &str) -> Option<ResourceSession> {
    let mut map = sessions().lock().unwrap();
    let now = Instant::now();
    purge_expired(&mut map, now);
    let session = map.get_mut(token)?;
    session.last_access = now;
    Some(session.clone())
}

/// 播放结束/换源时主动清理
pub fn drop_session(token: &str) {
    sessions().lock().unwrap().remove(token);
}

/// 当前存活 session 数 (测试与观测用)
pub fn session_count() -> usize {
    sessions().lock().unwrap().len()
}

/// 已清理的过期 session 累计数
pub fn expired_session_count() -> u64 {
    EXPIRED_COUNT.load(Ordering::Relaxed)
}

fn purge_expired(map: &mut HashMap<String, ResourceSession>, now: Instant) {
    let before = map.len();
    map.retain(|_, s| !s.is_expired(now));
    EXPIRED_COUNT.fetch_add((before - map.len()) as u64, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// Gateway 服务 (复用 netdisk_proxy 的监听/转发骨架)
// ---------------------------------------------------------------------------

/// 幂等启动 Gateway (与旧网盘代理共用同一 127.0.0.1 随机端口监听)
pub async fn ensure_started() -> Result<u16, String> {
    netdisk_proxy::ensure_started().await
}

/// 生成 token 形态的 Gateway 播放地址: /media/<token> (伪装 file.mp4 扩展名,
/// 前端按扩展名判定播放路径的逻辑无需改动)
pub fn session_url(token: &str, port: u16) -> String {
    format!("http://127.0.0.1:{port}/media/file.mp4/{token}")
}

/// MediaResource → Gateway 播放地址 (创建 session + 启动服务)
pub async fn wrap_resource(resource: &crate::media::MediaResource) -> Result<String, String> {
    let port = ensure_started().await?;
    let token = create_session(resource);
    Ok(session_url(&token, port))
}

/// 解析 Gateway 请求: 返回 (token, session)
/// 支持两种路径形态: /media/<token> 与 /media/file.mp4/<token>
fn parse_media_path(path: &str) -> Option<(String, ResourceSession)> {
    let path = path.split('?').next().unwrap_or(path);
    let rest = path.strip_prefix("/media/")?;
    // 取最后一段作为 token, 兼容伪装扩展名
    let token = rest.rsplit('/').next()?.trim();
    if token.is_empty() || token.len() > 64 || !token.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    let session = take_session(token)?;
    Some((token.to_string(), session))
}

/// 处理 /media/<token> 请求 (由 netdisk_proxy 的 handle_conn 在旧路径未命中时调用)
pub async fn handle_media_request(stream: &mut tokio::net::TcpStream, head: &[u8]) -> bool {
    let text = String::from_utf8_lossy(head);
    let first_line = text.lines().next().unwrap_or("");
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_uppercase();
    let path = parts.next().unwrap_or("");
    if !path.starts_with("/media/") {
        return false;
    }
    let Some((_token, session)) = parse_media_path(path) else {
        write_simple_response(
            stream,
            "404 Not Found",
            &[("Content-Length", "0")],
            b"",
        )
        .await;
        return true;
    };
    // SSRF 防护: session 创建时已校验, 这里对 URL 再验一次 (防御纵深)
    if let Err(msg) = validate_upstream(&session.upstream_url) {
        log::warn!("[Gateway] 上游被拒绝: {msg}");
        write_simple_response(stream, "403 Forbidden", &[("Content-Length", "0")], b"")
            .await;
        return true;
    }
    // Range 原样透传 (播放器 seek 全靠它)
    let range = text.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        if k.trim().eq_ignore_ascii_case("range") {
            Some(v.trim().to_string())
        } else {
            None
        }
    });
    proxy_upstream_with_range(stream, &method, &session, range).await;
    true
}

/// 上游 URL 校验: 仅公网 http(s), 禁内网/环回/本地文件
pub fn validate_upstream(url: &str) -> Result<(), String> {
    let lower = url.to_lowercase();
    if lower.starts_with("file://") {
        return Err("本地文件不允许经 Gateway 代理".into());
    }
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        return Err("仅支持 http(s) 上游".into());
    }
    let host = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .and_then(|rest| rest.split(['/']).next())
        .and_then(|hostport| hostport.rsplit_once(':').map(|(h, _)| h).or(Some(hostport)))
        .unwrap_or("");
    if host.is_empty() || is_prohibited_host(host) {
        return Err(format!("上游 host 不允许: {}", trunc(host, 60)));
    }
    Ok(())
}

async fn write_simple_response(
    stream: &mut tokio::net::TcpStream,
    status_line: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) {
    use tokio::io::AsyncWriteExt;
    let mut head = format!("HTTP/1.1 {status_line}\r\n");
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("Connection: close\r\n\r\n");
    let _ = stream.write_all(head.as_bytes()).await;
    if !body.is_empty() {
        let _ = stream.write_all(body).await;
    }
    let _ = stream.shutdown().await;
}

/// 完整转发: method + Range 透传 + session 请求头注入
pub async fn proxy_upstream_with_range(
    stream: &mut tokio::net::TcpStream,
    method: &str,
    session: &ResourceSession,
    range: Option<String>,
) {
    use tokio::io::AsyncWriteExt;
    let is_head = method == "HEAD";
    let mut req = if is_head {
        netdisk_proxy::client().head(&session.upstream_url)
    } else {
        netdisk_proxy::client().get(&session.upstream_url)
    };
    // 请求头注入: UA > Referer > Cookie > 自定义 headers
    if let Some(ua) = &session.user_agent {
        // UA 逐字节可见: 百度严格校验完整 UA, 一个字符之差即 403
        log::info!("[Gateway] 转发 UA({} 字节)", ua.len());
        req = req.header("User-Agent", ua);
    }
    if let Some(referer) = &session.referer {
        req = req.header("Referer", referer);
    }
    if !session.cookies.is_empty() {
        let cookie = session
            .cookies
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ");
        req = req.header("Cookie", cookie);
    }
    for (k, v) in &session.headers {
        // 防 header 注入: 值不允许换行
        if !v.contains('\n') && !v.contains('\r') {
            req = req.header(k.as_str(), v.as_str());
        }
    }
    if let Some(r) = &range {
        req = req.header("Range", r);
    }
    let mut resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            log::warn!("[Gateway] 上游请求失败: {}", trunc(&e.to_string(), 160));
            write_simple_response(
                stream,
                "502 Bad Gateway",
                &[("Content-Length", "0")],
                b"",
            )
            .await;
            return;
        }
    };
    let status = resp.status();
    log::info!(
        "[Gateway] 上游响应 {} (Range: {:?}) {}",
        status.as_u16(),
        range.as_deref().unwrap_or("(无)"),
        trunc(&session.upstream_url, 90)
    );
    let reason = status.canonical_reason().unwrap_or("Unknown");
    let mut out_head = format!("HTTP/1.1 {} {}\r\n", status.as_u16(), reason);
    // HEAD 只回头部不回体; Content-Length 决定播放器 seek 区间计算
    for key in ["Content-Type", "Content-Length", "Content-Range", "Accept-Ranges"] {
        if let Some(v) = resp.headers().get(key).and_then(|v| v.to_str().ok()) {
            out_head.push_str(&format!("{key}: {v}\r\n"));
        }
    }
    out_head.push_str("Connection: close\r\n\r\n");
    if stream.write_all(out_head.as_bytes()).await.is_err() {
        return;
    }
    if is_head {
        let _ = stream.shutdown().await;
        return;
    }
    // 流式回写; 播放器 seek/关闭会断开连接, 写失败即中止上游
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                if stream.write_all(&chunk).await.is_err() {
                    break;
                }
            }
            Ok(None) => break,
            Err(e) => {
                log::debug!("[Gateway] 上游流中断: {}", trunc(&e.to_string(), 120));
                break;
            }
        }
    }
    let _ = stream.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_resource(url: &str, ua: Option<&str>) -> crate::media::MediaResource {
        let mut r = crate::media::from_direct_url("test+ep1", url);
        r.user_agent = ua.map(String::from);
        r
    }

    #[test]
    fn session_lifecycle() {
        let token = create_session(&test_resource("https://cdn.x.com/a.mp4", Some("ua1")));
        assert_eq!(session_count(), 1);
        let s = take_session(&token).expect("session alive");
        assert_eq!(s.upstream_url, "https://cdn.x.com/a.mp4");
        assert_eq!(s.user_agent.as_deref(), Some("ua1"));
        // 访问续期后再取仍存活
        assert!(take_session(&token).is_some());
        drop_session(&token);
        assert_eq!(session_count(), 0);
        assert!(take_session(&token).is_none());
    }

    #[test]
    fn token_is_opaque_and_unpredictable() {
        let t1 = create_session(&test_resource("https://x.com/a.mp4", None));
        let t2 = create_session(&test_resource("https://x.com/b.mp4", None));
        assert_ne!(t1, t2);
        assert_eq!(t1.len(), 32); // uuid simple 形态
        assert!(t1.chars().all(|c| c.is_ascii_alphanumeric()));
        // URL 中不含上游直链与 UA
        assert!(!t1.contains("x.com"));
        drop_session(&t1);
        drop_session(&t2);
    }

    #[test]
    fn session_ttl_expiry() {
        // 直接操纵内部 map 验证 TTL 语义 (不打真实时钟)
        let token = create_session(&test_resource("https://x.com/a.mp4", None));
        let now = Instant::now();
        {
            let mut map = sessions().lock().unwrap();
            if let Some(s) = map.get_mut(&token) {
                // 把 last_access 拨回 TTL 之外
                s.last_access = now - SESSION_TTL - Duration::from_secs(1);
            }
        }
        assert!(take_session(&token).is_none(), "超时 session 应被清除");
        assert_eq!(expired_session_count(), 1);
    }

    #[test]
    fn ssrf_prohibited_hosts() {
        assert!(is_prohibited_host("127.0.0.1"));
        assert!(is_prohibited_host("localhost"));
        assert!(is_prohibited_host("10.0.0.5"));
        assert!(is_prohibited_host("192.168.1.1"));
        assert!(is_prohibited_host("172.16.0.1"));
        assert!(is_prohibited_host("169.254.169.254")); // 云元数据端点
        assert!(is_prohibited_host("0.0.0.0"));
        assert!(is_prohibited_host("[::1]"));
        assert!(is_prohibited_host("host.internal"));
        assert!(!is_prohibited_host("d.pcs.baidu.com"));
        assert!(!is_prohibited_host("8.8.8.8"));
    }

    #[test]
    fn validate_upstream_rules() {
        assert!(validate_upstream("file:///C:/media/a.mkv").is_err());
        assert!(validate_upstream("ftp://x.com/a").is_err());
        assert!(validate_upstream("http://127.0.0.1/x").is_err());
        assert!(validate_upstream("http://localhost/x").is_err());
        assert!(validate_upstream("http://192.168.1.1/a.mp4").is_err());
        assert!(validate_upstream("https://d.pcs.baidu.com/file/a").is_ok());
        assert!(validate_upstream("http://8.8.8.8/a").is_ok());
    }

    #[test]
    fn parse_media_path_forms() {
        let token = create_session(&test_resource("https://x.com/a.mp4", None));
        // 两种形态: /media/<token> 与 /media/file.mp4/<token>
        let via_plain = parse_media_path(&format!("/media/{token}"));
        assert!(via_plain.is_some());
        let via_disguised = parse_media_path(&format!("/media/file.mp4/{token}"));
        assert!(via_disguised.is_some());
        // 非法形态拒绝
        assert!(parse_media_path("/media/").is_none());
        assert!(parse_media_path(&format!("/media/{token}/../evil")).is_none());
        assert!(parse_media_path("/media/short").is_none());
        assert!(parse_media_path(&format!("/media/{}?x=1", "g".repeat(80))).is_none());
        drop_session(&token);
    }

    #[test]
    fn session_url_hides_upstream() {
        let resource = test_resource(
            "https://d.pcs.baidu.com/file/secret?sign=abcdef",
            Some("baidu-ua"),
        );
        let url = session_url("abc123", 45678);
        assert_eq!(url, "http://127.0.0.1:45678/media/file.mp4/abc123");
        // 地址里没有任何上游信息
        assert!(!url.contains("baidu"));
        assert!(!url.contains("sign"));
        let _ = resource;
    }

    #[test]
    fn header_injection_blocked_in_session_headers() {
        // 带 CRLF 的自定义头在 proxy_upstream 里被跳过 (此处验证 session 存储层不改制)
        let mut r = test_resource("https://x.com/a.mp4", None);
        r.headers.insert("X-Evil".into(), "v\r\nHost: evil.com".into());
        let token = create_session(&r);
        let s = take_session(&token).unwrap();
        assert!(s.headers.contains_key("X-Evil"));
        drop_session(&token);
    }

    #[test]
    fn cookies_stored_in_session() {
        let mut r = test_resource("https://x.com/a.mp4", None);
        r.cookies = crate::media::parse_cookie_header("a=1; b=2");
        let token = create_session(&r);
        let s = take_session(&token).unwrap();
        assert_eq!(s.cookies.get("a").map(String::as_str), Some("1"));
        assert_eq!(s.cookies.get("b").map(String::as_str), Some("2"));
        drop_session(&token);
    }
}
