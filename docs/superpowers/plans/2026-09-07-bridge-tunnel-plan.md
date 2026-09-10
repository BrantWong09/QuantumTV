# 桥接 v2（APK 主动注册 + TCP 隧道）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 桥接反转为 APK 主动注册：模拟器内 BridgeService 主动拨桌面端建持久隧道，桌面端删除 adb 扫描（Phase B）与 AVD 拉起（Phase C），spider 层零改动。

**Architecture:** APK 新增 TunnelClient（拨网关:18099，帧多路复用）；桌面端新增 `bridge/tunnel.rs`（TunnelServer + VirtualBridge：18099 收注册、18090 起虚拟桥接转发 spider 请求进隧道）；`startup_steps` 变为 Phase 0（等注册 6s）→ Phase A（远程直连）→ Failed；路由逻辑 APK 侧抽 `routeRequest` 供 Socket/隧道双模式共用。

**Tech Stack:** Rust (tokio 异步 TCP)、Java (Socket/DataInputStream)、React/TS；无新依赖。

**Spec:** docs/superpowers/specs/2026-09-07-bridge-tunnel-design.md

## Global Constraints

- 帧格式: `[u32 BE id][u32 BE len][payload]`；首帧 id=0 = 注册 JSON `{"device":"<Build.MODEL>","apk":"1.1"}`
- 桌面隧道监听 `127.0.0.1:18099`（env `QUANTUMTV_TUNNEL_PORT` 可覆盖）；虚拟桥接监听 `127.0.0.1:{host_port}`（默认 18090，env `QUANTUMTV_ADB_HOST_PORT` 沿用）
- spider 层 `effective_url()` 语义不变（仍 `http://127.0.0.1:{host_port}`）；新增 `EFFECTIVE_TUNNEL=4`、status mode `"tunnel"`
- APK 断线重拨: 5s 起指数退避，上限 30s；桌面端 pending 请求在隧道断开时立即以空 payload 失败
- Phase B/C 相关符号全部删除（清单见 Task 3）；`launch_bridge_activity`/`run_adb`/`all_device_serials`/`adb_path`/`default_sdk_root` 保留（WebView 登录兜底仍用 adb）
- 多台设备同时拨入: 先到先得，后者回 `{"err":"busy"}` 错误帧后关闭
- 无新 crate/npm 依赖；git 提交只含代码文件（docs/ 被 .gitignore）
- 注意: 跑 `cargo test` 时若桌面应用在运行，隧道单测用临时端口不受影响（禁止在测试中绑定 18090/18099 固定端口）

---

### Task 1: tunnel.rs 帧编解码 + 注册解析（TDD）

**Files:**
- Create: `crates/core/src/bridge/tunnel.rs`
- Modify: `crates/core/src/bridge/mod.rs`（顶部加 `pub mod tunnel;`）

**Interfaces:**
- Produces: `encode_frame(id: u32, payload: &[u8]) -> Vec<u8>`、`Frame { id: u32, payload: Vec<u8> }`、`parse_frame(buf: &mut Vec<u8>) -> Option<(Frame, usize)>`（缓冲区前缀解析，返回帧与消耗字节数）、`parse_register(payload: &[u8]) -> Option<(String, String)>`（device, apk）

- [ ] **Step 1: 写失败测试**

创建 `crates/core/src/bridge/tunnel.rs`：

```rust
//! 桥接隧道: APK 主动注册 + 帧多路复用
//!
//! 帧格式 [u32 BE id][u32 BE len][payload]; APK 首帧 id=0 = 注册 JSON;
//! 之后桌面→APK 帧 payload = 完整 HTTP 请求字节, APK→桌面帧 = HTTP 响应体字节,
//! 按 id 配对。VirtualBridge 在 127.0.0.1:{host_port} 接 spider 层请求转进隧道。

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip() {
        let raw = encode_frame(7, b"{\"code\":200}");
        assert_eq!(&raw[..4], &[0, 0, 0, 7]); // id BE
        assert_eq!(&raw[4..8], &[0, 0, 0, 11]); // len BE = 11
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
}
```

