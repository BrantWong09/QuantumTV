//! 桥接隧道: APK 主动注册 + 帧多路复用
//!
//! 帧格式 [u32 BE id][u32 BE len][payload]; APK 首帧 id=0 = 注册 JSON;
//! 之后桌面→APK 帧 payload = 完整 HTTP 请求字节, APK→桌面帧 = HTTP 响应体字节,
//! 按 id 配对。VirtualBridge 在 127.0.0.1:{host_port} 接 spider 层请求转进隧道。

use serde::Deserialize;

#[derive(Debug, Clone)]
pub(crate) struct Frame {
    pub id: u32,
    pub payload: Vec<u8>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RegisterInfo {
    #[serde(default)]
    pub device: String,
    #[serde(default)]
    pub apk: String,
}

/// [u32 BE id][u32 BE len][payload]
pub(crate) fn encode_frame(id: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + payload.len());
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// 从缓冲前缀解一帧; 不完整返回 None; 成功时从 buf 消耗掉对应字节
#[allow(dead_code)]
pub(crate) fn parse_frame(buf: &mut Vec<u8>) -> Option<(Frame, usize)> {
    if buf.len() < 8 {
        return None;
    }
    let id = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let len = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
    if buf.len() < 8 + len {
        return None;
    }
    let payload = buf[8..8 + len].to_vec();
    buf.drain(..8 + len);
    Some((Frame { id, payload }, 8 + len))
}

/// 注册帧 payload → (device, apk)
pub(crate) fn parse_register(payload: &[u8]) -> Option<(String, String)> {
    let reg: RegisterInfo = serde_json::from_slice(payload).ok()?;
    Some((reg.device, reg.apk))
}

// ---- 服务层 ----

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex as StdMutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Notify};

use super::session::{BridgeSession, Limits, RequestKind};

pub const DEFAULT_TUNNEL_PORT: u16 = 18099;

/// 测试入口: 用现成 listener 启动隧道/虚拟桥接 (与 ensure_started 共用 accept 循环)
#[cfg(test)]
async fn serve(tunnel: TcpListener, bridge: TcpListener, bridge_url: String) {
    STARTED.store(true, Ordering::SeqCst);
    *BRIDGE_URL.lock().unwrap() = bridge_url;
    let h1 = tokio::spawn(async move { tunnel_accept_loop(tunnel).await; });
    let h2 = tokio::spawn(async move { bridge_accept_loop(bridge).await; });
    HANDLES.lock().unwrap().extend([h1, h2]);
}

static ACTIVE: LazyLock<StdMutex<Option<Arc<BridgeSession>>>> =
    LazyLock::new(|| StdMutex::new(None));
static STARTED: AtomicBool = AtomicBool::new(false);
static NOTIFY: LazyLock<Notify> = LazyLock::new(|| Notify::new());
static HANDLES: LazyLock<StdMutex<Vec<tokio::task::JoinHandle<()>>>> =
    LazyLock::new(|| StdMutex::new(Vec::new()));
static BRIDGE_URL: StdMutex<String> = StdMutex::new(String::new());

/// 幂等启动两个常驻监听; 由 ensure_ready_with 在瀑布前调用。
/// 端口被占/被系统保留时自动向后探测最多 10 个; 返回实际生效的桥接端口。
pub async fn ensure_started(tunnel_port: u16, bridge_port: u16) -> Result<u16, String> {
    if STARTED.load(Ordering::SeqCst) {
        return Ok(bridge_port);
    }
    let tl = bind_fallback(tunnel_port, "隧道")
        .await
        .ok_or_else(|| format!("隧道监听 {tunnel_port} 起连续 10 个端口均失败"))?;
    let bl = bind_fallback(bridge_port, "虚拟桥接")
        .await
        .ok_or_else(|| format!("虚拟桥接监听 {bridge_port} 起连续 10 个端口均失败"))?;
    let actual = bl.local_addr().map_err(|e| e.to_string())?.port();
    *BRIDGE_URL.lock().unwrap() = format!("http://127.0.0.1:{actual}");
    let tport = tl.local_addr().map_err(|e| e.to_string())?.port();
    STARTED.store(true, Ordering::SeqCst);
    let h1 = tokio::spawn(async move { tunnel_accept_loop(tl).await; });
    let h2 = tokio::spawn(async move { bridge_accept_loop(bl).await; });
    HANDLES.lock().unwrap().extend([h1, h2]);
    log::info!("[桥接] 隧道服务就绪 (tunnel:{tport}, virtual:{actual})");
    Ok(actual)
}

