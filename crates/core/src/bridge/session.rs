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

// ---- 按类别超时 ----
// 方案 §18 的理想值 (8/8/15/2s) 实测对 wex 系 Android spider 过激:
// 真机 PGBM10 上 /search 普遍 >8s, 夸克 /playerContent >15s (旧行为靠 spider 层
// 60s/120s 硬扛成功)。取"设备可完成 + 低于旧外层上限"的折中值, 仍保证有界。
pub(crate) const SEARCH_TIMEOUT: Duration = Duration::from_secs(45);
pub(crate) const DETAIL_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const RESOLVE_TIMEOUT: Duration = Duration::from_secs(60);
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RequestError {
    Timeout,
    TunnelClosed,
    SendFailed,
}

impl RequestError {
    /// 转 spider 层可识别的业务错误 JSON (code!=200 即错, spider/mod.rs:460)
    pub(crate) fn to_err_json(&self) -> String {
        let (code, msg) = match self {
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
}

/// 长期桥接会话: 一条隧道连接上的全部逻辑请求复用 (方案 §4)
pub(crate) struct BridgeSession {
    pub device: String,
    writer: mpsc::Sender<Frame>,
    pending: StdMutex<HashMap<u32, PendingEntry>>,
    next_id: AtomicU32,
    search_gate: Arc<Semaphore>,
    detail_gate: Arc<Semaphore>,
    resolve_gate: Arc<Semaphore>,
    limits: Limits,
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
            limits,
        })
    }

    pub(crate) fn pending_count(&self) -> usize {
        self.pending.lock().unwrap().len()
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
        self.pending.lock().unwrap().insert(id, PendingEntry { tx });

        if self.writer.send(Frame { id, payload }).await.is_err() {
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
        // 迟到的响应不再有等待者: deliver 后 pending 干净 (方案 §5 的泄漏关注点)
        session.deliver(f.id, b"late".to_vec());
        assert_eq!(session.pending_count(), 0);
        // 取消释放了类别额度: 后续请求立即可发 (不会被卡额度)
        let s = session.clone();
        let h2 = tokio::spawn(async move { s.request(RequestKind::Search, b"next".to_vec()).await });
        let f2 = tokio::time::timeout(Duration::from_millis(300), rx.recv())
            .await
            .expect("取消后额度必须立即可用")
            .unwrap();
        session.deliver(f2.id, b"ok".to_vec());
        assert_eq!(h2.await.unwrap().unwrap(), b"ok".to_vec());
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
        // 排队中的搜索仍未获得额度: 短窗口内不可能完成
        assert!(tokio::time::timeout(Duration::from_millis(150), search2).await.is_err());

        // 清理: 放行所有挂起请求, 避免 await 悬挂
        session.fail_all_pending("test_end");
        let _ = tokio::time::timeout(Duration::from_millis(500), search1).await;
    }
}
