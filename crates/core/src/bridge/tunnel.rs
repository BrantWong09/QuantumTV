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

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{LazyLock, Mutex as StdMutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, Notify};

pub const DEFAULT_TUNNEL_PORT: u16 = 18099;

struct TunnelConn {
    #[allow(dead_code)]
    device: String,
    writer: mpsc::Sender<(u32, Vec<u8>)>,
}

/// 测试入口: 用现成 listener 启动隧道/虚拟桥接 (与 ensure_started 共用 accept 循环)
#[cfg(test)]
async fn serve(tunnel: TcpListener, bridge: TcpListener, bridge_url: String) {
    STARTED.store(true, Ordering::SeqCst);
    let h1 = tokio::spawn(async move { tunnel_accept_loop(tunnel, bridge_url).await; });
    let h2 = tokio::spawn(async move { bridge_accept_loop(bridge).await; });
    HANDLES.lock().unwrap().extend([h1, h2]);
}

static ACTIVE: LazyLock<StdMutex<Option<TunnelConn>>> = LazyLock::new(|| StdMutex::new(None));
static PENDING: LazyLock<StdMutex<HashMap<u32, oneshot::Sender<Vec<u8>>>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));
static NEXT_ID: AtomicU32 = AtomicU32::new(1);
static STARTED: AtomicBool = AtomicBool::new(false);
static NOTIFY: LazyLock<Notify> = LazyLock::new(|| Notify::new());
static HANDLES: LazyLock<StdMutex<Vec<tokio::task::JoinHandle<()>>>> =
    LazyLock::new(|| StdMutex::new(Vec::new()));

/// 幂等启动两个常驻监听; 由 ensure_ready_with 在瀑布前调用
pub async fn ensure_started(tunnel_port: u16, bridge_port: u16, bridge_url: String) -> Result<(), String> {
    if STARTED.load(Ordering::SeqCst) {
        return Ok(());
    }
    let tl = TcpListener::bind(("127.0.0.1", tunnel_port))
        .await
        .map_err(|e| format!("隧道监听 {tunnel_port} 失败: {e}"))?;
    let bl = TcpListener::bind(("127.0.0.1", bridge_port))
        .await
        .map_err(|e| format!("虚拟桥接监听 {bridge_port} 失败: {e}"))?;
    STARTED.store(true, Ordering::SeqCst);
    let url = bridge_url.clone();
    let h1 = tokio::spawn(async move { tunnel_accept_loop(tl, url).await; });
    let h2 = tokio::spawn(async move { bridge_accept_loop(bl).await; });
    HANDLES.lock().unwrap().extend([h1, h2]);
    log::info!("[桥接] 隧道服务就绪 (tunnel:{tunnel_port}, virtual:{bridge_port})");
    Ok(())
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
    *ACTIVE.lock().unwrap() = None;
    for (_, tx) in PENDING.lock().unwrap().drain() {
        let _ = tx.send(Vec::new());
    }
    log::info!("[桥接] 隧道服务已关闭");
}

async fn tunnel_accept_loop(listener: TcpListener, bridge_url: String) {
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
                    let (tx, rx) = mpsc::channel::<(u32, Vec<u8>)>(64);
                    *ACTIVE.lock().unwrap() = Some(TunnelConn { device: device.clone(), writer: tx });
                    tokio::spawn(writer_task(wr, rx));
                    tokio::spawn(reader_task(rd));
                    super::set_effective(&bridge_url, super::EFFECTIVE_TUNNEL);
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

async fn writer_task(mut wr: tokio::net::tcp::OwnedWriteHalf, mut rx: mpsc::Receiver<(u32, Vec<u8>)>) {
    while let Some((id, payload)) = rx.recv().await {
        if write_frame(&mut wr, id, &payload).await.is_err() {
            break;
        }
    }
}

async fn reader_task(mut rd: tokio::net::tcp::OwnedReadHalf) {
    loop {
        match read_frame(&mut rd).await {
            Some(f) => {
                if let Some(tx) = PENDING.lock().unwrap().remove(&f.id) {
                    let _ = tx.send(f.payload);
                }
            }
            None => break,
        }
    }
    // 隧道断开: 所有 pending 立即失败 (空 payload = 传输层死亡)
    for (_, tx) in PENDING.lock().unwrap().drain() {
        let _ = tx.send(Vec::new());
    }
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
    let writer_tx = {
        let guard = ACTIVE.lock().unwrap();
        match guard.as_ref() {
            Some(c) => c.writer.clone(),
            None => return, // 无隧道: 直接关连接 → reqwest 传输错误
        }
    };
    let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = oneshot::channel();
    PENDING.lock().unwrap().insert(id, tx);
    if writer_tx.send((id, raw)).await.is_err() {
        PENDING.lock().unwrap().remove(&id);
        return;
    }
    match rx.await {
        Ok(body) if !body.is_empty() => {
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.write_all(&body).await;
            let _ = stream.shutdown().await;
        }
        _ => { /* 隧道死亡: 直接关闭 → reqwest 传输错误 */ }
    }
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
}