/// 从 port 起逐个尝试绑定, 最多 10 个 (Hyper-V 保留段/占用端口兜底)
async fn bind_fallback(port: u16, label: &str) -> Option<TcpListener> {
    for p in port..port.saturating_add(10) {
        match TcpListener::bind(("127.0.0.1", p)).await {
            Ok(l) => {
                if p != port {
                    log::warn!("[桥接] {label} 端口 {port} 不可用, 回退 {p}");
                }
                return Some(l);
            }
            Err(e) => log::warn!("[桥接] {label} 端口 {p} 绑定失败: {e}"),
        }
    }
    None
}

/// Phase 0: 等待隧道注册
pub async fn wait_registration(timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if is_active() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    is_active()
}

pub fn is_active() -> bool {
    ACTIVE.lock().unwrap().is_some()
}

/// 应用退出: 关监听 + 断隧道 + 清 pending
pub async fn shutdown_all() {
    if !STARTED.swap(false, Ordering::SeqCst) {
        return;
    }
    NOTIFY.notify_waiters();
    for h in HANDLES.lock().unwrap().drain(..) {
        h.abort();
    }
    if let Some(s) = ACTIVE.lock().unwrap().take() {
        s.fail_all_pending("shutdown");
    }
    log::info!("[桥接] 隧道服务已关闭");
}

async fn tunnel_accept_loop(listener: TcpListener) {
    loop {
        let (stream, _) = tokio::select! {
            _ = NOTIFY.notified() => return,
            r = listener.accept() => match r { Ok(x) => x, Err(_) => return },
        };
        if is_active() {
            // 先到先得: 回 busy 帧后关闭
            let mut s = stream;
            let _ = s.write_all(&encode_frame(0, b"{\"err\":\"busy\"}")).await;
            continue;
        }
        let mut s = stream;
        match read_frame(&mut s).await {
            Some(f) if f.id == 0 => {
                if let Some((device, _apk)) = parse_register(&f.payload) {
                    let (rd, wr) = s.into_split();
                    let (tx, rx) = mpsc::channel::<Frame>(64);
                    let session = BridgeSession::new(device.clone(), tx, Limits::default());
                    *ACTIVE.lock().unwrap() = Some(session.clone());
                    tokio::spawn(writer_task(wr, rx));
                    tokio::spawn(reader_task(rd, session));
                    let url = BRIDGE_URL.lock().unwrap().clone();
                    super::set_effective(&url, super::EFFECTIVE_TUNNEL);
                    // Starting→Ready 由 ensure_ready_with 收尾; Failed/Idle 时隧道拨入即自愈
                    if super::status() != super::BridgeStatus::Starting {
                        super::set_status(super::BridgeStatus::Ready);
                    }
                    log::info!("[桥接] 隧道注册成功: {device}");
                }
            }
            _ => { /* 首帧不是注册/读取失败: 丢弃 */ }
        }
    }
}

async fn writer_task(mut wr: tokio::net::tcp::OwnedWriteHalf, mut rx: mpsc::Receiver<Frame>) {
    while let Some(f) = rx.recv().await {
        if write_frame(&mut wr, f.id, &f.payload).await.is_err() {
            break;
        }
    }
}

async fn reader_task(mut rd: tokio::net::tcp::OwnedReadHalf, session: Arc<BridgeSession>) {
    loop {
        match read_frame(&mut rd).await {
            Some(f) => session.deliver(f.id, f.payload),
            None => break,
        }
    }
    // 隧道断开: 所有 pending 立即失败 (方案 §45: 不能永久挂起)
    session.fail_all_pending("隧道断开");
    *ACTIVE.lock().unwrap() = None;
    log::warn!("[桥接] 隧道断开, 等待 APK 重拨");
}

