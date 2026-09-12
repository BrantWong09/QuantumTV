# Bridge 与播放性能优化实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 Bridge 从"一次 HTTP 请求一次隧道生命周期"升级为"长期 BridgeSession + 请求复用 + 缓存 + 去重 + 播放优先",并消除播放链路上的重复 Search/Detail/Resolve。

**Architecture:** 桌面侧隧道层新增 `BridgeSession`(按请求类别隔离并发额度与超时,pending 表按 frame id 配对);VirtualBridge HTTP 层改为 keep-alive 会话循环;src-tauri 层用 moka `try_get_with` 同时实现搜索缓存(60s)、详情缓存(10min)、Resolve 缓存(10min)+SingleFlight;搜索代际(generation)贯通到前端做旧结果丢弃;网关层校准连接池参数并补 Range 透传测试。

**Tech Stack:** Rust (tokio 1 full / moka 0.12 / reqwest 0.12 rustls), Tauri v2, Next.js 前端 (src/)。

**Spec:** `docs/QuantumTV Bridge 与播放性能优化实施方案 V2.md`(下称"方案";任务中 §N 指该文档章节)

## Global Constraints

- 禁止方案 §41 列出的修改方式: 不盲目加 timeout/线程/并发,不在 UI 高频 retry,不把视频读进内存,不加无谓 buffer。
- 不做 P3 (zero-copy / 深层协议优化); Frame 编码保持 `encode_frame` 现状,仅统一 writer 通道类型为 `Frame` (§17)。
- 不修改 Android APK (`android/spider-bridge/`): 设备端已有 4 线程池 + 单线程 detailExecutor;桌面端额度(搜索4/详情2/解析1)映射到设备端队列即可 (§22 第一阶段只做 Desktop)。
- 不引入新 Cargo 依赖: `dashmap` 不引入,pending 用 `std::sync::Mutex<HashMap>` 短临界区 (§4 允许);缓存用已有 `moka`。
- 帧协议不变: `[u32 BE id][u32 BE len][payload]`,首帧 id=0 注册 (tunnel.rs 现状),Android 侧零改动。
- spider 层错误判定保持 `{"code":200,...}` / `code!=200` 即错 (spider/mod.rs:460);桌面侧超时/断连返回 `{"code":5xx,"err":"..."}` 让 reqwest 侧快速失败,不再静默挂死。
- `src-tauri` 独立构建(不在 workspace),core 改动用 `cargo test -p quantumtv-core` 验证。
- 验收必须通过 (§45): `npm run lint`、`npm run typecheck`、`npm test`、`cargo test` (workspace + src-tauri)。

## 现状调用链(执行前必读,方案 §44 结论)

- 播放点击链已收口: 前端 `invoke('playback_play_episode')` (src/app/play/page.tsx:260) → `ResolverManager.resolve` 恰好 1 次 → Gateway → mpv,**不含 Search/Detail**。
- 重复点(整改对象):
  - `initialize_player_view` 每次挂载都重拉详情(video.rs:1594 `fetch_detail_item`,无缓存);播放中 preload tick(video.rs:2894 → preload.rs:71)再拉一次。
  - `enrich_first_episode_direct`(video.rs:2666)在详情阶段对第 1 集做 playerContent,与随后点击播放第 1 集的 `playback_play_episode` 重复 Resolve,无缓存无单飞。
  - 搜索代际(SEARCH_GENERATION, video.rs:907)只在 Rust 内部丢弃,前端 search/page.tsx 无 generation 守卫、新搜索前不 abort。
- Bridge 数据面(tunnel.rs): `handle_bridge_client` 一连接一请求、`Connection: close`、oneshot 无超时、无并发上限、PENDING 全局表;隧道本身已多路复用(1 条隧道多帧),缺的是会话化封装+超时+额度+keep-alive。
- 网关(netdisk_proxy.rs/gateway.rs): 已流式回写、Range 双向透传、全局 Client 复用;缺 pool 参数校准与 Range 集成测试。

---

### Task 1: BridgeSession 核心模块

**Files:**
- Create: `crates/core/src/bridge/session.rs`
- Modify: `crates/core/src/bridge/mod.rs` (加 `pub(crate) mod session;`)
- Test: `crates/core/src/bridge/session.rs` 内 `mod tests`

**Interfaces:**
- Consumes: `super::tunnel::Frame { id: u32, payload: Vec<u8> }` (tunnel.rs:9-13)
- Produces (Task 2/3 依赖,签名逐字):
  - `session::RequestKind::{Resolve, Detail, Search, Health}`,`RequestKind::from_path(path: &str) -> RequestKind`
  - `session::Limits { search_inflight, detail_inflight, resolve_inflight: usize; search_timeout, detail_timeout, resolve_timeout, health_timeout: Duration }`,实现 `Default`(=production)
  - `session::RequestError::{NoTunnel, Timeout, TunnelClosed, SendFailed}`,`to_err_json(&self) -> String`
  - `session::BridgeSession::new(device: String, writer: mpsc::Sender<Frame>, limits: Limits) -> Arc<BridgeSession>`
  - `async fn request(self: &Arc<Self>, kind: RequestKind, payload: Vec<u8>) -> Result<Vec<u8>, RequestError>`
  - `fn deliver(self: &Arc<Self>, id: u32, payload: Vec<u8>)`
  - `fn fail_all_pending(self: &Arc<Self>, reason: &str)`
  - `fn pending_count(&self) -> usize`

- [x] **Step 1: 在 `crates/core/src/bridge/mod.rs` 模块声明区(tunnel 声明旁)加一行**

```rust
pub(crate) mod session;
```

- [x] **Step 2: 写失败测试 — 创建 `crates/core/src/bridge/session.rs`,先只写头部注释 + 类型骨架 + 完整 tests 模块**

先写入以下内容(实现部分暂用 `todo!()` 占位使编译通过但测试失败):

