//! 网盘直链本地代理: 桌面端代抓网盘流量
//!
//! 背景: wex 系 spider 的 playerContent 返回的是手机本机代理地址
//! (http://127.0.0.1:8096/kaiser?url=<内层直链>), 播放器无法访问手机/模拟器
//! 本机端口。实测 MuMu 模拟器与宿主同出口公网 IP, 百度 PCS 的 dlink 按 IP
//! 绑定 → 内层直链可在桌面直接请求; 唯一缺口是 UA (百度严格校验 spider 返回
//! 的 Android UA) 与 <video> 无法带请求头。
//!
//! 方案: 本模块在 127.0.0.1 随机端口起一个流式 HTTP 代理, 把网盘直链包装成
//! /netdisk/file.mp4?url=..&ua=.. 交给 <video src>; Chromium 的 Range/拖动
//! 请求原样透传上游, 响应流式回写, 不落盘不缓存。

use std::sync::OnceLock;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

static PORT: OnceLock<u16> = OnceLock::new();

/// 已启动的代理端口; 未启动返回 None
pub fn proxy_port() -> Option<u16> {
    PORT.get().copied()
}

/// 幂等启动; 端口随机分配, 返回实际端口
pub async fn ensure_started() -> Result<u16, String> {
    if let Some(p) = PORT.get() {
        return Ok(*p);
    }
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| format!("网盘代理绑定失败: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| e.to_string())?
        .port();
    match PORT.set(port) {
        Ok(()) => {
            tokio::spawn(async move { accept_loop(listener).await });
            log::info!("[网盘代理] 本地代理就绪: http://127.0.0.1:{port}");
            Ok(port)
        }
        // 并发启动竞争: 以先到者为准, 自身监听直接丢弃
        Err(_) => Ok(PORT.get().copied().unwrap_or(port)),
    }
}

/// 把网盘直链 + UA 包装成本地代理地址。
/// 伪装 file.mp4 扩展名: 前端 isDirectPlayableUrl/isMsePlayableUrl 按扩展名
/// 判定播放路径, 伪装后无需改动任何前端判定逻辑。
pub fn wrap_proxy_url(inner: &str, ua: Option<&str>, port: u16) -> String {
    let mut url = format!(
        "http://127.0.0.1:{port}/netdisk/file.mp4?url={}",
        percent_encode(inner)
    );
    if let Some(ua) = ua {
        url.push_str(&format!("&ua={}", percent_encode(ua)));
    }
    url
}

static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

/// 最近一次 resolve 下发的网盘 UA (百度严格校验完整 UA, 差一个字符即 403)。
/// 播放器请求代理时 URL 里本应带 &ua=, 但任何一环截断/丢失都会导致 403,
/// 故 resolve 时缓存一份, 转发缺失时兜底。
static LAST_UA: StdMutex<Option<String>> = StdMutex::new(None);
use std::sync::Mutex as StdMutex;

/// resolve 链路调用: 记录 spider 返回的原始 UA
pub fn remember_user_agent(ua: &str) {
    *LAST_UA.lock().unwrap() = Some(ua.to_string());
    log::warn!("[网盘代理] 记录 UA({} 字节): {:?}", ua.len(), ua);
}

fn effective_ua(param_ua: Option<String>) -> Option<String> {
    match param_ua {
        Some(ua) => Some(ua),
        None => {
            let cached = LAST_UA.lock().unwrap().clone();
            if cached.is_some() {
                log::warn!("[网盘代理] 请求未带 ua 参数, 使用最近 resolve 的 UA 兜底");
            }
            cached
        }
    }
}

/// 供 PlaybackGateway (gateway.rs) 复用同一连接池
pub(crate) fn client() -> &'static reqwest::Client {
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            // 流式透传不能设总超时(整个 3GB 响应都走这一个请求), 只限连接建立 (方案 §29: 5s)
            .connect_timeout(Duration::from_secs(5))
            // Phase 7: 显式重定向策略 (原为 reqwest 默认 10 跳)。
            // 网盘 302 → CDN 必须跟随, 收紧到 5 跳并显式声明
            .redirect(reqwest::redirect::Policy::limited(5))
            // 方案 §29: 连接池参数 (pool_idle_timeout 90s / tcp_keepalive 30s)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_nodelay(true)
            .tcp_keepalive(Duration::from_secs(30))
            .no_proxy()
            .build()
            .expect("netdisk proxy client")
    })
}

