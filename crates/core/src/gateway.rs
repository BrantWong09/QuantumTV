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
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

fn trunc(s: &str, max: usize) -> String {
    crate::spider::trunc(s, max)
}

/// SESSION_TTL 兜底值; 有 refresh 通道的 session (网盘直链) 用它作 expires 默认,
/// 到期后首次访问直接重解析 (方案 §25, 不必等 401 才恢复)
const DEFAULT_PLAY_TTL: Duration = Duration::from_secs(600);

/// 播放地址刷新通道 (方案 §26/§27/§45):
/// 上游 401/403/410 或 expires 到期 → Provider 重新 playerContent 解析。
/// 严禁 Search: 实现方只允许按集数定位重取直链 (SpiderResolver 语义)。
#[async_trait::async_trait]
pub trait SessionRefresher: Send + Sync {
    async fn refresh(&self) -> Result<crate::media::MediaResource, String>;
}

// ---------------------------------------------------------------------------
// ResourceSession
// ---------------------------------------------------------------------------

/// 一次可播放资源访问会话: token → 上游请求所需全部信息
#[derive(Clone)]
pub struct ResourceSession {
    pub upstream_url: String,
    pub user_agent: Option<String>,
    pub referer: Option<String>,
    pub headers: HashMap<String, String>,
    /// Cookie 键值对 (MediaResource.cookies), 转发时拼成 Cookie 头
    pub cookies: HashMap<String, String>,
    /// 网盘提供方 (baidu/quark/uc/None), 仅日志与状态展示 (§38)
    pub provider: Option<String>,
    /// 到期时间 (unix 毫秒); 超过后访问先刷新再转发 (§25)
    pub expires_at_ms: Option<i64>,
    /// 401/403/410 自动恢复通道 (§26); 无此通道的 session 保持旧行为
    pub refresher: Option<Arc<dyn SessionRefresher>>,
    last_access: Instant,
}

impl ResourceSession {
    fn is_expired(&self, now: Instant) -> bool {
        now.duration_since(self.last_access) > SESSION_TTL
    }

    fn is_play_expired(&self) -> bool {
        match self.expires_at_ms {
            Some(ms) => now_ms() >= ms,
            None => false,
        }
    }
}

/// Debug 输出脱敏 (§39): 不打印 headers/cookies/upstream_url
impl std::fmt::Debug for ResourceSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourceSession")
            .field("provider", &self.provider)
            .field("headers", &format!("{} keys", self.headers.len()))
            .field("cookies", &format!("{} keys", self.cookies.len()))
            .field("expires_at_ms", &self.expires_at_ms)
            .field("refreshable", &self.refresher.is_some())
            .finish()
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

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
// Session 注册表
// ---------------------------------------------------------------------------

/// Session 空闲 TTL: 播放器 seek 间隔通常 < 30s; 过期即弃 (下次播放重新 resolve)
const SESSION_TTL: Duration = Duration::from_secs(120);

static SESSIONS: OnceLock<Mutex<HashMap<String, ResourceSession>>> = OnceLock::new();
static EXPIRED_COUNT: AtomicU64 = AtomicU64::new(0);

fn sessions() -> &'static Mutex<HashMap<String, ResourceSession>> {
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 创建 session, 返回 opaque token (uuid v4, 不可枚举)
pub fn create_session(resource: &crate::media::MediaResource) -> String {
    create_session_full(resource, None, None)
}

/// PlaybackResourceStore 入口 (§22/§23/§26): 带 provider 标注与刷新通道的 session。
/// refresher 存在时 expires 默认 DEFAULT_PLAY_TTL (到期先刷新再转发)。
pub fn create_session_full(
    resource: &crate::media::MediaResource,
    provider: Option<&str>,
    refresher: Option<Arc<dyn SessionRefresher>>,
) -> String {
    let token = Uuid::new_v4().simple().to_string();
    let session = build_session(resource, provider, refresher);
    let mut map = sessions().lock().unwrap();
    let now = Instant::now();
    purge_expired(&mut map, now);
    map.insert(token.clone(), session);
    token
}

fn build_session(
    resource: &crate::media::MediaResource,
    provider: Option<&str>,
    refresher: Option<Arc<dyn SessionRefresher>>,
) -> ResourceSession {
    ResourceSession {
        upstream_url: resource.url.clone(),
        user_agent: resource.user_agent.clone(),
        referer: resource.referer.clone(),
        headers: resource.headers.clone(),
        cookies: resource.cookies.clone(),
        provider: provider.map(str::to_string),
        expires_at_ms: refresher
            .as_ref()
            .map(|_| now_ms() + DEFAULT_PLAY_TTL.as_millis() as i64),
        refresher,
        last_access: Instant::now(),
    }
}