```rust
//! BridgeSession: 长期桥接会话 + 逻辑请求复用 (方案 §4-§7, §17-§21)
//!
//! 原则 (方案 §3/§43): TCP 隧道 = Bridge Session(长期), HTTP 请求 = Frame(逻辑请求)。
//! 按请求类别隔离并发额度与超时: Resolve(1) / Detail(2) / Search(4)。
//! 任何异常路径都必须移除 pending, 防止 PENDING 泄漏 (方案 §5)。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot, Semaphore};

use super::tunnel::Frame;

// ---- 并发额度 (方案 §6/§22: 搜索 4 / 详情 2 / 播放解析 1) ----
pub(crate) const MAX_SEARCH_INFLIGHT: usize = 4;
pub(crate) const MAX_DETAIL_INFLIGHT: usize = 2;
pub(crate) const MAX_RESOLVE_INFLIGHT: usize = 1;

// ---- 按类别超时 (方案 §18: Search 8s / Detail 8s / Resolve 15s / Health 2s) ----
pub(crate) const SEARCH_TIMEOUT: Duration = Duration::from_secs(8);
pub(crate) const DETAIL_TIMEOUT: Duration = Duration::from_secs(8);
pub(crate) const RESOLVE_TIMEOUT: Duration = Duration::from_secs(15);
pub(crate) const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);

/// 请求类别: 由 VirtualBridge 收到的 HTTP 请求行路径推断
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RequestKind {
    Resolve,
    Detail,
    Search,
    Health,
}

impl RequestKind {
    pub(crate) fn from_path(path: &str) -> Self {
        todo!()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RequestError {
    NoTunnel,
    Timeout,
    TunnelClosed,
    SendFailed,
}

impl RequestError {
    /// 转 spider 层可识别的业务错误 JSON (code!=200 即错, spider/mod.rs:460)
    pub(crate) fn to_err_json(&self) -> String {
        todo!()
    }
}

/// 并发/超时参数 (生产用 Default; 测试注入更短超时与小额度)
pub(crate) struct Limits {
    pub search_inflight: usize,
    pub detail_inflight: usize,
    pub resolve_inflight: usize,
    pub search_timeout: Duration,
    pub detail_timeout: Duration,
    pub resolve_timeout: Duration,
    pub health_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            search_inflight: MAX_SEARCH_INFLIGHT,
            detail_inflight: MAX_DETAIL_INFLIGHT,
            resolve_inflight: MAX_RESOLVE_INFLIGHT,
            search_timeout: SEARCH_TIMEOUT,
            detail_timeout: DETAIL_TIMEOUT,
            resolve_timeout: RESOLVE_TIMEOUT,
            health_timeout: HEALTH_TIMEOUT,
        }
    }
}

struct PendingEntry {
    tx: oneshot::Sender<Vec<u8>>,
    enqueued_at: Instant,
}

/// 长期桥接会话: 一条隧道连接上的全部逻辑请求复用 (方案 §4)
pub(crate) struct BridgeSession {
    pub device: String,
    writer: mpsc::Sender<Frame>,
    pending: StdMutex<HashMap<u32, PendingEntry>>,
    next_id: AtomicU32,
    /// 观测用: 正常完成路径递减; 调用方取消时不保证归零, 权威值看 pending_count
    inflight: AtomicU32,
    search_gate: Arc<Semaphore>,
    detail_gate: Arc<Semaphore>,
    resolve_gate: Arc<Semaphore>,
    limits: Limits,
}

impl BridgeSession {
    pub(crate) fn new(device: String, writer: mpsc::Sender<Frame>, limits: Limits) -> Arc<Self> {
        todo!()
    }

    pub(crate) fn pending_count(&self) -> usize {
        todo!()
    }

    pub(crate) fn inflight_count(&self) -> u32 {
        todo!()
    }

    pub(crate) async fn request(
        self: &Arc<Self>,
        kind: RequestKind,
        payload: Vec<u8>,
    ) -> Result<Vec<u8>, RequestError> {
        todo!()
    }

    pub(crate) fn deliver(self: &Arc<Self>, id: u32, payload: Vec<u8>) {
        todo!()
    }

    pub(crate) fn fail_all_pending(self: &Arc<Self>, reason: &str) {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fast_limits(search: usize, detail: usize, resolve: usize, ms: u64) -> Limits {
        Limits {
            search_inflight: search,
            detail_inflight: detail,
            resolve_inflight: resolve,
            search_timeout: Duration::from_millis(ms),
            detail_timeout: Duration::from_millis(ms),
            resolve_timeout: Duration::from_millis(ms),
            health_timeout: Duration::from_millis(ms),
        }
    }

    #[test]
    fn request_kind_from_path() {
        // 播放解析 > 详情 > 搜索 的类别判定 (方案 §20)
        assert_eq!(RequestKind::from_path("/playerContent"), RequestKind::Resolve);
        assert_eq!(RequestKind::from_path("/detail"), RequestKind::Detail);
        assert_eq!(RequestKind::from_path("/health"), RequestKind::Health);
        assert_eq!(RequestKind::from_path("/search"), RequestKind::Search);
        assert_eq!(RequestKind::from_path("/init"), RequestKind::Search);
        assert_eq!(RequestKind::from_path("/search?wd=x"), RequestKind::Search);
    }

    #[test]
    fn request_error_json_shape() {
        assert_eq!(
            RequestError::Timeout.to_err_json(),
            "{\"code\":504,\"err\":\"bridge_timeout\"}"
        );
        assert!(RequestError::TunnelClosed.to_err_json().contains("502"));
    }

    #[tokio::test]
    async fn session_multiplexes_concurrent_requests() {
        let (tx, mut rx) = mpsc::channel::<Frame>(16);
        let session = BridgeSession::new("MuMu".into(), tx.clone(), fast_limits(4, 2, 1, 5000));
        drop(tx);

        let s = session.clone();
        let h1 = tokio::spawn(async move { s.request(RequestKind::Search, b"req-1".to_vec()).await });
        let s = session.clone();
        let h2 = tokio::spawn(async move { s.request(RequestKind::Detail, b"req-2".to_vec()).await });

        // 两条逻辑请求复用同一条隧道, frame id 互不相同
        let f1 = rx.recv().await.unwrap();
        let f2 = rx.recv().await.unwrap();
        assert_ne!(f1.id, f2.id);

        // 倒序应答, 按 id 配对 (多路复用语义)
        session.deliver(f2.id, b"resp-2".to_vec());
        session.deliver(f1.id, b"resp-1".to_vec());
        assert_eq!(h1.await.unwrap().unwrap(), b"resp-1".to_vec());
        assert_eq!(h2.await.unwrap().unwrap(), b"resp-2".to_vec());
        assert_eq!(session.pending_count(), 0);
    }

    #[tokio::test]
    async fn session_timeout_returns_error_and_cleans_pending() {
        let (tx, mut rx) = mpsc::channel::<Frame>(16);
        let session = BridgeSession::new("MuMu".into(), tx.clone(), fast_limits(4, 2, 1, 100));
        drop(tx);

        let s = session.clone();
        let h = tokio::spawn(async move { s.request(RequestKind::Health, b"ping".to_vec()).await });
        let _frame = rx.recv().await.unwrap(); // 帧已发出但 mock 永不应答

        let started = Instant::now();
        assert_eq!(h.await.unwrap(), Err(RequestError::Timeout));
        assert!(started.elapsed() < Duration::from_secs(2));
        // PENDING 无泄漏 (方案 §5)
        assert_eq!(session.pending_count(), 0);
    }

    #[tokio::test]
    async fn session_tunnel_disconnect_fails_all_pending() {
        let (tx, mut rx) = mpsc::channel::<Frame>(16);
        let session = BridgeSession::new("MuMu".into(), tx.clone(), fast_limits(4, 2, 1, 5000));
        drop(tx);

        let s = session.clone();
        let h1 = tokio::spawn(async move { s.request(RequestKind::Search, b"a".to_vec()).await });
        let s = session.clone();
        let h2 = tokio::spawn(async move { s.request(RequestKind::Search, b"b".to_vec()).await });
        let _f1 = rx.recv().await.unwrap();
        let _f2 = rx.recv().await.unwrap();

        // 隧道断开: 所有 pending 在有限时间内失败, 不能永久挂起 (方案 §45)
        session.fail_all_pending("tunnel_closed");
        assert_eq!(h1.await.unwrap(), Err(RequestError::TunnelClosed));
        assert_eq!(h2.await.unwrap(), Err(RequestError::TunnelClosed));
        assert_eq!(session.pending_count(), 0);
    }

    #[tokio::test]
    async fn session_caller_cancelled_cleans_pending() {
        let (tx, mut rx) = mpsc::channel::<Frame>(16);
        let session = BridgeSession::new("MuMu".into(), tx.clone(), fast_limits(4, 2, 1, 5000));
        drop(tx);

        let s = session.clone();
        let h = tokio::spawn(async move { s.request(RequestKind::Search, b"q".to_vec()).await });
        let f = rx.recv().await.unwrap();
        h.abort(); // 调用方取消(如搜索代际过期)

        tokio::time::sleep(Duration::from_millis(20)).await;
        // 迟到的响应不再有等待者: deliver 后 pending 干净
        session.deliver(f.id, b"late".to_vec());
        assert_eq!(session.pending_count(), 0);
    }

    #[tokio::test]
    async fn playback_resolve_not_blocked_by_search_queue() {
        // search_inflight=1: 占满搜索额度后, 解析必须仍有独立通道 (方案 §21)
        let (tx, mut rx) = mpsc::channel::<Frame>(16);
        let session = BridgeSession::new("MuMu".into(), tx.clone(), fast_limits(1, 1, 1, 5000));
        drop(tx);

        let s = session.clone();
        let search1 = tokio::spawn(async move { s.request(RequestKind::Search, b"old".to_vec()).await });
        let f_search = rx.recv().await.unwrap();

        // 第二个搜索: 只能等搜索额度, 不得占用解析通道
        let s = session.clone();
        let search2 = tokio::spawn(async move { s.request(RequestKind::Search, b"queued".to_vec()).await });

        // 播放解析: 立即获得独立额度并发出帧 (不被搜索阻塞)
        let s = session.clone();
        let resolve = tokio::spawn(async move { s.request(RequestKind::Resolve, b"play".to_vec()).await });
        let f_resolve = tokio::time::timeout(Duration::from_millis(300), rx.recv())
            .await
            .expect("resolve 帧必须先于排队中的搜索发出")
            .unwrap();
        assert_ne!(f_resolve.id, f_search.id);

        session.deliver(f_resolve.id, b"media".to_vec());
        assert_eq!(resolve.await.unwrap().unwrap(), b"media".to_vec());
        // 排队中的搜索仍未获得额度
        assert!(tokio::time::timeout(Duration::from_millis(150), search2).await.is_err());

        // 清理: 放行所有挂起请求, 避免 await 悬挂
        session.fail_all_pending("test_end");
        let _ = tokio::time::timeout(Duration::from_millis(500), search1).await;
    }
}
```

- [x] **Step 3: 运行测试确认失败**

Run: `cargo test -p quantumtv-core bridge::session`
Expected: 编译通过但 `request_kind_from_path` panic (`not yet implemented: todo!`) 等 — 全部 FAIL/panic。

- [x] **Step 4: 写最小实现 (替换全部 `todo!()`)**