async fn accept_loop(listener: TcpListener) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(handle_conn(stream));
            }
            Err(e) => {
                // 瞬时 accept 错误 (Windows WSAECONNABORTED/WSAEMFILE) 在播放器
                // 试探连接/快速关闭时是常态, 绝不能 break —— 那会让网关监听永久
                // 死亡, 所有网盘播放集体卡死 (2026-09-13 真机回归教训)。退避后继续。
                log::warn!("[网盘代理] accept 瞬时错误 (忽略重试): {e}");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}

async fn handle_conn(mut stream: TcpStream) {
    let Some(head) = read_request_head(&mut stream).await else {
        return;
    };
    let text = String::from_utf8_lossy(&head);
    let first_line = text.lines().next().unwrap_or("");
    log::info!("[网盘代理] 请求: {}", trunc(first_line, 160));
    let path = first_line.split_whitespace().nth(1).unwrap_or("");
    // V2 Phase 3: /media/<token> 走 PlaybackGateway (opaque token session);
    // 旧 /netdisk/file.mp4?url=..&ua=.. 路径保持兼容, Phase 5 UI 切换后删除
    if path.starts_with("/media/") && crate::gateway::handle_media_request(&mut stream, &head).await
    {
        return;
    }
    let query = path.split_once('?').map(|(_, q)| q).unwrap_or("");
    let mut inner_url: Option<String> = None;
    let mut ua: Option<String> = None;
    for kv in query.split('&') {
        if let Some((k, v)) = kv.split_once('=') {
            match k {
                "url" => inner_url = Some(percent_decode(v)),
                "ua" => ua = Some(percent_decode(v)),
                _ => {}
            }
        }
    }
    let Some(inner_url) = inner_url else {
        let _ = stream
            .write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await;
        return;
    };
    if !inner_url.starts_with("http://") && !inner_url.starts_with("https://") {
        let _ = stream
            .write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await;
        return;
    }

    // Range 原样透传 (Chromium 的 seek 全靠它)
    let range = text.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        if k.trim().eq_ignore_ascii_case("range") {
            Some(v.trim().to_string())
        } else {
            None
        }
    });

    let mut req = client().get(&inner_url);
    if let Some(ua) = effective_ua(ua) {
        // UA 逐字节可见: 百度严格校验完整 UA, 一个字符之差即 403
        log::warn!("[网盘代理] 转发 UA({} 字节): {:?}", ua.len(), ua);
        req = req.header("User-Agent", ua);
    } else {
        log::warn!("[网盘代理] ⚠ 无可用 UA(参数与缓存均空) → 上游大概率 403");
    }
    if let Some(r) = &range {
        req = req.header("Range", r);
    }
    let mut resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            log::warn!("[网盘代理] 上游请求失败: {}", trunc(&e.to_string(), 160));
            let _ = stream
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await;
            return;
        }
    };
    let status = resp.status();
    log::info!(
        "[网盘代理] 上游响应 {} (Range: {:?}) {}",
        status.as_u16(),
        range.as_deref().unwrap_or("(无)"),
        trunc(&inner_url, 90)
    );
    if !status.is_success() && status.as_u16() != 206 {
        log::warn!("[网盘代理] 上游状态 {}: {}", status.as_u16(), trunc(&inner_url, 100));
    }

    let reason = status.canonical_reason().unwrap_or("Unknown");
    let mut out_head = format!("HTTP/1.1 {} {}\r\n", status.as_u16(), reason);
    // 透传媒体播放必需的响应头; Content-Length 决定 Chromium 的 seek 区间计算
    for key in ["Content-Type", "Content-Length", "Content-Range", "Accept-Ranges"] {
        if let Some(v) = resp.headers().get(key).and_then(|v| v.to_str().ok()) {
            out_head.push_str(&format!("{key}: {v}\r\n"));
        }
    }
    out_head.push_str("Connection: close\r\n\r\n");
    if stream.write_all(out_head.as_bytes()).await.is_err() {
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
                // 上游超时/断流: 常见于 dlink 签名过期, 只记 debug (播放器会自己报错)
                log::debug!("[网盘代理] 上游流中断: {}", trunc(&e.to_string(), 120));
                break;
            }
        }
    }
    let _ = stream.shutdown().await;
}