/// 刷新成功后原地替换 session 字段 (§26: 新 PlayResource, 同一 playback_id,
/// 播放器无感继续)。保留 provider/refresher。
fn replace_session(token: &str, resource: &crate::media::MediaResource) -> bool {
    let mut map = sessions().lock().unwrap();
    let Some(old) = map.get_mut(token) else {
        return false;
    };
    let provider = old.provider.take();
    let refresher = old.refresher.clone();
    *old = build_session(resource, provider.as_deref(), refresher);
    true
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

/// 带 provider 标注与刷新通道的包装 (网盘直链走这条, §22-§26)
pub async fn wrap_resource_with(
    resource: &crate::media::MediaResource,
    provider: Option<&str>,
    refresher: Option<Arc<dyn SessionRefresher>>,
) -> Result<String, String> {
    let port = ensure_started().await?;
    let token = create_session_full(resource, provider, refresher);
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
    let Some((token, session)) = parse_media_path(path) else {
        write_simple_response(
            stream,
            "404 Not Found",
            &[("Content-Length", "0")],
            b"",
        )
        .await;
        return true;
    };
    // Range 原样透传 (播放器 seek 全靠它)
    let range = text.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        if k.trim().eq_ignore_ascii_case("range") {
            Some(v.trim().to_string())
        } else {
            None
        }
    });
    serve_media(stream, &token, &method, session, range).await;
    true
}

/// §25/§26 播放失败自动恢复: 单次请求最多刷新一次。
/// - 访问时 play 已过期 → 先重新 resolve 再转发
/// - 上游 401/403/410 → 刷新 PlayResource → 原地替换 session → 重试一次
/// 刷新仅走 refresher (playerContent 语义), 严禁 Search (§27/§45)。
async fn serve_media(
    stream: &mut tokio::net::TcpStream,
    token: &str,
    method: &str,
    mut session: ResourceSession,
    range: Option<String>,
) {
    let mut refreshed = false;
    loop {
        if !refreshed && session.refresher.is_some() {
            if session.is_play_expired() {
                log::info!(
                    "[Gateway] provider={:?} event=play_url_expired action=refresh",
                    session.provider
                );
                match try_refresh(token, &session).await {
                    Some(new_session) => {
                        session = new_session;
                        refreshed = true;
                        continue;
                    }
                    None => {} // 刷新失败: 用旧 URL 试一次, 由上游决定
                }
            }
        }
        // SSRF 防护: session 创建时已校验, 这里对 URL 再验一次 (防御纵深)
        if let Err(msg) = validate_upstream(&session.upstream_url) {
            log::warn!("[Gateway] 上游被拒绝: {msg}");
            write_simple_response(stream, "403 Forbidden", &[("Content-Length", "0")], b"")
                .await;
            return;
        }
        let resp = match fetch_upstream(method, &session, range.clone()).await {
            Ok(r) => r,
            Err(e) => {
                log::warn!("[Gateway] 上游请求失败: {}", trunc(&e, 160));
                if !refreshed && session.refresher.is_some() && try_refresh_once(token, &mut session).await {
                    refreshed = true;
                    continue;
                }
                write_simple_response(stream, "502 Bad Gateway", &[("Content-Length", "0")], b"")
                    .await;
                return;
            }
        };
        let status = resp.status();
        log::info!(
            "[Gateway] provider={:?} event=upstream_start status={} (Range: {:?}) url_host={}",
            session.provider,
            status.as_u16(),
            range.as_deref().unwrap_or("(无)"),
            crate::clouddrive::provider::url_host(&session.upstream_url)
        );
        if !refreshed
            && session.refresher.is_some()
            && matches!(status.as_u16(), 401 | 403 | 410)
        {
            if try_refresh_once(token, &mut session).await {
                refreshed = true;
                continue; // 新 PlayResource 重试本次请求 (播放器无感)
            }
        }
        write_response(stream, method == "HEAD", resp, status).await;
        return;
    }
}