```rust
impl RequestKind {
    pub(crate) fn from_path(path: &str) -> Self {
        match path.split('?').next().unwrap_or(path) {
            "/playerContent" => RequestKind::Resolve,
            "/detail" => RequestKind::Detail,
            "/health" => RequestKind::Health,
            _ => RequestKind::Search,
        }
    }

    fn label(self) -> &'static str {
        match self {
            RequestKind::Resolve => "resolve",
            RequestKind::Detail => "detail",
            RequestKind::Search => "search",
            RequestKind::Health => "health",
        }
    }
}

impl RequestError {
    pub(crate) fn to_err_json(&self) -> String {
        let (code, msg) = match self {
            RequestError::NoTunnel => (503, "bridge_not_connected"),
            RequestError::Timeout => (504, "bridge_timeout"),
            RequestError::TunnelClosed => (502, "bridge_disconnected"),
            RequestError::SendFailed => (502, "bridge_send_failed"),
        };
        format!("{{\"code\":{code},\"err\":\"{msg}\"}}")
    }
}

impl Limits {
    fn timeout(&self, kind: RequestKind) -> Duration {
        match kind {
            RequestKind::Resolve => self.resolve_timeout,
            RequestKind::Detail => self.detail_timeout,
            RequestKind::Search => self.search_timeout,
            RequestKind::Health => self.health_timeout,
        }
    }
}

impl BridgeSession {
    pub(crate) fn new(device: String, writer: mpsc::Sender<Frame>, limits: Limits) -> Arc<Self> {
        Arc::new(Self {
            search_gate: Arc::new(Semaphore::new(limits.search_inflight)),
            detail_gate: Arc::new(Semaphore::new(limits.detail_inflight)),
            resolve_gate: Arc::new(Semaphore::new(limits.resolve_inflight)),
            device,
            writer,
            pending: StdMutex::new(HashMap::new()),
            next_id: AtomicU32::new(1),
            inflight: AtomicU32::new(0),
            limits,
        })
    }

    pub(crate) fn pending_count(&self) -> usize {
        self.pending.lock().unwrap().len()
    }

    pub(crate) fn inflight_count(&self) -> u32 {
        self.inflight.load(Ordering::SeqCst)
    }

    /// 一次逻辑请求 (方案 §5 生命周期):
    /// 取类别额度 → 注册 pending[id] → 发帧 → 带超时等响应 → 任何路径都移除 pending
    pub(crate) async fn request(
        self: &Arc<Self>,
        kind: RequestKind,
        payload: Vec<u8>,
    ) -> Result<Vec<u8>, RequestError> {
        let gate = match kind {
            RequestKind::Resolve => &self.resolve_gate,
            RequestKind::Detail => &self.detail_gate,
            RequestKind::Search | RequestKind::Health => &self.search_gate,
        };
        let enqueued_at = Instant::now();
        // 额度排队: 搜索吃不到播放/详情的独立额度 (方案 §21)
        let _permit = gate.acquire().await.map_err(|_| RequestError::TunnelClosed)?;
        let queue_ms = enqueued_at.elapsed().as_millis();

        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap()
            .insert(id, PendingEntry { tx, enqueued_at });
        self.inflight.fetch_add(1, Ordering::SeqCst);

        if self.writer.send(Frame { id, payload }).await.is_err() {
            self.inflight.fetch_sub(1, Ordering::SeqCst);
            self.pending.lock().unwrap().remove(&id);
            return Err(RequestError::SendFailed);
        }

        let result = match tokio::time::timeout(self.limits.timeout(kind), rx).await {
            Err(_) => Err(RequestError::Timeout),
            // 空 payload = 传输层死亡 (reader_task/shutdown 广播的既有语义)
            Ok(Ok(body)) if body.is_empty() => Err(RequestError::TunnelClosed),
            Ok(Ok(body)) => Ok(body),
            // PendingEntry 被 deliver/fail_all 消费后丢弃, 或会话整体被 drop
            Ok(Err(_)) => Err(RequestError::TunnelClosed),
        };
        self.inflight.fetch_sub(1, Ordering::SeqCst);
        // 兜底清理: 超时/取消路径的 pending 也必须移除 (方案 §5)
        self.pending.lock().unwrap().remove(&id);

        let total_ms = enqueued_at.elapsed().as_millis();
        log::info!(
            "[BridgePerf] type={} device={} queue={}ms bridge={}ms total={}ms ok={} pending={}",
            kind.label(),
            self.device,
            queue_ms,
            total_ms.saturating_sub(queue_ms),
            total_ms,
            result.is_ok(),
            self.pending_count()
        );
        result
    }

    /// reader_task 收到响应帧: 按 id 配对唤醒等待者
    pub(crate) fn deliver(self: &Arc<Self>, id: u32, payload: Vec<u8>) {
        if let Some(entry) = self.pending.lock().unwrap().remove(&id) {
            let _ = entry.tx.send(payload);
        }
    }

    /// 隧道断开/退出: 所有 pending 立即以空 payload 失败 (沿用"空=死亡"语义)
    pub(crate) fn fail_all_pending(self: &Arc<Self>, reason: &str) {
        let entries: Vec<PendingEntry> =
            self.pending.lock().unwrap().drain().map(|(_, v)| v).collect();
        if !entries.is_empty() {
            log::warn!(
                "[桥接] 会话 {} 失败全部 pending ({} 个): {}",
                self.device,
                entries.len(),
                reason
            );
        }
        for e in entries {
            let _ = e.tx.send(Vec::new());
        }
    }
}
```

- [x] **Step 5: 运行测试确认通过**

Run: `cargo test -p quantumtv-core bridge::session`
Expected: 7 个测试全部 PASS。

- [x] **Step 6: Commit**

```bash
git add crates/core/src/bridge/session.rs crates/core/src/bridge/mod.rs
git commit -m "feat(bridge): BridgeSession 会话模块 (类别额度/超时/取消清理/性能指标)"
```

---

### Task 2: 隧道接线 BridgeSession (超时快速失败)

**Files:**
- Modify: `crates/core/src/bridge/tunnel.rs` (替换 PENDING/NEXT_ID/TunnelConn 静态与 handle_bridge_client/reader_task/writer_task/shutdown_all)
- Test: `crates/core/src/bridge/tunnel.rs` `mod tests` 追加

**Interfaces:**
- Consumes: Task 1 的 `BridgeSession`/`RequestKind`/`RequestError`/`Limits`
- Produces: `ACTIVE: std::sync::Mutex<Option<Arc<BridgeSession>>>` 全局持有会话;`is_active()`/`wait_registration()`/`shutdown_all()` 对外签名不变。

- [x] **Step 1: 写失败测试 — 在 tunnel.rs `mod tests` 末尾追加**

```rust
#[tokio::test]
async fn bridge_timeout_returns_error_json() {
    // mock APK 注册但不应答 /health → 桌面侧 2s 超时, 返回 {"code":504,...} 而非挂死
    let tl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let tunnel_port = tl.local_addr().unwrap().port();
    let bridge_port = bl.local_addr().unwrap().port();
    serve(tl, bl, format!("http://127.0.0.1:{bridge_port}")).await;

    let mock = tokio::spawn(async move {
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", tunnel_port)).await.unwrap();
        write_frame(&mut s, 0, br#"{"device":"SilentMu","apk":"1.1"}"#).await.unwrap();
        // 不读取/不回答后续帧
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
```

- [x] **Step 2: 运行确认失败**

Run: `cargo test -p quantumtv-core bridge::tunnel::tests::bridge_timeout`
Expected: FAIL — 现实现无超时,连接被静默关闭,响应不含 `"code":504` (read_to_end 得到空/挂起)。

- [x] **Step 3: 实施 tunnel.rs 接线**

3a. 顶部 use 区(`use super::tunnel::Frame` 已在本文件,加):

```rust
use std::sync::Arc;
use super::session::{BridgeSession, Limits, RequestError, RequestKind};
```

3b. 删除 `TunnelConn` 结构体(tunnel.rs:66-70)、`PENDING`、`NEXT_ID` 两个静态(tunnel.rs:83-85),`ACTIVE` 类型改为:

```rust
static ACTIVE: LazyLock<StdMutex<Option<Arc<BridgeSession>>>> =
    LazyLock::new(|| StdMutex::new(None));
```

3c. `tunnel_accept_loop` 注册段(tunnel.rs:179-183)替换 channel 创建与会话构造:

```rust
let (rd, wr) = s.into_split();
let (tx, rx) = mpsc::channel::<Frame>(64);
let session = BridgeSession::new(device.clone(), tx, Limits::default());
*ACTIVE.lock().unwrap() = Some(session.clone());
tokio::spawn(writer_task(wr, rx));
tokio::spawn(reader_task(rd, session));
```

3d. `writer_task` 改收 `Frame`:

```rust
async fn writer_task(mut wr: tokio::net::tcp::OwnedWriteHalf, mut rx: mpsc::Receiver<Frame>) {
    while let Some(f) = rx.recv().await {
        if write_frame(&mut wr, f.id, &f.payload).await.is_err() {
            break;
        }
    }
}
```

3e. `reader_task` 改为按会话配对:

```rust
async fn reader_task(mut rd: tokio::net::tcp::OwnedReadHalf, session: Arc<BridgeSession>) {
    loop {
        match read_frame(&mut rd).await {
            Some(f) => session.deliver(f.id, f.payload),
            None => break,
        }
    }
    // 隧道断开: 所有 pending 有限时间内失败 (方案 §45)
    session.fail_all_pending("隧道断开");
    *ACTIVE.lock().unwrap() = None;
    log::warn!("[桥接] 隧道断开, 等待 APK 重拨");
}
```

3f. `shutdown_all` 中 PENDING 段(tunnel.rs:156-159)替换为:

```rust
    if let Some(s) = ACTIVE.lock().unwrap().take() {
        s.fail_all_pending("shutdown");
    }
```

3g. `handle_bridge_client` 整体替换(经会话发起,失败返回业务错误 JSON):

```rust
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
```

- [x] **Step 4: 运行 tunnel + session 全部测试**

Run: `cargo test -p quantumtv-core bridge::`
Expected: 全部 PASS(含既有 `tunnel_end_to_end_and_reject_second`、新增 `bridge_timeout_returns_error_json`)。

- [x] **Step 5: Commit**

```bash
git add crates/core/src/bridge/tunnel.rs
git commit -m "feat(bridge): 隧道接线 BridgeSession, 请求带类别超时并快速失败"
```

---