async fn bridge_accept_loop(listener: TcpListener) {
    loop {
        let (stream, _) = tokio::select! {
            _ = NOTIFY.notified() => return,
            r = listener.accept() => match r { Ok(x) => x, Err(_) => return },
        };
        tokio::spawn(handle_bridge_client(stream));
    }
}

async fn handle_bridge_client(mut stream: TcpStream) {
    let Some(raw) = read_http_request(&mut stream).await else { return };
    let session = ACTIVE.lock().unwrap().clone();
    let Some(session) = session else { return }; // 无隧道: 直接关连接 → reqwest 传输错误
    let kind = RequestKind::from_path(request_path(&raw));
    let body = match session.request(kind, raw).await {
        Ok(b) => b,
        Err(e) => e.to_err_json().into_bytes(),
    };
    write_http_response(&mut stream, &body).await;
}

/// 请求行路径 ("POST /search HTTP/1.1" → "/search")
fn request_path(raw: &[u8]) -> &str {
    let head = std::str::from_utf8(raw).unwrap_or("");
    let line = head.lines().next().unwrap_or("");
    line.split_whitespace().nth(1).unwrap_or("/")
}

/// 写 HTTP 响应头+体并关闭 (本任务保持 Connection: close; Task 3 改 keep-alive)
async fn write_http_response(stream: &mut TcpStream, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(body).await;
    let _ = stream.shutdown().await;
}