`crates/core/src/bridge/mod.rs` 模块声明区（`pub mod playback;` 后）加：

```rust
pub mod tunnel;
```

- [ ] **Step 2: 跑测试确认编译失败**

Run: `cargo test -p quantumtv-core --lib bridge::tunnel`
Expected: 编译错误（encode_frame/parse_frame/parse_register 未定义）

- [ ] **Step 3: 实现纯函数**

在 tunnel.rs 顶部（tests 模块之前）加：

```rust
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
```

注意：tunnel.rs 需要 `serde_json`（core 已依赖）。

- [ ] **Step 4: 跑测试确认全绿**

Run: `cargo test -p quantumtv-core --lib bridge::tunnel`
Expected: 3 PASS

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/bridge/tunnel.rs crates/core/src/bridge/mod.rs
git commit -m "feat(bridge): 隧道帧编解码与注册帧解析"
```

---

### Task 2: tunnel.rs 服务层（TunnelServer + VirtualBridge，TDD）

**Files:**
- Modify: `crates/core/src/bridge/tunnel.rs`

**Interfaces:**
- Consumes: Task 1 帧/注册解析；`super::set_effective` / `super::set_status` / `super::BridgeStatus` / `super::EFFECTIVE_TUNNEL`（Task 3 加常量，本任务先用字面量 4 并在 Task 3 改引常量——**直接在本任务加 `pub const EFFECTIVE_TUNNEL: u8 = 4;` 到 mod.rs 常量区**）
- Produces: `pub async fn ensure_started(tunnel_port: u16, bridge_port: u16, bridge_url: String) -> Result<(), String>`（幂等）、`pub async fn wait_registration(timeout: Duration) -> bool`、`pub fn is_active() -> bool`、`pub async fn shutdown_all()`

- [ ] **Step 1: 写失败的集成测试（临时端口，可与应用共存）**

在 tunnel.rs 的 tests 模块追加：

```rust
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
            // 第二台拨入应被拒绝: 收到错误帧后对端关闭
            let mut s2 = tokio::net::TcpStream::connect(("127.0.0.1", tunnel_port)).await.unwrap();
            let f2 = read_frame(&mut s2).await.unwrap();
            assert!(String::from_utf8_lossy(&f2.payload).contains("busy"));
        });

        assert!(wait_registration(Duration::from_secs(3)).await);
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
        super::reset_effective();
        super::set_status(super::BridgeStatus::Idle);
    }
```

同时在文件顶部补 use（tests 模块内）：`use std::time::Duration;` 与外层 `use tokio::io::{AsyncReadExt, AsyncWriteExt};` 等按实现需要。

- [ ] **Step 2: 跑测试确认编译失败**

Run: `cargo test -p quantumtv-core --lib bridge::tunnel`
Expected: serve/write_frame/read_frame/wait_registration/is_active/shutdown_all 未定义

- [ ] **Step 3: 实现服务层**

在 tunnel.rs 追加：

```rust
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex as StdMutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, Mutex, Notify};

pub const DEFAULT_TUNNEL_PORT: u16 = 18099;

struct TunnelConn {
    device: String,
    writer: mpsc::Sender<(u32, Vec<u8>)>,
}

static ACTIVE: StdMutex<Option<TunnelConn>> = StdMutex::new(None);
static PENDING: StdMutex<HashMap<u32, oneshot::Sender<Vec<u8>>>> = StdMutex::new(HashMap::new());
static NEXT_ID: AtomicU32 = AtomicU32::new(1);
static STARTED: AtomicBool = AtomicBool::new(false);
static NOTIFY: Notify = Notify::new();
static HANDLES: StdMutex<Vec<tokio::task::JoinHandle<()>>> = StdMutex::new(Vec::new());

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