### Task 3: VirtualBridge HTTP Keep-Alive 会话化

**Files:**
- Modify: `crates/core/src/bridge/tunnel.rs` (handle_bridge_client → handle_bridge_session 循环)
- Test: `crates/core/src/bridge/tunnel.rs` `mod tests` 追加

**Interfaces:**
- Consumes: Task 2 的 ACTIVE/session 接线
- Produces: 一条 TCP 连接可服务多个 HTTP 请求 (`Connection: keep-alive`);函数名 `handle_bridge_session`(方案 §16)。

- [x] **Step 1: 写失败测试 — tunnel.rs `mod tests` 追加**

```rust
#[tokio::test]
async fn bridge_keep_alive_serves_multiple_requests_per_connection() {
    // 方案 §15/§16: 一条 TCP 客户端连接服务多个 HTTP 请求
    let tl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let tunnel_port = tl.local_addr().unwrap().port();
    let bridge_port = bl.local_addr().unwrap().port();
    serve(tl, bl, format!("http://127.0.0.1:{bridge_port}")).await;

    let mock = tokio::spawn(async move {
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", tunnel_port)).await.unwrap();
        write_frame(&mut s, 0, br#"{"device":"KeepMu","apk":"1.1"}"#).await.unwrap();
        for _ in 0..2 {
            let f = read_frame(&mut s).await.unwrap();
            assert!(f.payload.starts_with(b"POST /health"));
            write_frame(&mut s, f.id, b"{\"code\":200,\"err\":\"ok\",\"data\":\"{}\"}").await.unwrap();
        }
    });

    assert!(wait_registration(std::time::Duration::from_secs(3)).await);

    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", bridge_port)).await.unwrap();
    for _ in 0..2 {
        s.write_all(b"POST /health HTTP/1.1\r\nContent-Length: 0\r\n\r\n").await.unwrap();
        let resp = read_http_response(&mut s).await;
        assert!(resp.starts_with("HTTP/1.1 200 OK"), "{resp}");
        assert!(resp.contains("\"code\":200"), "{resp}");
    }
    // keep-alive 响应不得再出现 Connection: close
    s.write_all(b"POST /health HTTP/1.1\r\nContent-Length: 0\r\n\r\n").await.unwrap();
    let resp = read_http_response(&mut s).await;
    assert!(resp.contains("Connection: keep-alive"), "{resp}");

    mock.await.unwrap();
    shutdown_all().await;
    crate::bridge::reset_effective();
    crate::bridge::set_status(crate::bridge::BridgeStatus::Idle);
}

/// 测试辅助: 按 Content-Length 读完整一个 HTTP 响应 (keep-alive 连接不能 read_to_end)
async fn read_http_response(s: &mut tokio::net::TcpStream) -> String {
    let mut buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; 1024];
    let header_end = loop {
        let n = s.read(&mut tmp).await.unwrap();
        assert!(n > 0, "连接被对端关闭");
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let content_length: usize = head
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            if k.trim().eq_ignore_ascii_case("content-length") {
                v.trim().parse().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    while buf.len() < header_end + content_length {
        let n = s.read(&mut tmp).await.unwrap();
        assert!(n > 0);
        buf.extend_from_slice(&tmp[..n]);
    }
    format!("{}{}", head, String::from_utf8_lossy(&buf[header_end..]))
}
```

注意: `mod tests` 里已有 `use tokio::io::{AsyncReadExt, AsyncWriteExt};` 的话去掉重复导入。

- [x] **Step 2: 运行确认失败**

Run: `cargo test -p quantumtv-core bridge::tunnel::tests::bridge_keep_alive`
Expected: FAIL — 现实现一个请求后 shutdown,第二个请求读到 EOF panic("连接被对端关闭")。

- [x] **Step 3: 改为 keep-alive 会话循环**

3a. `bridge_accept_loop`(tunnel.rs:231)中 spawn 目标改名:

```rust
tokio::spawn(handle_bridge_session(stream));
```

3b. `handle_bridge_client` 改名并加循环;`write_http_response` 改 keep-alive 且返回写状态(§15/§16):

```rust
/// 方案 §16: 一条客户端连接 = 一个 HTTP 会话, 循环服务多个请求
async fn handle_bridge_session(mut stream: TcpStream) {
    loop {
        // 空闲读兜底 120s: keep-alive 客户端(reqwest 池)长时间不发请求则关闭任务
        let read = tokio::time::timeout(std::time::Duration::from_secs(120), read_http_request(&mut stream));
        let Ok(Some(raw)) = read.await else { return };
        let Some(session) = ACTIVE.lock().unwrap().clone() else { return };
        let kind = RequestKind::from_path(request_path(&raw));
        let body = match session.request(kind, raw).await {
            Ok(b) => b,
            Err(e) => e.to_err_json().into_bytes(),
        };
        if !write_http_response(&mut stream, &body).await {
            return; // 客户端已断开
        }
    }
}

async fn write_http_response(stream: &mut TcpStream, body: &[u8]) -> bool {
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
        body.len()
    );
    if stream.write_all(head.as_bytes()).await.is_err() {
        return false;
    }
    stream.write_all(body).await.is_ok()
}
```

(删除旧 `handle_bridge_client` 与 close 版 `write_http_response`;连接关闭靠 stream drop,不再 shutdown。)

- [x] **Step 4: 适配既有测试并运行全部 bridge 测试**

4a. 既有测试 `tunnel_end_to_end_and_reject_second` 用 `read_to_end` 读响应 — keep-alive 下服务端不再主动 shutdown,会永久挂起。把其读法替换为 Step 1 加入的 `read_http_response` 辅助函数,断言不变。同理 `bridge_timeout_returns_error_json`。

4b. Run: `cargo test -p quantumtv-core bridge::`
Expected: 全部 PASS。

- [x] **Step 5: Commit**

```bash
git add crates/core/src/bridge/tunnel.rs
git commit -m "feat(bridge): VirtualBridge HTTP keep-alive 会话化 (一连接多请求)"
```

---

### Task 4: Bridge/Spider 站点级搜索缓存 (TTL 60s)

**Files:**
- Modify: `src-tauri/src/commands/video.rs` (新增 BridgeSearchCache + 改造 search_site_results spider 分支 video.rs:1087-1139)
- Test: `src-tauri/src/commands/video.rs` `mod tests` 追加

**Interfaces:**
- Consumes: `Cache` (moka, video.rs 已用)
- Produces: `BridgeSearchCache::get_or_insert_with(key: String, fetch) -> Result<Vec<SearchResult>, String>`;静态 `BRIDGE_SEARCH_CACHE: LazyLock<BridgeSearchCache>`。

- [x] **Step 1: 写失败测试 — video.rs `mod tests` 追加**

```rust
#[tokio::test]
async fn bridge_search_cache_hit_avoids_refetch() {
    // 方案 §8/§38: 重复搜索必须命中缓存, 不再访问 Bridge
    let cache = crate::commands::video::BridgeSearchCache::new();
    let counter = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let c2 = counter.clone();
    let fetch = move || {
        let c = c2.clone();
        async move {
            c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok::<Vec<crate::commands::video::SearchResult>, String>(vec![])
        }
    };
    let r1 = cache.get_or_insert_with("k:斗破".into(), fetch).await;
    let r2 = cache.get_or_insert_with("k:斗破".into(), fetch).await;
    assert!(r1.is_ok() && r2.is_ok());
    assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn bridge_search_cache_error_not_cached() {
    // 失败不缓存: 下次重试
    let cache = crate::commands::video::BridgeSearchCache::new();
    let counter = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let c2 = counter.clone();
    let fetch = move || {
        let c = c2.clone();
        async move {
            c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err::<Vec<crate::commands::video::SearchResult>, String>("boom".into())
        }
    };
    assert!(cache.get_or_insert_with("k2:x".into(), fetch).await.is_err());
    assert!(cache.get_or_insert_with("k2:x".into(), fetch).await.is_err());
    assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 2);
}
```

- [x] **Step 2: 运行确认失败**

Run(src-tauri 目录): `cargo test --manifest-path src-tauri\Cargo.toml --lib bridge_search_cache`
Expected: 编译失败 — `BridgeSearchCache` 不存在。

- [x] **Step 3: 实现 BridgeSearchCache (放在 video.rs 的 SearchCacheManager 之后)**

```rust
/// Bridge/Spider 站点级搜索缓存 (方案 §8: key=spider_id+keyword, TTL 60s)。
/// 用户"搜索→详情→返回→再搜索"与换关键词回退场景不重复打 Android Spider;
/// try_get_with 兼带请求级 SingleFlight, 失败结果不入缓存。
pub(crate) struct BridgeSearchCache {
    cache: Cache<String, std::sync::Arc<Vec<SearchResult>>>,
}

impl BridgeSearchCache {
    pub fn new() -> Self {
        Self {
            cache: Cache::builder()
                .max_capacity(500)
                .time_to_live(std::time::Duration::from_secs(60))
                .build(),
        }
    }

    pub async fn get_or_insert_with<F, Fut>(
        &self,
        key: String,
        fetch: F,
    ) -> Result<Vec<SearchResult>, String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<Vec<SearchResult>, String>>,
    {
        self.cache
            .try_get_with(key, async { fetch().await.map(std::sync::Arc::new) })
            .await
            .map(|v| (*v).clone())
            .map_err(|e| (*e).clone())
    }
}

pub(crate) static BRIDGE_SEARCH_CACHE: std::sync::LazyLock<BridgeSearchCache> =
    std::sync::LazyLock::new(BridgeSearchCache::new);
```