async fn read_request_head(s: &mut TcpStream) -> Option<Vec<u8>> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut tmp = [0u8; 4096];
    loop {
        let n = s.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            return Some(buf);
        }
        if buf.len() > 64 * 1024 {
            return None;
        }
    }
}

fn trunc(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut n = max;
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    format!("{}…", &s[..n])
}

/// query 组件级 percent-encode (RFC 3986 unreserved 之外全转义)
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + s.len() / 4);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// percent-decode (%XX; 不处理 '+', 参数由本模块生成, 空格已编码为 %20)
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let inner = "https://d.pcs.baidu.com/file/abc?bkt=en-x&ccn=CN&type=baidupan&n=目录";
        let enc = percent_encode(inner);
        assert!(!enc.contains(' '));
        // & 必须被转义, 否则会和包装参数的 &ua= 混淆
        assert!(!enc.contains('&'));
        assert_eq!(percent_decode(&enc), inner);
    }

    #[test]
    fn wrap_contains_mp4_and_params() {
        let url = wrap_proxy_url(
            "https://d.pcs.baidu.com/file/a?b=c",
            Some("com.android.chrome UA"),
            45678,
        );
        assert!(url.starts_with("http://127.0.0.1:45678/netdisk/file.mp4?url="));
        assert!(url.contains("&ua="));
        // 前端 isDirectPlayableUrl 的扩展名判定
        assert!(url.ends_with(".mp4?url=") == false);
        let q = url.split_once('?').unwrap().1;
        assert!(q.starts_with("url="));
        assert!(q.contains("&ua=com.android.chrome%20UA") || q.contains("ua="));
    }

    #[test]
    fn wrap_without_ua_omits_param() {
        let url = wrap_proxy_url("https://x.com/a", None, 1);
        assert!(!url.contains("&ua="));
    }

    #[test]
    fn decode_rejects_truncated_escape() {
        assert_eq!(percent_decode("abc%2"), "abc%2");
        assert_eq!(percent_decode("a%ZZb"), "a%ZZb");
    }

    #[tokio::test]
    async fn netdisk_range_forwarded_and_206_passthrough() {
        // 方案 §27/§45: mpv 的 Range 必须原样打给上游, 206 透传回播放器
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // 上游 mock: 校验 Range 头, 回 206 + Content-Range
        let upstream = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_port = upstream.local_addr().unwrap().port();
        let up = tokio::spawn(async move {
            let (mut s, _) = upstream.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = s.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            // reqwest 上线时头名小写化 (range:), 代理提取本就大小写不敏感; 断言按原值匹配
            assert!(
                req.to_lowercase().contains("range: bytes=50000000-"),
                "上游未收到原样 Range: {req}"
            );
            s.write_all(
                b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 50000000-50000099/100000000\r\nContent-Length: 100\r\nContent-Type: video/mp4\r\n\r\n",
            )
            .await
            .unwrap();
            s.write_all(&vec![7u8; 100]).await.unwrap();
        });

        let port = ensure_started().await.unwrap();
        let inner = format!("http://127.0.0.1:{upstream_port}/video.mp4");
        let wrapped = wrap_proxy_url(&inner, None, port);
        let path = wrapped
            .strip_prefix(&format!("http://127.0.0.1:{port}"))
            .unwrap()
            .to_string();

        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        s.write_all(
            format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nRange: bytes=50000000-\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
        // 代理响应为 Connection: close + shutdown, read_to_end 可靠终止
        let mut resp = Vec::new();
        s.read_to_end(&mut resp).await.unwrap();
        let text = String::from_utf8_lossy(&resp);
        assert!(text.starts_with("HTTP/1.1 206 Partial Content"), "{text}");
        assert!(
            text.contains("Content-Range: bytes 50000000-50000099/100000000"),
            "{text}"
        );
        let body_len = resp.len() - text.find("\r\n\r\n").unwrap() - 4;
        assert_eq!(body_len, 100);

        up.await.unwrap();
    }
}