/// 读完整 HTTP 请求 (头 + Content-Length body); 返回原始字节
async fn read_http_request(s: &mut TcpStream) -> Option<Vec<u8>> {
    let mut buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; 4096];
    let header_end;
    loop {
        let n = s.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            header_end = pos + 4;
            break;
        }
        if buf.len() > 256 * 1024 {
            return None;
        }
    }
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let content_length = head
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            if k.trim().eq_ignore_ascii_case("content-length") {
                v.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    while buf.len() < header_end + content_length {
        let n = s.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    Some(buf)
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

// ---- 帧异步 IO (tokio) ----

pub(crate) async fn write_frame<W: AsyncWriteExt + Unpin>(s: &mut W, id: u32, payload: &[u8]) -> std::io::Result<()> {
    s.write_all(&encode_frame(id, payload)).await
}

pub(crate) async fn read_frame<S: AsyncReadExt + Unpin>(s: &mut S) -> Option<Frame> {
    let mut head = [0u8; 8];
    s.read_exact(&mut head).await.ok()?;
    let id = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
    let len = u32::from_be_bytes([head[4], head[5], head[6], head[7]]) as usize;
    if len > 8 * 1024 * 1024 {
        return None;
    }
    let mut payload = vec![0u8; len];
    if len > 0 {
        s.read_exact(&mut payload).await.ok()?;
    }
    Some(Frame { id, payload })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ACTIVE/STARTED/HANDLES/BRIDGE_URL 是进程级全局, 端到端测试并行会互踩
    /// (一方注册使另一方被 busy 拒绝; 一方 shutdown 掐断另一方的 accept 循环);
    /// 触碰全局态的用例全部持此锁串行执行 (同 gateway.rs::sessions_lock 模式)。
    fn tunnel_lock() -> std::sync::MutexGuard<'static, ()> {
        static TEST_LOCK: StdMutex<()> = StdMutex::new(());
        TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn frame_roundtrip() {
        let raw = encode_frame(7, b"{\"code\":200}");
        assert_eq!(&raw[..4], &[0, 0, 0, 7]); // id BE
        assert_eq!(&raw[4..8], &[0, 0, 0, 12]); // len BE = 12
        let mut buf = raw.clone();
        let (f, used) = parse_frame(&mut buf).unwrap();
        assert_eq!(used, raw.len());
        assert_eq!(f.id, 7);
        assert_eq!(f.payload, b"{\"code\":200}");
    }

    #[test]
    fn frame_partial_buffer_returns_none() {
        let raw = encode_frame(1, b"hello");
        let mut buf = raw[..raw.len() - 2].to_vec(); // 缺 2 字节
        assert!(parse_frame(&mut buf).is_none());
        buf.extend_from_slice(&raw[raw.len() - 2..]);
        let (f, used) = parse_frame(&mut buf).unwrap();
        assert_eq!(f.payload, b"hello");
        assert_eq!(used, raw.len());
    }

    #[test]
    fn parse_register_json() {
        let (device, apk) = parse_register(br#"{"device":"MuMu","apk":"1.1"}"#).unwrap();
        assert_eq!(device, "MuMu");
        assert_eq!(apk, "1.1");
        assert!(parse_register(b"garbage").is_none());
    }

    #[tokio::test]
    async fn tunnel_end_to_end_and_reject_second() {
        let _g = tunnel_lock();
        let tl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let tunnel_port = tl.local_addr().unwrap().port();
        let bridge_port = bl.local_addr().unwrap().port();
        serve(tl, bl, format!("http://127.0.0.1:{bridge_port}")).await;

        // mock APK: 注册 + 应答一帧 /health
        let mock = tokio::spawn(async move {
            let mut s = tokio::net::TcpStream::connect(("127.0.0.1", tunnel_port)).await.unwrap();
            write_frame(&mut s, 0, br#"{"device":"MockMu","apk":"1.1"}"#).await.unwrap();
            let f = read_frame(&mut s).await.unwrap();
            assert!(f.payload.starts_with(b"POST /health"));
            write_frame(&mut s, f.id, b"{\"code\":200,\"err\":\"ok\",\"data\":\"{}\"}").await.unwrap();
            // 第二台拨入应被拒绝: 收到 busy 错误帧
            let mut s2 = tokio::net::TcpStream::connect(("127.0.0.1", tunnel_port)).await.unwrap();
            let f2 = read_frame(&mut s2).await.unwrap();
            assert!(String::from_utf8_lossy(&f2.payload).contains("busy"));
        });

        assert!(wait_registration(std::time::Duration::from_secs(3)).await);
        assert!(is_active());

        // spider 层视角: 连虚拟桥接发 /health, 收 200 + JSON
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", bridge_port)).await.unwrap();
        s.write_all(b"POST /health HTTP/1.1\r\nContent-Length: 0\r\n\r\n").await.unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.unwrap();
        let text = String::from_utf8_lossy(&buf);
        assert!(text.starts_with("HTTP/1.1 200 OK"), "{text}");
        assert!(text.contains("\"code\":200"), "{text}");

        mock.await.unwrap();
        // 清理全局态, 不污染其他用例
        shutdown_all().await;
        crate::bridge::reset_effective();
        crate::bridge::set_status(crate::bridge::BridgeStatus::Idle);
    }

    #[tokio::test]
    async fn bridge_timeout_returns_error_json() {
        let _g = tunnel_lock();
        // mock APK 注册后不应答 → 桌面侧 HEALTH_TIMEOUT(2s) 内返回业务错误 JSON, 不再挂死 (方案 §18)
        let tl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let tunnel_port = tl.local_addr().unwrap().port();
        let bridge_port = bl.local_addr().unwrap().port();
        serve(tl, bl, format!("http://127.0.0.1:{bridge_port}")).await;

        let mock = tokio::spawn(async move {
            let mut s = tokio::net::TcpStream::connect(("127.0.0.1", tunnel_port)).await.unwrap();
            write_frame(&mut s, 0, br#"{"device":"SilentMu","apk":"1.1"}"#).await.unwrap();
            // 不读取、不应答后续帧
            tokio::time::sleep(std::time::Duration::from_secs(4)).await;
        });

        assert!(wait_registration(std::time::Duration::from_secs(3)).await);

        let started = std::time::Instant::now();
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", bridge_port)).await.unwrap();
        s.write_all(b"POST /health HTTP/1.1\r\nContent-Length: 0\r\n\r\n").await.unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.unwrap();
        let text = String::from_utf8_lossy(&buf);
        assert!(started.elapsed() < std::time::Duration::from_secs(4), "超时必须快速失败");
        assert!(text.contains("\"code\":504"), "{text}");

        mock.abort();
        shutdown_all().await;
        crate::bridge::reset_effective();
        crate::bridge::set_status(crate::bridge::BridgeStatus::Idle);
    }
}