video.rs 若未引入 `std::sync::LazyLock`/`Cache`,补 use(`Cache` 已有)。

- [x] **Step 4: 改造 search_site_results 的 spider 分支 (video.rs:1087-1139)**

用缓存包装整个 spider 取数+映射(闭包必须 'static,借用字段全部 clone 进 async move):

```rust
    if site.site_type.unwrap_or(1) == 3 {
        if site.searchable.unwrap_or(1) != 1 {
            return Ok(vec![]);
        }
        let class_name = site.api.strip_prefix("csp_").unwrap_or(&site.api);
        // 方案 §8: 站点级搜索缓存 key = spider_id + keyword
        let cache_key = format!("bridge-search:{}:{}", site.key, query);
        let class_owned = class_name.to_string();
        let query_owned = query.to_string();
        let source_key = site.key.clone();
        let source_name = site.name.clone();
        let source_site_type = site.site_type;
        let spider_owned = site.spider.clone().unwrap_or_default();
        let cache_root_owned = cache_root.to_path_buf();

        BRIDGE_SEARCH_CACHE
            .get_or_insert_with(cache_key, || async move {
                let items = if quantumtv_core::spider::is_bridge_class(&class_owned) {
                    // wex Guard 类: 走 Android 桥接 (含 OLLVM/DexNative 保护, JVM 无法加载)
                    let Some(bridge_url) = quantumtv_core::bridge::effective_url() else {
                        return Err("桥接未就绪".to_string());
                    };
                    quantumtv_core::spider::spider_bridge_search(&class_owned, &query_owned, &bridge_url).await?
                } else {
                    quantumtv_core::spider::spider_search(
                        &source_key, &query_owned, &class_owned, &spider_owned, &cache_root_owned,
                    )
                    .await?
                };
                Ok(items
                    .into_iter()
                    .map(|item| {
                        let play_url = item.vod_play_url.as_deref().unwrap_or("");
                        let (episodes, episodes_titles, play_groups) =
                            parse_episode_groups(play_url, item.vod_play_from.as_deref(), true);
                        SearchResult {
                            id: match item.vod_id {
                                Value::String(s) => s,
                                Value::Number(n) => n.to_string(),
                                _ => "".to_string(),
                            },
                            title: item.vod_name.trim().to_string(),
                            poster: item.vod_pic,
                            episodes,
                            episodes_titles,
                            play_groups,
                            source: source_key.clone(),
                            source_name: source_name.clone(),
                            class: item.vod_class,
                            year: item.vod_year,
                            desc: item.vod_content.map(|c| clean_html_tags(&c)),
                            type_name: item.type_name,
                            douban_id: item
                                .vod_douban_id
                                .and_then(|v| v.as_i64())
                                .map(|v| v as i32),
                            source_site_type,
                            login_hint: None,
                            episodes_raw: Vec::new(),
                        }
                    })
                    .collect::<Vec<SearchResult>>())
            })
            .await
    } else {
        // CMS 分支原样保留 (video.rs:1140-1196 不动)
    }
```

- [x] **Step 5: 运行测试与编译**

Run(src-tauri 目录): `cargo test --manifest-path src-tauri\Cargo.toml --lib bridge_search_cache`
Expected: 全部 PASS。

- [x] **Step 6: Commit**

```bash
git add src-tauri/src/commands/video.rs
git commit -m "perf(search): Spider 站点级搜索缓存 60s + 请求 SingleFlight"
```

---

### Task 5: 详情缓存 (TTL 10 分钟)

**Files:**
- Modify: `src-tauri/src/commands/video.rs` (fetch_detail_item video.rs:1467-1523 加缓存;新增 cached_detail_with)
- Test: `src-tauri/src/commands/video.rs` `mod tests` 追加

**Interfaces:**
- Consumes: moka
- Produces: `DETAIL_CACHE: LazyLock<moka::future::Cache<String, Arc<ApiSearchItem>>>`;`async fn cached_detail_with(key: String, fetch) -> Result<ApiSearchItem, String>`;`fetch_detail_item` 对外签名不变。

- [x] **Step 1: 写失败测试 — video.rs `mod tests` 追加**

```rust
#[tokio::test]
async fn detail_cache_hit_avoids_refetch() {
    // 方案 §9: 详情 TTL 5~10 分钟; key = source_id + vod_id
    let counter = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let key = format!("detail:ut:{}:v1", std::process::id());
    let c2 = counter.clone();
    let k2 = key.clone();
    let fetch = move || {
        let c = c2.clone();
        let k = k2.clone();
        async move {
            c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok::<crate::commands::video::ApiSearchItem, String>(crate::commands::video::ApiSearchItem {
                vod_id: serde_json::Value::String(k),
                vod_name: "测试片".into(),
                vod_pic: String::new(),
                vod_remarks: None,
                vod_play_url: Some("第1集$http://x/1.mp4".into()),
                vod_play_from: None,
                vod_class: None,
                vod_year: None,
                vod_content: None,
                vod_douban_id: None,
                type_name: None,
            })
        }
    };
    let first = crate::commands::video::cached_detail_with(key.clone(), fetch).await.unwrap();
    let second = crate::commands::video::cached_detail_with(key, fetch).await.unwrap();
    assert_eq!(first.vod_name, second.vod_name);
    assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 1);
}
```

- [x] **Step 2: 运行确认失败**

Run(src-tauri 目录): `cargo test --manifest-path src-tauri\Cargo.toml --lib detail_cache_hit`
Expected: 编译失败 — `cached_detail_with` 不存在。

- [x] **Step 3: 实现 (fetch_detail_item 上方加)**

```rust
/// 详情缓存 (方案 §9: key = source_id + vod_id, TTL 10 分钟; 详情变化频率远低于搜索)。
/// 播放页挂载/返回再进/播放中 preload 共享同一份, 不再每次打详情接口。
static DETAIL_CACHE: std::sync::LazyLock<moka::future::Cache<String, std::sync::Arc<ApiSearchItem>>> =
    std::sync::LazyLock::new(|| {
        moka::future::Cache::builder()
            .max_capacity(300)
            .time_to_live(std::time::Duration::from_secs(600))
            .build()
    });

/// Detail 缓存包装: 同 key 并发调用共享一次执行 (兼 SingleFlight), 失败不入缓存
async fn cached_detail_with<F, Fut>(key: String, fetch: F) -> Result<ApiSearchItem, String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<ApiSearchItem, String>>,
{
    DETAIL_CACHE
        .try_get_with(key, async { fetch().await.map(std::sync::Arc::new) })
        .await
        .map(|v| (*v).clone())
        .map_err(|e| (*e).clone())
}
```

- [x] **Step 4: fetch_detail_item 接入缓存 (保持函数签名)**

函数体整体改为: 先组 key,再把原实现移入 'static 闭包(借用字段 clone 进 async move):

```rust
async fn fetch_detail_item(
    site: &ApiSite,
    id: &str,
    cache_root: &std::path::Path,
) -> Result<ApiSearchItem, String> {
    let key = format!("detail:{}:{}", site.key, id);
    let site_type = site.site_type.unwrap_or(1);
    let class_owned = site.api.strip_prefix("csp_").unwrap_or(&site.api).to_string();
    let api_owned = site.api.clone();
    let spider_owned = site.spider.clone().unwrap_or_default();
    let source_key = site.key.clone();
    let id_owned = id.to_string();
    let cache_root_owned = cache_root.to_path_buf();

    cached_detail_with(key, move || async move {
        let item = if site_type == 3 {
            if quantumtv_core::spider::is_bridge_class(&class_owned) {
                // wex Guard 类: 走 Android 桥接
                let Some(bridge_url) = quantumtv_core::bridge::effective_url() else {
                    return Err("桥接未就绪".to_string());
                };
                quantumtv_core::spider::spider_bridge_detail(&class_owned, &id_owned, &bridge_url).await?
            } else {
                quantumtv_core::spider::spider_detail(
                    &source_key, &id_owned, &class_owned, &spider_owned, &cache_root_owned,
                )
                .await?
            };
            ApiSearchItem {
                vod_id: serde_json::Value::String(id_owned),
                vod_name: item.vod_name,
                vod_pic: item.vod_pic,
                vod_remarks: item.vod_remarks,
                vod_play_url: item.vod_play_url,
                vod_play_from: item.vod_play_from,
                vod_class: item.vod_class,
                vod_year: item.vod_year,
                vod_content: item.vod_content,
                vod_douban_id: item.vod_douban_id,
                type_name: item.type_name,
            }
        } else {
            let client = get_video_client();
            let url = format!("{}?ac=videolist&ids={}", api_owned, id_owned);
            let resp = timeout(Duration::from_secs(8), client.get(&url).send())
                .await
                .map_err(|_| "Failed to fetch detail: timeout".to_string())?
                .map_err(|e| format!("Failed to fetch detail: {}", e))?;
            if !resp.status().is_success() {
                return Err(format!("Failed to fetch detail: {}", resp.status()));
            }
            let body = timeout(Duration::from_secs(5), resp.text())
                .await
                .map_err(|_| "Failed to read response: timeout".to_string())?
                .map_err(|e| format!("Failed to read response: {}", e))?;
            let search_res = serde_json::from_str::<ApiSearchResponse>(&body)
                .map_err(|e| format!("Parse error: {}, body: {}", e, body))?;
            search_res
                .list
                .into_iter()
                .next()
                .ok_or_else(|| "Video not found".to_string())?
        };
        Ok(item)
    })
    .await
}
```

- [x] **Step 5: 运行测试**

Run(src-tauri 目录): `cargo test --manifest-path src-tauri\Cargo.toml --lib detail_cache`
Expected: PASS。

- [x] **Step 6: Commit**

```bash
git add src-tauri/src/commands/video.rs
git commit -m "perf(detail): 详情缓存 10min, 播放页挂载/preload 不再重复拉详情"
```

---

### Task 6: Resolve 缓存 + SingleFlight

**Files:**
- Modify: `src-tauri/src/commands/playback.rs` (新增 RESOLVE_CACHE + cached_resolve_with)
- Test: `src-tauri/src/commands/playback.rs` 新建 `mod tests`

**Interfaces:**
- Consumes: `MediaResource` (Clone, media.rs:78), `MediaResource::new(id, url, ResourceType)`
- Produces (Task 7 依赖,签名逐字):
  - `static RESOLVE_CACHE: LazyLock<moka::future::Cache<String, Arc<MediaResource>>>`
  - `pub(crate) async fn cached_resolve_with<F, Fut>(key: String, fetch: F) -> Result<Arc<MediaResource>, String>`
  - resolve key 约定: `resolve:{source}:{vod_id}:{flag}:{episode_id}`

- [x] **Step 1: 写失败测试 — playback.rs 文件末尾追加**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use quantumtv_core::media::ResourceType;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn uniq_key(tag: &str) -> String {
        format!(
            "resolve:{}:{}:{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }

    #[tokio::test]
    async fn resolve_cache_hit_skips_second_resolve() {
        // 方案 §12: Resolve 缓存命中不得再触发解析
        let key = uniq_key("cache");
        let counter = Arc::new(AtomicU32::new(0));
        let c2 = counter.clone();
        let fetch = move || {
            let c = c2.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Ok::<_, String>(quantumtv_core::media::MediaResource::new(
                    "ep-1",
                    "http://example.com/1.m3u8",
                    ResourceType::Hls,
                ))
            }
        };
        let r1 = cached_resolve_with(key.clone(), fetch).await.unwrap();
        let r2 = cached_resolve_with(key, fetch).await.unwrap();
        assert_eq!(r1.url, r2.url);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn resolve_singleflight_concurrent_callers_share_one_execution() {
        // 方案 §13/§45: 同一 Episode 并发 5 次 → 实际 Resolve = 1 次
        let key = uniq_key("flight");
        let counter = Arc::new(AtomicU32::new(0));
        let mut handles = Vec::new();
        for _ in 0..5 {
            let c = counter.clone();
            let k = key.clone();
            handles.push(tokio::spawn(async move {
                cached_resolve_with(k, move || {
                    let c = c.clone();
                    async move {
                        c.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        Ok::<_, String>(quantumtv_core::media::MediaResource::new(
                            "ep-2",
                            "http://example.com/2.m3u8",
                            ResourceType::Hls,
                        ))
                    }
                })
                .await
            }));
        }
        for h in handles {
            h.await.unwrap().unwrap();
        }
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn resolve_error_not_cached() {
        // 解析失败不入缓存: 下次播放重试
        let key = uniq_key("err");
        let counter = Arc::new(AtomicU32::new(0));
        let c2 = counter.clone();
        let fetch = move || {
            let c = c2.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err::<quantumtv_core::media::MediaResource, String>("resolve failed".into())
            }
        };
        assert!(cached_resolve_with(key.clone(), fetch).await.is_err());
        assert!(cached_resolve_with(key, fetch).await.is_err());
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }
}
```

- [x] **Step 2: 运行确认失败**

Run(src-tauri 目录): `cargo test --manifest-path src-tauri\Cargo.toml --lib commands::playback`
Expected: 编译失败 — `cached_resolve_with` 不存在。

- [x] **Step 3: 实现 (playback.rs `use` 区后新增)**

```rust
use std::sync::LazyLock;
use quantumtv_core::media::MediaResource;

/// Resolve 缓存 + SingleFlight (方案 §12/§13):
/// key = resolve:{source}:{vod_id}:{flag}:{episode_id}; TTL 10 分钟(普通 URL 5~30 分钟档)。
/// moka try_get_with: 同 key 并发调用共享同一 Future — 只执行一次解析 (SingleFlight);
/// 失败不入缓存 (下次播放自动重试)。临时签名 URL 的 expires_at 细化留待观测后调整。
static RESOLVE_CACHE: LazyLock<moka::future::Cache<String, Arc<MediaResource>>> =
    LazyLock::new(|| {
        moka::future::Cache::builder()
            .max_capacity(200)
            .time_to_live(std::time::Duration::from_secs(600))
            .build()
    });

pub(crate) async fn cached_resolve_with<F, Fut>(
    key: String,
    fetch: F,
) -> Result<Arc<MediaResource>, String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<MediaResource, String>>,
{
    RESOLVE_CACHE
        .try_get_with(key, async { fetch().await.map(Arc::new) })
        .await
        .map_err(|e| (*e).clone())
}
```

- [x] **Step 4: 运行测试**

Run(src-tauri 目录): `cargo test --manifest-path src-tauri\Cargo.toml --lib commands::playback`
Expected: 3 个测试 PASS。

- [x] **Step 5: Commit**

```bash
git add src-tauri/src/commands/playback.rs
git commit -m "perf(resolve): Resolve 缓存 + SingleFlight (并发同集只解析一次)"
```

---

### Task 7: 播放链路接入缓存 + vodId 透传 (P0)

**Files:**
- Modify: `src-tauri/src/commands/playback.rs:190-244` (playback_play_episode 用 cached_resolve_with;签名加 `vod_id: Option<String>`)
- Modify: `src-tauri/src/commands/video.rs:2666-2688` (enrich_first_episode_direct 用同一缓存)
- Modify: `src/app/play/page.tsx:260-267` (invoke 增加 vodId)
- Test: Task 6 已覆盖缓存行为;本任务以编译+既有测试回归为主

**Interfaces:**
- Consumes: Task 6 `cached_resolve_with`;key 约定 `resolve:{source}:{vod_id}:{flag}:{episode_id}`
- Produces: `playback_play_episode` 新可选参数 `vod_id`;`enrich_first_episode_direct` 的首集解析与点击播放第 1 集共享同一缓存条目 (消除重复 playerContent)。

- [x] **Step 1: playback_play_episode 加参数并接缓存**

签名(`src-tauri/src/commands/playback.rs:196` `episode_id: String,` 之后)插入:

```rust
    vod_id: Option<String>,
```

解析段(:210-244)替换为:

```rust
    // 站点信息: spider 站点需要类名与 site_type 判定 (原 :203-208 保留)
    let config =
        crate::commands::config::get_config_with_db_sources(&storage, &db)?;
    let site = crate::commands::video::resolve_enabled_source(&config, &source)
        .ok_or_else(|| format!("Source not found or disabled: {}", source))?;
    let site_type = site.site_type.unwrap_or(1);

    // 桥接就绪前置检查保留 (fail fast, 不进缓存闭包, 不污染缓存)
    let bridge_url_owned = if site_type == 3 {
        Some(
            quantumtv_core::bridge::effective_url()
                .ok_or_else(|| "桥接未就绪".to_string())?,
        )
    } else {
        None
    };

    // Resolve 缓存 + SingleFlight (方案 §10/§12/§13): 播放只 Resolve, 不得 Search
    let source_c = source.clone();
    let flag_c = flag.clone();
    let ep_c = episode_id.clone();
    let vod_c = vod_id.clone().unwrap_or_default();
    let class_c = site.api.strip_prefix("csp_").unwrap_or(&site.api).to_string();
    let site_type_c = site_type;
    let resolve_key = format!("resolve:{}:{}:{}:{}", source_c, vod_c, flag_c, ep_c);
    let resource = crate::commands::playback::cached_resolve_with(resolve_key, move || async move {
        if site_type_c == 3 {
            let manager = quantumtv_core::resolver::ResolverManager::with_defaults(Arc::new(
                quantumtv_core::spider::BridgeSpiderPlayFetcher {
                    bridge_url: bridge_url_owned.clone().unwrap_or_default(),
                },
            ));
            manager
                .resolve(&quantumtv_core::resolver::ResolveInput::spider(
                    &source_c, &flag_c, &ep_c, &class_c,
                ))
                .await
                .map_err(|e| e.to_string())
        } else {
            let manager = quantumtv_core::resolver::ResolverManager::with_defaults(Arc::new(
                quantumtv_core::spider::BridgeSpiderPlayFetcher { bridge_url: String::new() },
            ));
            manager
                .resolve(&quantumtv_core::resolver::ResolveInput::direct(
                    source_c.clone(),
                    ep_c.clone(),
                ))
                .await
                .map_err(|e| e.to_string())
        }
    })
    .await?;
    let mut resource = (*resource).clone();
```

注意:
- 删除原 `quantum_core_resolve_input_spider` 辅助(:291-298)与原 :210-244 的 if/else 解析块;`class_c` 即类名 (`site.api` 去 `csp_` 前缀)。
- `effective_url()` 检查保持在闭包外,使"桥接未就绪"仍是即时错误且不污染缓存。
- `resource.metadata.title/episode` 的赋值段(:247-249)在 `let mut resource = (*resource).clone();` 之后原样保留。

- [x] **Step 2: enrich_first_episode_direct 接同一缓存 (video.rs:2676-2688)**

```rust
    let flag = result
        .play_groups
        .first()
        .map(|g| g.flag.clone())
        .unwrap_or_default(); // 默认组 flag, 与前端 activeGroupIndex=0 时 playback_play_episode 的 key 对齐
    // 与 playback_play_episode 共用 Resolve 缓存 + SingleFlight (方案 §10/§13):
    // 详情阶段解析过首集后, 点击播放第 1 集直接命中缓存, 不再发第二次 playerContent
    let resolve_key = format!("resolve:{}:{}:{}:{}", site.key, result.id, flag, first_raw);
    let class_c = class_name.to_string();
    let source_c = site.key.clone();
    let first_c = first_raw.clone();
    let bridge_c = bridge_url.clone();
    let flag_c = flag.clone();
    let resolved = crate::commands::playback::cached_resolve_with(resolve_key, move || async move {
        let manager = quantumtv_core::resolver::ResolverManager::with_defaults(Arc::new(
            quantumtv_core::spider::BridgeSpiderPlayFetcher { bridge_url: bridge_c },
        ));
        manager
            .resolve(&quantumtv_core::resolver::ResolveInput::spider(
                source_c, flag_c, first_c, &class_c,
            ))
            .await
            .map_err(|e| e.to_string())
    })
    .await;
    match resolved {
        Ok(resource) => {
            let resource = (*resource).clone();
            // V2 Phase 3: 网盘直链经 PlaybackGateway 包装 (opaque token)
            let final_url = match quantumtv_core::gateway::wrap_resource(&resource).await {
                Ok(url) => url,
                Err(e) => {
                    log::warn!(
                        "[播放解析] 首集 Gateway 包装失败, 回退旧代理路径: {}",
                        quantumtv_core::spider::trunc(&e, 120)
                    );
                    match quantumtv_core::netdisk_proxy::ensure_started().await {
                        Ok(port) => quantumtv_core::netdisk_proxy::wrap_proxy_url(
                            &resource.url,
                            resource.user_agent.as_deref(),
                            port,
                        ),
                        Err(_) => resource.url.clone(),
                    }
                }
            };
            log::info!(
                "[播放解析] 首集直链化成功 ({}): url={}",
                site.key,
                quantumtv_core::spider::trunc(&final_url, 100)
            );
            if !result.episodes.is_empty() {
                result.episodes[0] = final_url; // 第 1 集已是可播地址
            }
            // 其余集保持 raw id, 前端切集时逐集解析
        }
        Err(err) => {
            // 解析失败: 保留 raw id(前端会再试), 并置提示
            log::warn!(
                "[播放解析] 首集直链化失败 ({}): {}",
                site.key,
                quantumtv_core::spider::trunc(&err.to_string(), 200)
            );
            result.login_hint = Some(err.to_string());
        }
    }
```

- [x] **Step 3: 前端透传 vodId (src/app/play/page.tsx:260-267)**

```ts
      await invoke('playback_play_episode', {
        source: d.source,
        flag,
        episodeId,
        vodId: d.id || null,
        title: d.title || null,
        episode: currentEpisodeTitle(),
        startAt: startAt ?? null,
      });
```

(Tauri v2 自动 camelCase↔snake_case 映射;`vod_id: Option<String>` 兼容旧调用不传。)

- [x] **Step 4: 回归测试**

Run(src-tauri 目录): `cargo test`
Run(workspace 根): `cargo test -p quantumtv-core`
Run(仓库根): `npm run typecheck`
Expected: 全部 PASS。

- [x] **Step 5: Commit**

```bash
git add src-tauri/src/commands/playback.rs src-tauri/src/commands/video.rs src/app/play/page.tsx
git commit -m "perf(playback): 播放/首集直链化共用 Resolve 缓存, 透传 vodId (禁止重复 Resolve)"
```

---

### Task 8: 搜索代际贯通前端 (旧结果丢弃 + 新搜索先 abort)

**Files:**
- Modify: `src-tauri/src/commands/video.rs` (SearchStreamEvent:574-581 加 generation;emit 事件 :1317、:1437;search_with_cache_hit :1199 返回三元组;新增 current_search_generation())
- Modify: `src-tauri/src/commands/search.rs` (:553-611 两个命令与两个响应结构 :323-341 加 generation)
- Modify: `src/app/search/page.tsx` (generationRef 守卫 + 新搜索前 abort_active_search)
- Test: src-tauri 既有测试回归 + `npm run typecheck`

**Interfaces:**
- Produces: `SearchStreamEvent.generation: u64`;`SearchPageQueryResponse/SearchPageOpenResponse.generation: u64`;`search_with_cache_hit(...) -> Result<(Vec<SearchResult>, bool, u64), String>`;`pub(crate) fn current_search_generation() -> u64`。

- [x] **Step 1: Rust 侧**

1a. video.rs `SearchStreamEvent`(:574-581)加字段:

```rust
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SearchStreamEvent {
    pub results: Vec<SearchResult>,
    pub source: String,
    pub source_name: String,
    pub total_sources: i32,
    pub completed_sources: i32,
    /// 本次搜索的代际: 前端只接受 generation == 最新代际的事件 (方案 §7)
    pub generation: u64,
}
```

1b. video.rs 在 `abort_active_search`(:911)旁加访问器:

```rust
/// 当前代际值 (缓存命中/空查询路径回填 generation 用)
pub(crate) fn current_search_generation() -> u64 {
    SEARCH_GENERATION.load(std::sync::atomic::Ordering::SeqCst)
}
```

1c. `search_with_cache_hit`(:1199)签名与返回改为三元组:

```rust
) -> Result<(Vec<SearchResult>, bool, u64), String> {
    // 首先尝试从缓存获取结果
    if let Some(cached_results) = cache.get(&query).await {
        let gen = current_search_generation();
        return Ok((cached_results, true, gen));
    }
```

流式事件构造(:1317)与完成事件(:1437)加 `generation`:

```rust
                    SearchStreamEvent {
                        results,
                        source: site_key.to_string(),
                        source_name: site_name.to_string(),
                        total_sources,
                        completed_sources: completed.get(),
                        generation: my_generation,
                    },
```

```rust
                serde_json::json!({
                    "total": unique_results.len(),
                    "query": query,
                    "generation": my_generation
                }),
```

函数内所有 `return Ok((Vec::new(), false))` / `Ok((unique_results, false))` 均改为 `(.., my_generation)`(早期源列表为空的 `return Ok((vec![], false))` 处改用 `current_search_generation()`,代际尚未分配)。

1d. 调用方适配:
- video.rs `search` 命令包装: `let (results, _cache_hit, _generation) = ...`
- video.rs `initialize_player_by_query`: `let (results, _, _generation) = ...`
- search.rs:561 与 :590: `let (results, cache_hit, generation) = ...`;两个响应结构(:325-341)各加 `pub generation: u64`,构造处带上(空查询路径用 `crate::commands::video::current_search_generation()`)。

- [x] **Step 2: 前端 search/page.tsx**

2a. 在组件内 ref 声明区(streamingRef 附近)加:

```tsx
  // 搜索代际守卫 (方案 §7): 只接受最新一次搜索的流式事件, 旧代际结果直接丢弃
  const generationRef = useRef<number>(-1);
```

2b. 搜索 effect(:298-381)在 `streamingRef.current = [];` 之后、invoke 之前加 abort(与播放页 :855 同款):

```tsx
    // 新搜索启动即断旧搜索: Rust 侧旧代际任务自行退出, 不再继续打站点
    invoke('abort_active_search').catch(() => {});
```

2c. `search_page_open` 的 `.then`(:330)首行记录代际:

```tsx
        generationRef.current = response?.generation ?? -1;
```

2d. 流式监听守卫 — `search-stream-result` 处理器(:393-410)顶部加:

```tsx
          if (
            generationRef.current >= 0 &&
            typeof event.payload?.generation === 'number' &&
            event.payload.generation !== generationRef.current
          ) {
            return;
          }
```

`search-stream-completed` 处理器(:413)同样在回调第一行加同款守卫。

2e. 类型: `SearchPageOpenResponse` 定义处(src/app/search/page.tsx:33)加 `generation: number;`。

- [x] **Step 3: 验证**

Run(src-tauri 目录): `cargo test`
Run(仓库根): `npm run typecheck`
Expected: 全部 PASS。

- [x] **Step 4: Commit**

```bash
git add src-tauri/src/commands/video.rs src-tauri/src/commands/search.rs src/app/search/page.tsx
git commit -m "feat(search): 搜索代际贯通到前端, 新搜索先 abort 旧代际"
```

---

### Task 9: 网关参数校准 + NetdiskRangeTest

**Files:**
- Modify: `crates/core/src/netdisk_proxy.rs:90-104` (client() 参数)
- Test: `crates/core/src/netdisk_proxy.rs` `mod tests` 追加

**Interfaces:**
- Consumes: `ensure_started()` / `wrap_proxy_url()` (netdisk_proxy.rs:26/:51)
- Produces: 无签名变化;连接池参数对齐方案 §29。

- [x] **Step 1: 写失败测试 — netdisk_proxy.rs `mod tests` 追加**

```rust
    #[tokio::test]
    async fn netdisk_range_forwarded_and_206_passthrough() {
        // 方案 §27/§45: mpv 的 Range 必须原样打给上游, 206 透传回播放器
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // 上游 mock: 校验收到的 Range 头, 返回 206 + Content-Range
        let upstream = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_port = upstream.local_addr().unwrap().port();
        let up = tokio::spawn(async move {
            let (mut s, _) = upstream.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = s.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            assert!(req.contains("Range: bytes=50000000-"), "{req}");
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
            format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nRange: bytes=50000000-\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
        let mut resp = Vec::new();
        s.read_to_end(&mut resp).await.unwrap();
        let text = String::from_utf8_lossy(&resp);
        assert!(text.starts_with("HTTP/1.1 206 Partial Content"), "{text}");
        assert!(text.contains("Content-Range: bytes 50000000-50000099/100000000"), "{text}");
        let body_len = resp.len() - text.find("\r\n\r\n").unwrap() - 4;
        assert_eq!(body_len, 100);

        up.await.unwrap();
    }
```

- [x] **Step 2: 运行确认通过 (现状即满足, 防回归基线)**

Run: `cargo test -p quantumtv-core netdisk_proxy`
Expected: PASS。

- [x] **Step 3: 校准 client() 参数 (方案 §29)**

netdisk_proxy.rs:90-104 的 builder 替换为:

```rust
        reqwest::Client::builder()
            // 流式透传不能设总超时(整个 3GB 响应都走这一个请求), 只限连接建立
            .connect_timeout(Duration::from_secs(5))
            // Phase 7: 显式重定向策略 (原为 reqwest 默认 10 跳)。
            // 网盘 302 → CDN 必须跟随, 收紧到 5 跳并显式声明
            .redirect(reqwest::redirect::Policy::limited(5))
            // 方案 §29: 连接池参数
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_nodelay(true)
            .tcp_keepalive(Duration::from_secs(30))
            .no_proxy()
            .build()
            .expect("netdisk proxy client")
```

- [x] **Step 4: 回归**

Run: `cargo test -p quantumtv-core --lib netdisk` 与 `cargo test -p quantumtv-core --lib gateway`
Expected: 全部 PASS。

- [x] **Step 5: Commit**

```bash
git add crates/core/src/netdisk_proxy.rs
git commit -m "perf(gateway): 连接池参数对齐方案 §29 + Range 透传防回归测试"
```

---

### Task 10: 最终验收

**Files:** 无新改动;全量验证。

- [x] **Step 1: workspace Rust 测试**

Run(仓库根): `cargo test -p quantumtv-core`
Expected: 全部 PASS,包括 doc §45 要求的用例映射:
- BridgeSessionTest → `bridge::session::tests::session_multiplexes_concurrent_requests`
- BridgeTimeoutTest → `bridge::session::tests::session_timeout_returns_error_and_cleans_pending` + `tunnel::tests::bridge_timeout_returns_error_json`
- BridgeCancellationTest → `session_caller_cancelled_cleans_pending` + `session_tunnel_disconnect_fails_all_pending`
- PlaybackPriorityTest → `playback_resolve_not_blocked_by_search_queue`
- SearchCacheTest → `src-tauri commands::video::tests::bridge_search_cache_*`
- ResolveCacheTest / ResolveSingleFlightTest → `src-tauri commands::playback::tests::resolve_*`

- [x] **Step 2: src-tauri 测试**

Run: `cargo test --manifest-path src-tauri\Cargo.toml`
Expected: 全部 PASS。

- [x] **Step 3: 前端验收 (方案 §45)**

Run: `npm run typecheck`
Run: `npm run lint` (next lint 脚本在本仓库当前 Next 版本下失效,用 `npm run lint:strict` 替代并只要求零新增告警)
Run: `npm test` (仓库无任何 JS 测试文件 — 既有状态)
Expected: 全部通过/无回归。

- [ ] **Step 4: 手工冒烟 (有模拟器时, 方案 §38)** — 待人工执行

- 重复同一关键词搜索: 第二次全部命中缓存(Rust 日志无新的站点请求)
- 搜索过程中点击播放: `BridgePerf type=resolve` 日志 queue 接近 0ms
- 播放中切回搜索再播放同一集: `BridgePerf` 显示 resolve 未再次发出(缓存命中)

- [x] **Step 5: Commit (执行记录)**

```bash
git add docs/superpowers/plans/2026-09-12-bridge-playback-perf-optimization.md
```

---

## 已评估不做的事项 (记录理由)

- **PlaybackResolveContext 独立结构体 (方案 §11)**: 其全部字段 (`source_id/vod_id/episode_id/flag`) 已由 resolve key `resolve:{source}:{vod_id}:{flag}:{episode_id}` + `ResolveInput` 完整承载, 播放链路经 Task 7 后不再需要上下文在层间传递;引入结构体只会多一层转换。
- **Resolver Selection Cache (方案 §34)**: ResolverManager 仅遍历 4 个本地 resolver, 纯内存分支判断 (resolver.rs:361-364), 无 IO, 收益趋近零 — YAGNI。
- **Episode 数据模型加 resolver_hint (方案 §32-33)**: 当前解析路由由 `site_type + episode_id 形态` 确定性决定, 无"遍历猜测";引入 hint 字段只会增加两套真相。
- **前端 Bridge 状态轮询 (方案 §35)**: 现状已合规 — `BridgeSettings.tsx:63-78` 1s 轮询, ready/failed 或 120s 后自停, 无高频 setInterval;其余 1s 循环为 `player_tick` 进度保存, 与 Bridge 无关。
- **Android 端 SpiderExecutor 并发/缓存 (方案 §22-23)**: 设备端已有 4 线程池 + 单线程 detailExecutor, 队列天然缓冲;第一阶段只做 Desktop Cache (方案 §23 自己的建议)。APK 需重新分发, 不在本轮。
- **熔断 (方案 §19)**: BridgeSession 已有按类别超时 + 断连快速失败 + 指标日志;完整 Degraded/Unavailable 状态机待 P50/P95 指标观测后再决定是否引入 (方案 §46 定为 P2)。
- **`initialize_player_by_query` 的重搜 (§10)**: 已有 SearchCacheManager(1h) 兜底且仅"无 source/id 的标题兜底入口"触达;正常搜索→点播路径不含 Search。

---

## 执行记录 (2026-09-12)

全部 10 个 Task 已按序完成并提交 (dc91d52 → 8046e0e)。与计划的偏差与适配:

- Task 1 测试/实现收尾: tokio 版本无 `JoinHandle::is_running`, 优先级测试改为"排队搜索在短窗口内不得完成"的超时断言; 取消测试改为"pending 干净 + 额度随后立即可用"。`NoTunnel` 变体与 `inflight` 计数器在告警清理时移除 (无隧道路径走"关连接→reqwest 传输错误"的既有语义, `[BridgePerf]` 的 `pending=` 即权威观测值)。
- Task 2/3 测试: 两个端到端用例共享进程级全局 (ACTIVE/STARTED/HANDLES), 并行互踩, 按 gateway.rs `sessions_lock` 惯例加 `tunnel_lock()` 串行; keep-alive 后 `read_to_end` 改为按 Content-Length 的 `read_http_response` 辅助。
- Task 3 测试: 计划中"第三个请求验证 keep-alive 头"存在与 mock 退出的竞态, 实现时改为在第一个响应上断言 `Connection: keep-alive`。
- Task 4 测试: moka `try_get_with` 的 fetch 为 FnOnce, 测试改为每次新建闭包 (Box::pin 统一 Future 类型)。
- Task 7 enrich key 对齐: 首集缓存 key 的 flag 取 `play_groups[0].flag` (前端默认 activeGroupIndex=0), 否则与 `playback_play_episode` 的 key 无法共享; 用户切组时 key 不同 → 正确回退为重新解析。
- Task 8 前端: 流式事件先于 `search_page_open` 返回到达, 代际守卫实现为"单调递增最大值"语义 (低于已见最大代际即丢弃), 而非 `== currentGeneration`。
- Task 9 测试: reqwest 上线将头名小写化为 `range:`, 上游 mock 断言改为大小写不敏感 (代理自身提取即大小写不敏感)。

验收结果: quantumtv-core 141+2 全过; src-tauri 270 全过; `npm run typecheck` 干净。既有环境问题 (与本次无关, 已验证基线同样失败): `npm test` (jest 无任何测试文件), `next lint` 脚本失效, `lint:strict` 存量 console 告警 (改动文件基线 12 = 改后 12, 零新增), api-server `config_file_test` 依赖不存在的 `crates/api-server/data.json`。方案 §38 冒烟需模拟器+APK, 待人工执行。