async fn tunnel_accept_loop(mut listener: TcpListener, bridge_url: String) {
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
                    let (rd, mut wr) = s.into_split();
                    let (tx, rx) = mpsc::channel::<(u32, Vec<u8>)>(64);
                    *ACTIVE.lock().unwrap() = Some(TunnelConn { device, writer: Mutex::new(tx) });
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

async fn bridge_accept_loop(mut listener: TcpListener) {
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
```

同时 mod.rs 常量区（`pub const EFFECTIVE_AVD: u8 = 3;` 后）加：

```rust
pub const EFFECTIVE_TUNNEL: u8 = 4;
```

- [ ] **Step 4: 跑测试确认全绿**

Run: `cargo test -p quantumtv-core --lib bridge::tunnel`
Expected: 4 PASS（frame_roundtrip、frame_partial、parse_register、tunnel_end_to_end）

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/bridge/tunnel.rs crates/core/src/bridge/mod.rs
git commit -m "feat(bridge): 隧道服务层 (TunnelServer/VirtualBridge/帧mux)"
```

---

### Task 3: mod.rs 瀑布重写 + Phase B/C 删除

**Files:**
- Modify: `crates/core/src/bridge/mod.rs`（全局删除指定符号；重写 startup_steps/shutdown；BridgeConfig 收缩）

**Interfaces:**
- Consumes: `tunnel::{ensure_started, wait_registration, is_active, shutdown_all}`
- Produces: `BridgeConfig { enabled, host_port, bridge_url, tunnel_port, url_override, remote_url }`（字段收缩）；`startup_steps` 新瀑布；`get_bridge_status` 可用 `"tunnel"` mode

- [ ] **Step 1: BridgeConfig 收缩 + ENV_KEYS 清理**

ENV_KEYS 改为：

```rust
const ENV_KEYS: [&str; 5] = [
    "QUANTUMTV_BRIDGE_ENABLED",
    "QUANTUMTV_ADB_HOST_PORT",
    "QUANTUMTV_TUNNEL_PORT",
    "QUANTUMTV_BRIDGE_URL",
    "QUANTUMTV_BRIDGE_REMOTE_URL",
];
```

BridgeConfig 结构体改为（同时改 from_map 与文档注释）：

```rust
pub struct BridgeConfig {
    pub enabled: bool,
    pub host_port: u16,
    pub bridge_url: String,
    pub tunnel_port: u16,
    /// env QUANTUMTV_BRIDGE_URL，Phase A 首候选
    pub url_override: Option<String>,
    /// env/UI 远程桥接地址，Phase A 次候选
    pub remote_url: Option<String>,
}
```

from_map 改为：

```rust
    pub fn from_map(m: &HashMap<String, String>) -> BridgeConfig {
        let enabled = m.get("QUANTUMTV_BRIDGE_ENABLED").map(|v| v != "0").unwrap_or(true);
        let host_port = m.get("QUANTUMTV_ADB_HOST_PORT").and_then(|v| v.parse().ok()).unwrap_or(18080);
        let tunnel_port = m.get("QUANTUMTV_TUNNEL_PORT").and_then(|v| v.parse().ok()).unwrap_or(crate::bridge::tunnel::DEFAULT_TUNNEL_PORT);
        let url_override = m.get("QUANTUMTV_BRIDGE_URL").map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let remote_url = m.get("QUANTUMTV_BRIDGE_REMOTE_URL").map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        BridgeConfig { enabled, host_port, bridge_url: format!("http://127.0.0.1:{host_port}"), tunnel_port, url_override, remote_url }
    }
```

删除字段/函数（连同其测试）: `avd`、`sdk_root`、`apk_path`、`adb_addresses`、`auto_scan`、`adb_path` 之外的 sdk 相关（`default_sdk_root` 保留——launch_bridge_activity 用）、`emulator_path`、`emulator_args`、`forward_args`、`emulator_serial`、`emulator_serials`、`SCAN_PORTS`、`connectable_serials`、`has_emulator_device`、`parse_avd_name`、`phase_b_addresses`、`spawn_emulator`、`wait_boot`、`ensure_apk_installed`、`start_bridge_service`、`forward_cmd_args`、`forward_port`、`wait_serial_device`、`try_adb_device`、`ensure_avd_bridge`、`wait_for_avd_serial`、`serial_matches_avd`、`EMU_CHILD`、`STARTED_SERIAL`、`WE_STARTED`、`we_started`、`set_we_started`。
保留: `run_adb`、`all_device_serials`、`adb_path`、`default_sdk_root`、`parse_health_body`、`probe_health`、`remote_candidates`（launch_bridge_activity 与 Phase A 仍用）。

- [ ] **Step 2: 重写 startup_steps（Phase 0/A）**

```rust
async fn startup_steps(cfg: &BridgeConfig) -> Result<(), String> {
    // Phase 0: 隧道注册 (APK 主动拨入; 等一个拨号周期)
    if tunnel::wait_registration(std::time::Duration::from_secs(6)).await {
        set_effective(&cfg.bridge_url, EFFECTIVE_TUNNEL);
        log::info!("[桥接] 隧道就绪: {}", cfg.bridge_url);
        return Ok(());
    }

    // Phase A: 远程真机直连
    for cand in remote_candidates(cfg.url_override.as_deref(), cfg.remote_url.as_deref()) {
        if probe_health(&cand).await {
            set_effective(&cand, EFFECTIVE_REMOTE);
            log::info!("[桥接] 远程桥接就绪: {}", cand);
            return Ok(());
        }
    }

    if tunnel::is_active() {
        // 等待期间隧道拨入: 以隧道为准
        set_effective(&cfg.bridge_url, EFFECTIVE_TUNNEL);
        return Ok(());
    }
    Err("未发现桥接设备：请将 bridge.apk 拖入模拟器安装并保持其运行，或在局域网真机上填写远程桥接地址".to_string())
}
```

ensure_ready_with 在 `try_begin_start()` 成功后、startup_steps 之前加：

```rust
    tunnel::ensure_started(cfg.tunnel_port, cfg.host_port, cfg.bridge_url.clone()).await?;
```

- [ ] **Step 3: shutdown 收缩**

```rust
pub async fn shutdown() {
    tunnel::shutdown_all().await;
    reset_effective();
    set_status(BridgeStatus::Idle);
    log::info!("[桥接] 已关闭");
}
```

- [ ] **Step 4: spawn_retry（commands/bridge.rs:96）收缩**

删除其中 `if quantumtv_core::bridge::we_started() { quantumtv_core::bridge::shutdown().await; }` 块（隧道是常驻设施，重试不断隧道），保留 reset_effective + ensure_ready_with。

- [ ] **Step 5: 测试适配**

mod.rs tests 模块：删除引用已删符号的用例（`paths_and_args`、`emulator_args_dns_env_override`、scan/serial/avd 相关、`phase_b_addresses` 相关）；`from_map` 用例改为断言 `host_port`/`tunnel_port`/`bridge_url`；`effective_url_roundtrip_and_reset` 保留。
命令: `cargo test -p quantumtv-core`
Expected: 全绿（无编译错误）

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/bridge/
git commit -m "refactor(bridge): 移除 adb/AVD 瀑布, Phase 0 隧道注册优先"
```

---

### Task 4: 命令层 + UI 收缩

**Files:**
- Modify: `src-tauri/src/commands/bridge.rs`
- Modify: `src/components/BridgeSettings.tsx`

**Interfaces:**
- Produces: `BridgeSettingsDto { remote_url: String }`（serde(default) 兼容旧数据）；mode `"tunnel"`

- [ ] **Step 1: commands/bridge.rs 收缩**

- `BridgeSettingsDto` 删 `adb_addresses`/`auto_scan` 字段，Default 改 `Self { remote_url: String::new() }`
- `build_bridge_map`：删 `QUANTUMTV_BRIDGE_AVD/SDK/ADB_ADDRESSES/AUTO_SCAN` 的 env 收集与 UI insert（保留 REMOTE_URL insert；env 键清单同步为 Task 3 的 ENV_KEYS 五键）
- `validate_settings`：删除 adb 地址校验循环（remote_url 校验保留）
- `get_bridge_status` mode match 改为：

```rust
    let mode = match quantumtv_core::bridge::effective_kind() {
        quantumtv_core::bridge::EFFECTIVE_TUNNEL => "tunnel",
        quantumtv_core::bridge::EFFECTIVE_REMOTE => "remote",
        _ => "none",
    };
```

- [ ] **Step 2: BridgeSettings.tsx 收缩**

- `BridgeSettingsDto` interface 改 `{ remote_url: string }`；state 初始值同步
- `BridgeStatusDto.mode` 类型改 `'none' | 'remote' | 'tunnel'`；`MODE_LABEL` 改 `{ none: '无', remote: '远程直连', tunnel: '模拟器隧道' }`
- 删除「adb 连接地址」textarea 与「自动扫描」checkbox 两块
- 在远程桥接地址块之前插入拖装提示块：

```tsx
      {/* 模拟器隧道 */}
      <div className='rounded-lg border border-gray-200 p-3 text-sm dark:border-gray-700'>
        <div className='font-medium text-gray-900 dark:text-gray-100'>模拟器隧道（推荐）</div>
        <p className='mt-1 text-xs text-gray-500 dark:text-gray-400'>
          将项目内 <code className='rounded bg-gray-100 px-1 dark:bg-gray-800'>android/spider-bridge/out/bridge.apk</code>{' '}
          拖入模拟器窗口安装并保持运行，桥接自动建立（无需 adb）。模拟器重启后服务自启，隧道自动重连。
        </p>
      </div>
```

- Run: `npm run typecheck && npx eslint src/components/BridgeSettings.tsx`
Expected: 通过

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/commands/bridge.rs src/components/BridgeSettings.tsx
git commit -m "refactor(bridge): UI/命令层收缩为远程地址+隧道模式"
```

---

### Task 5: APK TunnelClient + routeRequest 抽取 + 重打包

**Files:**
- Create: `android/spider-bridge/src/com/quantumtv/bridge/TunnelClient.java`
- Modify: `android/spider-bridge/src/com/quantumtv/bridge/BridgeService.java`
- Rebuild: `android/spider-bridge/out/bridge.apk`

**Interfaces:**
- Consumes: `BridgeService.routeRequest`（Task 5 抽出）、`pool`/`detailExecutor` 线程池
- Produces: 桌面隧道注册（18099），Socket:8080 模式保留

- [ ] **Step 1: BridgeService 抽 routeRequest + parseRequest**

handle() 中 if/else 路由链（`/health` 到 `/setCookie`）抽为：

```java
    /** 路由分发 (同步阻塞; doDetail/doPlayerContent 由调用方决定是否投递 detailExecutor) */
    private String routeRequest(String method, String path, String body) {
        if ("/health".equals(path)) {
            return json(200, "ok", "{\"initialized\":" + initialized + "}");
        }
        if ("/init".equals(path) && "POST".equalsIgnoreCase(method)) return doInit(body);
        if ("/search".equals(path) && "POST".equalsIgnoreCase(method)) return doSearch(body);
        if ("/playerContent".equals(path) && "POST".equalsIgnoreCase(method)) return doPlayerContent(body);
        if ("/detail".equals(path) && "POST".equalsIgnoreCase(method)) return doDetail(body);
        if ("/home".equals(path) && "POST".equalsIgnoreCase(method)) return doHome(body);
        if ("/category".equals(path) && "POST".equalsIgnoreCase(method)) return doCategory(body);
        if ("/setCookie".equals(path) && "POST".equalsIgnoreCase(method)) return doSetCookie(body);
        return json(404, "not_found", null);
    }
```

handle(Socket) 改为：detail/playerContent → `detailExecutor.submit(() -> { String r = routeRequest(m,p,b); writeHttp(s, r); s.close(); })`；其余 → `writeHttp(s, routeRequest(m,p,b))`（语义与现状一致）。
新增静态解析（隧道帧 payload → 三元组）：

```java
    /** 隧道帧 payload (原始 HTTP 请求字节) → [method, path, body]; 解析失败返回 null */
    static String[] parseRequest(byte[] raw) {
        try {
            java.io.ByteArrayInputStream in = new java.io.ByteArrayInputStream(raw);
            String line = readLineRaw(in);
            if (line == null) return null;
            String[] parts = line.split(" ");
            if (parts.length < 2) return null;
            int contentLength = 0;
            String h;
            while ((h = readLineRaw(in)) != null && !h.isEmpty()) {
                if (h.toLowerCase().startsWith("content-length:")) {
                    contentLength = Integer.parseInt(h.split(":", 2)[1].trim());
                }
            }
            byte[] body = new byte[contentLength];
            int read = 0;
            while (read < contentLength) {
                int n = in.read(body, read, contentLength - read);
                if (n < 0) break;
                read += n;
            }
            return new String[]{parts[0], parts[1], new String(body, 0, read, java.nio.charset.StandardCharsets.UTF_8)};
        } catch (Exception e) {
            return null;
        }
    }
```

`readLineRaw` 改为 `private static String readLineRaw(java.io.InputStream in) throws Exception`（无实例状态，可直接 static 化）。
onStartCommand 里 `new Thread(this::acceptLoop, "BridgeAccept").start();` 之后加 `TunnelClient.start(this);`。

- [ ] **Step 2: 新建 TunnelClient.java**

```java
package com.quantumtv.bridge;

import android.os.Build;
import android.util.Log;

import java.io.BufferedInputStream;
import java.io.DataInputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.InetSocketAddress;
import java.net.Socket;

/**
 * 桥接隧道客户端: 主动拨宿主网关 18099, 与桌面端建立持久帧隧道。
 * 帧格式 [u32 BE id][u32 BE len][payload]; 首帧 id=0 注册 JSON;
 * 桌面→APK 帧 payload = 完整 HTTP 请求字节; APK 回 [id][len][响应体]。
 * 断线自动重拨: 全部网关失败时 5s 起指数退避, 上限 30s; 成功后复位 5s。
 * 网关候选: 10.0.2.2 (QEMU 系) → ip route default via → 192.168.56.1 (VirtualBox 系)。
 */
public class TunnelClient implements Runnable {
    static final int PORT = 18099;
    private static final String TAG = "BridgeTunnel";
    private final BridgeService svc;
    private volatile boolean running = true;

    private TunnelClient(BridgeService svc) { this.svc = svc; }

    static void start(BridgeService svc) {
        Thread t = new Thread(new TunnelClient(svc), "BridgeTunnel");
        t.setDaemon(true);
        t.start();
    }

    @Override
    public void run() {
        long backoff = 5000;
        while (running) {
            boolean connected = false;
            for (String host : gateways()) {
                if (!running) return;
                try (Socket sock = new Socket()) {
                    sock.connect(new InetSocketAddress(host, PORT), 3000);
                    sock.setTcpNoDelay(true);
                    session(sock);
                    connected = true;
                    break;
                } catch (Exception e) {
                    Log.d(TAG, "dial " + host + " failed: " + e);
                }
            }
            sleep(connected ? 5000 : backoff);
            backoff = connected ? 5000 : Math.min(backoff * 2, 30000);
        }
    }

    private void session(Socket sock) throws Exception {
        OutputStream out = sock.getOutputStream();
        DataInputStream in = new DataInputStream(new BufferedInputStream(sock.getInputStream(), 16 * 1024));
        String reg = "{\"device\":\"" + Build.MODEL + "\",\"apk\":\"1.1\"}";
        writeFrame(out, 0, reg.getBytes(java.nio.charset.StandardCharsets.UTF_8));
        Log.i(TAG, "tunnel registered to host");
        while (running) {
            int id = in.readInt();
            int len = in.readInt();
            if (len < 0 || len > 8 * 1024 * 1024) break;
            byte[] payload = new byte[len];
            in.readFully(payload);
            final int fid = id;
            final byte[] fpayload = payload;
            Runnable job = () -> {
                try {
                    String[] mpb = BridgeService.parseRequest(fpayload);
                    String resp = mpb == null
                        ? "{\"code\":400,\"err\":\"bad_request\"}"
                        : svc.routeRequest(mpb[0], mpb[1], mpb[2]);
                    synchronized (out) {
                        writeFrame(out, fid, resp.getBytes(java.nio.charset.StandardCharsets.UTF_8));
                    }
                } catch (Exception e) {
                    Log.e(TAG, "frame handle: " + e);
                }
            };
            String path = mpbPath(fpayload);
            if ("/detail".equals(path) || "/playerContent".equals(path)) {
                svc.detailExecutor.submit(job);
            } else {
                svc.pool.submit(job);
            }
        }
        Log.w(TAG, "tunnel closed by host");
    }

    private static String mpbPath(byte[] raw) {
        try {
            java.io.ByteArrayInputStream in = new java.io.ByteArrayInputStream(raw);
            byte[] buf = new byte[512];
            int n = 0, b;
            while ((b = in.read()) != -1 && b != '\n' && n < buf.length) buf[n++] = (byte) b;
            String line = new String(buf, 0, n, java.nio.charset.StandardCharsets.UTF_8).trim();
            String[] parts = line.split(" ");
            return parts.length >= 2 ? parts[1] : "";
        } catch (Exception e) {
            return "";
        }
    }

    private static void writeFrame(OutputStream out, int id, byte[] payload) throws Exception {
        out.write(intBytes(id));
        out.write(intBytes(payload.length));
        out.write(payload);
        out.flush();
    }

    private static byte[] intBytes(int v) {
        return new byte[]{(byte) (v >>> 24), (byte) (v >>> 16), (byte) (v >>> 8), (byte) v};
    }

    private static String[] gateways() {
        java.util.LinkedHashSet<String> list = new java.util.LinkedHashSet<>();
        list.add("10.0.2.2");
        try {
            java.util.Scanner sc = new java.util.Scanner(Runtime.getRuntime().exec(new String[]{"sh", "-c", "ip route show table wlan0"}).getInputStream());
            while (sc.hasNextLine()) {
                String line = sc.nextLine();
                if (line.startsWith("default")) {
                    String[] p = line.split("\\s+");
                    for (int i = 0; i < p.length - 1; i++) {
                        if ("via".equals(p[i])) list.add(p[i + 1]);
                    }
                }
            }
            sc.close();
        } catch (Exception ignored) {
        }
        list.add("192.168.56.1");
        return list.toArray(new String[0]);
    }

    private static void sleep(long ms) {
        try { Thread.sleep(ms); } catch (InterruptedException ignored) { }
    }
}
```

- [ ] **Step 3: 重打包 + 拖装**

Run: `pwsh -NoProfile -File android/spider-bridge/build.ps1`
Expected: `BUILD OK`。用户把 `android/spider-bridge/out/bridge.apk` 拖入 MuMu 窗口安装（或开发期 `adb install -r`）。

- [ ] **Step 4: Commit**

```bash
git add android/spider-bridge/src/
git commit -m "feat(bridge): APK 隧道客户端主动注册 + routeRequest 双模式抽取"
```

---

### Task 6: ADR 0003 + E2E 验证（HITL）

**Files:**
- Create: `docs/adr/0003-bridge-tunnel.md`（docs 不入库，仅本地）
- 无代码改动

- [ ] **Step 1: 写 ADR 0003**

内容要点：背景（adb 打架/forward 冲突/AVD 兜底弊端）→ 决策（APK 主动注册隧道，Phase 0/A 保留，Phase B/C 删除）→ 后果（APK 手动拖装、多设备先到先得、局域网真机走 Phase A 不变）。

- [ ] **Step 2: E2E 冒烟（用户操作 + 观察）**

1. 确认 MuMu 内 BridgeService 运行（新 APK）
2. 启动应用（不带任何 adb 操作）→ 管理页桥接状态应显示 `就绪 · 模式: 模拟器隧道`
3. 搜索点播 → detail/playerContent 走隧道出直链
4. 扫码登录 → setCookie 走隧道生效
5. 在 MuMu 里强制停止 BridgeService 再启动 → 5s 内隧道自愈
6. 退出应用 → 隧道关闭

- [ ] **Step 3: 收尾检查**

```bash
grep -rn "SCAN_PORTS\|spawn_emulator\|wait_boot\|ensure_avd_bridge\|forward_port" crates/core/src/bridge/ | grep -v tunnel.rs
```

Expected: 无输出（残留引用为零）；`cargo test -p quantumtv-core` 全绿。