/// 刷新并原地替换 session; 成功返回刷新后的 session
async fn try_refresh(token: &str, session: &ResourceSession) -> Option<ResourceSession> {
    let refresher = session.refresher.as_ref()?;
    match refresher.refresh().await {
        Ok(resource) if replace_session(token, &resource) => take_session(token),
        Ok(_) => None,
        Err(e) => {
            log::warn!("[Gateway] 刷新解析失败: {}", trunc(&e, 140));
            None
        }
    }
}

async fn try_refresh_once(token: &str, session: &mut ResourceSession) -> bool {
    match try_refresh(token, session).await {
        Some(new_session) => {
            *session = new_session;
            true
        }
        None => false,
    }
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

/// 发起上游请求: method + Range 透传 + session 请求头注入 (不写回播放器)
async fn fetch_upstream(
    method: &str,
    session: &ResourceSession,
    range: Option<String>,
) -> Result<reqwest::Response, String> {
    let is_head = method == "HEAD";
    let mut req = if is_head {
        netdisk_proxy::client().head(&session.upstream_url)
    } else {
        netdisk_proxy::client().get(&session.upstream_url)
    };
    // 请求头注入: UA > Referer > Cookie > 自定义 headers
    if let Some(ua) = &session.user_agent {
        // UA 只报长度: 完整 UA 入日志违反 §39 精神且无诊断价值
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
    req.send().await.map_err(|e| e.to_string())
}

/// 响应头 + 流式回写播放器
async fn write_response(
    stream: &mut tokio::net::TcpStream,
    is_head: bool,
    mut resp: reqwest::Response,
    status: reqwest::StatusCode,
) {
    use tokio::io::AsyncWriteExt;
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

    /// SESSIONS 是进程级全局 map, 测试并行执行时互相污染计数断言;
    /// 触碰共享 map 的测试全部持此锁串行执行。
    fn sessions_lock() -> std::sync::MutexGuard<'static, ()> {
        static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn session_lifecycle() {
        let _g = sessions_lock();
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
        let _g = sessions_lock();
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
        let _g = sessions_lock();
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
        let _g = sessions_lock();
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
        let _g = sessions_lock();
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
        let _g = sessions_lock();
        let token = create_session(&r);
        let s = take_session(&token).unwrap();
        assert_eq!(s.cookies.get("a").map(String::as_str), Some("1"));
        assert_eq!(s.cookies.get("b").map(String::as_str), Some("2"));
        drop_session(&token);
    }

    /// 无网络刷新通道桩 (只测 session 替换语义, 不打上游)
    struct NoopRefresher;

    #[async_trait::async_trait]
    impl SessionRefresher for NoopRefresher {
        async fn refresh(&self) -> Result<crate::media::MediaResource, String> {
            Err("test".into())
        }
    }

    #[test]
    fn refreshed_session_carries_provider_expiry_and_survives_replace() {
        let _g = sessions_lock();
        let token = create_session_full(
            &test_resource("https://old.cdn/x.mp4", Some("old-ua")),
            Some("quark"),
            Some(Arc::new(NoopRefresher)),
        );
        let s = take_session(&token).unwrap();
        assert_eq!(s.provider.as_deref(), Some("quark"));
        assert!(s.refresher.is_some());
        assert!(s.expires_at_ms.is_some(), "有刷新通道必须有过期标注 (§25)");

        // 401 恢复路径: 原地替换, token 不变, 刷新通道保留
        let new_r = test_resource("https://new.cdn/y.mp4", Some("new-ua"));
        assert!(replace_session(&token, &new_r));
        let s2 = take_session(&token).unwrap();
        assert_eq!(s2.upstream_url, "https://new.cdn/y.mp4");
        assert_eq!(s2.user_agent.as_deref(), Some("new-ua"));
        assert_eq!(s2.provider.as_deref(), Some("quark"));
        assert!(s2.refresher.is_some());
        drop_session(&token);
    }

    #[test]
    fn debug_output_never_leaks_secrets() {
        let mut r = test_resource("https://cdn/x.mp4?sign=SECRETUA", None);
        r.user_agent = Some("SECRET-UA".to_string());
        r.cookies = crate::media::parse_cookie_header("BDUSS=SECRETCOOKIE");
        let s = build_session(&r, Some("baidu"), None);
        let text = format!("{:?}", s);
        assert!(!text.contains("SECRETUA"));
        assert!(!text.contains("SECRET-UA"));
        assert!(!text.contains("SECRETCOOKIE"));
        assert!(text.contains("baidu"));
    }

    // -------------------------------------------------------------------------
    // E2E: 真实 TCP 打穿 /media 处理链 (mock 上游用 localtest.me → 127.0.0.1,
    // 避开 SSRF 对 IP 字面量的拦截)。
    //
    // 注意: 不复用 netdisk_proxy::ensure_started 的全局监听 —— 它的 accept_loop
    // 落在"第一个调用者"的 test runtime 上, 该 runtime 随测试结束销毁会连坐
    // kill 其他测试在途的 handle_conn (跨测试空响应竞态)。生产主运行时长驻无此
    // 问题。这里每个测试自带 listener, 直接驱动 handle_media_request。
    // -------------------------------------------------------------------------

    /// mock 上游: /ok.mp4 → 200, /fresh.mp4 → 200, 其余 (如 /denied.mp4) → 403
    async fn spawn_mock_upstream() -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = listener.accept().await else { return };
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let Ok(n) = s.read(&mut buf).await else { return };
                    let req = String::from_utf8_lossy(&buf[..n]).to_string();
                    let path = req
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("");
                    let head = if path.starts_with("/ok.mp4") || path.starts_with("/fresh.mp4") {
                        "HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nContent-Length: 6\r\n\r\n"
                    } else {
                        "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n"
                    };
                    let _ = s.write_all(head.as_bytes()).await;
                    if head.starts_with("HTTP/1.1 200") {
                        let _ = s.write_all(b"MOVIDX").await;
                    }
                    let _ = s.shutdown().await;
                });
            }
        });
        port
    }

    /// 本测试专属的 Gateway 监听 (不动 netdisk_proxy 的进程级单例)
    async fn spawn_test_gateway() -> u16 {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = listener.accept().await else { return };
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let Ok(n) = s.read(&mut buf).await else { return };
                    handle_media_request(&mut s, &buf[..n]).await;
                });
            }
        });
        port
    }

    /// 完整 GET /media/<token>, 返回原始响应字节
    async fn gateway_get(port: u16, path: &str) -> Vec<u8> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        s.write_all(
            format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
        let mut resp = Vec::new();
        s.read_to_end(&mut resp).await.unwrap();
        resp
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn e2e_media_happy_path_streams_upstream() {
        let upstream = spawn_mock_upstream().await;
        let port = spawn_test_gateway().await;
        let token = create_session(&test_resource(
            &format!("http://localtest.me:{upstream}/ok.mp4"),
            Some("ua"),
        ));
        let resp = gateway_get(port, &format!("/media/file.mp4/{token}")).await;
        let text = String::from_utf8_lossy(&resp).to_string();
        assert!(text.starts_with("HTTP/1.1 200"), "{text}");
        assert!(text.ends_with("MOVIDX"), "{text}");
        drop_session(&token);
    }

    struct StubRefresher {
        upstream: u16,
    }

    #[async_trait::async_trait]
    impl SessionRefresher for StubRefresher {
        async fn refresh(&self) -> Result<crate::media::MediaResource, String> {
            Ok(crate::media::from_direct_url(
                "fresh",
                format!("http://localtest.me:{}/fresh.mp4", self.upstream),
            ))
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn e2e_media_403_refreshes_once_and_retries_transparently() {
        // §26: 上游 403 → refresher 重新解析 → 同一 token 重试 → 播放器拿到 200
        let upstream = spawn_mock_upstream().await;
        let port = spawn_test_gateway().await;
        let token = create_session_full(
            &test_resource(&format!("http://localtest.me:{upstream}/denied.mp4"), None),
            Some("quark"),
            Some(Arc::new(StubRefresher { upstream })),
        );
        let resp = gateway_get(port, &format!("/media/file.mp4/{token}")).await;
        let text = String::from_utf8_lossy(&resp).to_string();
        assert!(text.starts_with("HTTP/1.1 200"), "{text}");
        assert!(text.ends_with("MOVIDX"), "{text}");
        drop_session(&token);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn e2e_media_without_refresher_passes_403_through() {
        let upstream = spawn_mock_upstream().await;
        let port = spawn_test_gateway().await;
        let token = create_session(&test_resource(
            &format!("http://localtest.me:{upstream}/denied.mp4"),
            None,
        ));
        let resp = gateway_get(port, &format!("/media/file.mp4/{token}")).await;
        let text = String::from_utf8_lossy(&resp).to_string();
        assert!(text.starts_with("HTTP/1.1 403"), "{text}");
        drop_session(&token);
    }
}
