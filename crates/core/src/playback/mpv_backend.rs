//! MpvBackend: mpv 子进程生命周期 + JSON IPC 命名管道封装
//!
//! V2 Phase 4: 从 src-tauri/commands/mpv_embed.rs (方案 C) 迁入 playback 域。
//! 独立窗口 + JSON IPC (--input-ipc-server 命名管道), 非 Windows stub。
//! 与 mpv_embed.rs 的差异: 不依赖 tauri (依赖方向 Core ⊅ tauri), 只暴露
//! [`MpvBackend`] 给 PlaybackManager; 事件统一经回调上抛, 事件发送由
//! Tauri 接线层完成。mpv.exe 查找用 [`locate_mpv`], 由调用方传入目录。

use super::state::MpvEvent;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::sync::mpsc;

/// Windows 命名管道名 (保持与旧 mpv_embed 一致, 兼容期同一进程只有一个 mpv)
pub const PIPE_NAME: &str = r"\\.\pipe\quantumtv-mpv-embed";

/// IPC 命令超时 (Phase 7 命令级超时): 命令实际写入管道后 start 计时,
/// 超时未收到 mpv 响应 → CommandFailed 事件 (loadfile 活跃态转 Error)
const COMMAND_TIMEOUT_MS_DEFAULT: u64 = 5000;

/// 待发/待确认命令 (Phase 7: 带 request_id 供响应关联与超时判定)
struct OutboundCmd {
    id: u32,
    desc: String,
    line: String,
}

#[derive(Debug)]
pub struct MpvLaunchResult {
    pub launched: bool,
    pub reused: bool,
    pub mpv_path: String,
}

/// mpv 子进程 + IPC 写通道; 进程退出时兜底 kill。
/// 内部状态在 Arc 中, Clone 廉价 (管道任务需要 'static 引用)。
#[derive(Clone, Default)]
pub struct MpvBackend {
    inner: Arc<MpvBackendInner>,
}

impl Default for MpvBackendInner {
    fn default() -> Self {
        Self {
            child: StdMutex::new(None),
            writer: StdMutex::new(None),
            pending: StdMutex::new(Vec::new()),
            pipe_connected: StdMutex::new(false),
            awaiting: StdMutex::new(HashMap::new()),
            next_id: AtomicU32::new(0),
            event_sink: StdMutex::new(None),
            command_timeout_ms: AtomicU64::new(COMMAND_TIMEOUT_MS_DEFAULT),
        }
    }
}

pub(crate) struct MpvBackendInner {
    child: StdMutex<Option<std::process::Child>>,
    writer: StdMutex<Option<mpsc::UnboundedSender<OutboundCmd>>>,
    pending: StdMutex<Vec<OutboundCmd>>,
    pipe_connected: StdMutex<bool>,
    /// 已写入管道、等待 mpv 响应的命令 (request_id → 命令描述)
    awaiting: StdMutex<HashMap<u32, String>>,
    next_id: AtomicU32,
    /// 事件上抛 (watchdog 需 'static 引用, launch 时注入)
    event_sink: StdMutex<Option<Arc<dyn Fn(MpvEvent) + Send + Sync>>>,
    command_timeout_ms: AtomicU64,
}

/// mpv.exe 查找顺序: 环境变量 QUANTUMTV_MPV_PATH → <app_data>/mpv →
/// <exe_dir>/mpv → PATH。app_data 由调用方 (Tauri 层) 传入。
pub fn locate_mpv(app_data_dir: Option<&Path>) -> Option<PathBuf> {
    if let Ok(p) = std::env::var("QUANTUMTV_MPV_PATH") {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Some(data_dir) = app_data_dir {
        let path = data_dir.join("mpv").join("mpv.exe");
        if path.is_file() {
            return Some(path);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let path = dir.join("mpv").join("mpv.exe");
            if path.is_file() {
                return Some(path);
            }
        }
    }
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("mpv.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

impl MpvBackend {
    /// 进程活着 → 本次 launch 只需复用 (换源经 send_command loadfile);
    /// 否则 spawn + 建管道。url 作为首个待发命令由 manager 下发 (统一走
    /// pending 队列, 避免 spawn 与管道就绪的时序竞态)。
    #[cfg(windows)]
    pub async fn launch(
        &self,
        app_data_dir: Option<&Path>,
        on_event: impl Fn(MpvEvent) + Send + Sync + 'static,
    ) -> Result<MpvLaunchResult, String> {
        let mpv = locate_mpv(app_data_dir).ok_or_else(|| {
            "未找到 mpv 播放器。请把 mpv.exe 放到应用数据目录 mpv/ 下".to_string()
        })?;

        // 进程活着 → 复用 (不重复 spawn)
        {
            let mut guard = self.inner.child.lock().unwrap();
            if let Some(child) = guard.as_mut() {
                if child.try_wait().map(|s| s.is_none()).unwrap_or(false) {
                    return Ok(MpvLaunchResult {
                        launched: true,
                        reused: true,
                        mpv_path: mpv.display().to_string(),
                    });
                }
                *guard = None;
            }
        }
        *self.inner.pipe_connected.lock().unwrap() = false;

        // 独立窗口模式 (方案 C): 宿主不经手窗口操作, 控制经 JSON IPC。
        // --idle=yes 启动空载, 播放内容统一由 manager 的 loadfile 下发。
        let child = std::process::Command::new(&mpv)
            .args([
                "--force-window=immediate".to_string(),
                "--ontop".to_string(),
                format!("--input-ipc-server={PIPE_NAME}"),
                "--idle=yes".to_string(),
                "--keep-open=always".to_string(),
                "--hwdec=auto-safe".to_string(),
                "--cache=yes".to_string(),
                "--demuxer-max-bytes=64MiB".to_string(),
                "--demuxer-readahead-secs=20".to_string(),
                "--no-save-position-on-quit".to_string(),
                "--volume-max=300".to_string(),
            ])
            .spawn()
            .map_err(|e| format!("启动 mpv 失败 ({}): {e}", mpv.display()))?;
        *self.inner.child.lock().unwrap() = Some(child);
        self.spawn_pipe_task(on_event);
        Ok(MpvLaunchResult {
            launched: true,
            reused: false,
            mpv_path: mpv.display().to_string(),
        })
    }

    #[cfg(not(windows))]
    pub async fn launch(
        &self,
        _app_data_dir: Option<&Path>,
        _on_event: impl Fn(MpvEvent) + Send + Sync + 'static,
    ) -> Result<MpvLaunchResult, String> {
        Err("mpv 播放仅支持 Windows".into())
    }

    /// 进程活着则复用 (不重新 spawn)
    pub fn is_alive(&self) -> bool {
        let mut guard = self.inner.child.lock().unwrap();
        match guard.as_mut() {
            Some(child) => {
                if child.try_wait().map(|s| s.is_none()).unwrap_or(false) {
                    true
                } else {
                    *guard = None;
                    false
                }
            }
            None => false,
        }
    }

    /// 分配 request_id 并入队 (管道未就绪时进待发队列)。
    /// awaiting 登记与超时看门狗在命令实际写入管道后启动 (writer 任务),
    /// 避免排队期 (管道连接最长 10s) 误判超时。
    pub fn send_command(&self, cmd: &[serde_json::Value]) -> Result<(), String> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let mut obj = serde_json::json!({ "command": cmd });
        obj["request_id"] = serde_json::json!(id);
        let out = OutboundCmd {
            id,
            desc: command_desc(cmd),
            line: obj.to_string(),
        };
        if let Some(tx) = self.inner.writer.lock().unwrap().clone() {
            tx.send(out).map_err(|_| "mpv IPC 已断开".to_string())
        } else {
            self.inner.pending.lock().unwrap().push(out);
            Ok(())
        }
    }

    /// 优雅 quit → 超时 kill
    pub fn shutdown(&self) {
        if let Some(tx) = self.inner.writer.lock().unwrap().clone() {
            let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
            let _ = tx.send(OutboundCmd {
                id,
                desc: "quit".into(),
                line: serde_json::json!({ "command": ["quit"], "request_id": id }).to_string(),
            });
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
        if let Some(mut child) = self.inner.child.lock().unwrap().take() {
            let exited = child.try_wait().map(|s| s.is_some()).unwrap_or(false);
            if !exited {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        self.inner.pending.lock().unwrap().clear();
        self.inner.awaiting.lock().unwrap().clear();
        *self.inner.pipe_connected.lock().unwrap() = false;
    }

    /// 进程退出兜底 (app 退出时调用)
    pub fn kill_now(&self) {
        if let Some(mut child) = self.inner.child.lock().unwrap().take() {
            let alive = child.try_wait().map(|s| s.is_none()).unwrap_or(false);
            if alive {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    /// 管道就绪状态 (观测)
    pub fn pipe_connected(&self) -> bool {
        *self.inner.pipe_connected.lock().unwrap()
    }

    #[cfg(test)]
    pub(crate) fn set_event_sink(&self, sink: Arc<dyn Fn(MpvEvent) + Send + Sync>) {
        *self.inner.event_sink.lock().unwrap() = Some(sink);
    }

    #[cfg(test)]
    pub(crate) fn set_command_timeout_ms(&self, ms: u64) {
        self.inner.command_timeout_ms.store(ms, Ordering::Relaxed);
    }

    /// 连接命名管道: 订阅播放状态属性 → 独立写任务消费命令通道 →
    /// 读循环把 mpv 事件翻译成 MpvEvent 经回调上抛。管道关闭 = mpv 退出。
    /// Phase 7: 命令带 request_id, 写入后登记 awaiting + 超时看门狗,
    /// 响应由读循环派发 (超时/mpv 报错 → CommandFailed)。
    #[cfg(windows)]
    fn spawn_pipe_task(&self, on_event: impl Fn(MpvEvent) + Send + Sync + 'static) {
        // 读写任务需要 'static 的 backend 引用
        let this = self.inner.clone();
        let on_event = Arc::new(on_event);
        *this.event_sink.lock().unwrap() = Some(on_event.clone());
        // Core 不依赖 tauri: 直接用 tokio 运行时 spawn (Tauri app 内即
        // tauri::async_runtime 的同一运行时)
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
            use tokio::net::windows::named_pipe::ClientOptions;

            // mpv 启动到建好管道服务端有间隔, 最多等 10s
            let mut pipe = None;
            for _ in 0..100 {
                match ClientOptions::new().open(PIPE_NAME) {
                    Ok(c) => {
                        pipe = Some(c);
                        break;
                    }
                    Err(_) => {
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                }
            }
            let Some(pipe) = pipe else {
                on_event(MpvEvent::ProcessDead);
                return;
            };
            let (reader, mut write_half) = tokio::io::split(pipe);

            for (id, name) in [
                (1, "playback-time"),
                (2, "duration"),
                (3, "pause"),
                (4, "eof-reached"),
            ] {
                let line =
                    format!("{{\"command\":[\"observe_property\",{id},\"{name}\"]}}\n");
                let _ = write_half.write_all(line.as_bytes()).await;
            }

            let (tx, mut rx) = mpsc::unbounded_channel::<OutboundCmd>();
            *this.writer.lock().unwrap() = Some(tx);
            *this.pipe_connected.lock().unwrap() = true;
            let this_writer = this.clone();
            let writer_task = tokio::spawn(async move {
                while let Some(out) = rx.recv().await {
                    if write_half.write_all(out.line.as_bytes()).await.is_err() {
                        break;
                    }
                    if write_half.write_all(b"\n").await.is_err() {
                        break;
                    }
                    let _ = write_half.flush().await;
                    // 命令已实际送达: 登记 pending + 启动超时看门狗
                    on_command_written(&this_writer, &out);
                }
            });

            // 补发连接前积压的命令 (如首个 loadfile)
            {
                let mut pending = this.pending.lock().unwrap();
                if let Some(tx) = this.writer.lock().unwrap().clone() {
                    for out in pending.drain(..) {
                        let _ = tx.send(out);
                    }
                }
            }

            let mut last_time_emit =
                std::time::Instant::now() - std::time::Duration::from_secs(1);
            let mut lines = BufReader::new(reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                    continue;
                };
                if v.get("event").is_none() {
                    // 命令响应行 (Phase 7): 按 request_id 派发
                    if let Some((id, error)) = extract_response(&v) {
                        let fired = {
                            let mut awaiting = this.awaiting.lock().unwrap();
                            resolve_response(&mut awaiting, id, &error)
                        };
                        if let Some(ev) = fired {
                            on_event(ev);
                        }
                    }
                    continue;
                }
                match v.get("event").and_then(|e| e.as_str()) {
                    Some("property-change") => {
                        let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("");
                        match name {
                            "playback-time" => {
                                if let Some(t) = v.get("data").and_then(|d| d.as_f64()) {
                                    // playback-time 逐条推送, 节流到 500ms
                                    // (前端播放页组件树大, 全量重渲是卡顿主因)
                                    if last_time_emit.elapsed()
                                        >= std::time::Duration::from_millis(500)
                                    {
                                        last_time_emit = std::time::Instant::now();
                                        on_event(MpvEvent::Time(t));
                                    }
                                }
                            }
                            "duration" => {
                                if let Some(d) = v.get("data").and_then(|d| d.as_f64()) {
                                    on_event(MpvEvent::Duration(d));
                                }
                            }
                            "pause" => {
                                let paused =
                                    v.get("data").and_then(|d| d.as_bool()).unwrap_or(false);
                                on_event(MpvEvent::Pause(paused));
                            }
                            "eof-reached" => {
                                if v.get("data").and_then(|d| d.as_bool()).unwrap_or(false) {
                                    on_event(MpvEvent::EofReached);
                                }
                            }
                            _ => {}
                        }
                    }
                    Some("file-loaded") => on_event(MpvEvent::FileLoaded),
                    Some("end-file") => {
                        let reason =
                            v.get("reason").and_then(|r| r.as_str()).unwrap_or("");
                        let error = v.get("error").and_then(|e| e.as_str()).unwrap_or("");
                        if reason == "error" {
                            on_event(MpvEvent::PlaybackError(
                                if error.is_empty() { "mpv 播放失败" } else { error }.into(),
                            ));
                        }
                        // quit/eof/redirect 等正常原因由 eof/dead 事件覆盖
                    }
                    _ => {}
                }
            }

            // 管道关闭 = mpv 退出
            writer_task.abort();
            *this.writer.lock().unwrap() = None;
            *this.pipe_connected.lock().unwrap() = false;
            on_event(MpvEvent::ProcessDead);
        });
    }
}

/// 命令已实际写入管道: 登记 awaiting + 启动超时看门狗 (Phase 7)。
/// 超时到达时仍在 awaiting → CommandFailed("命令超时");
/// 提前响应/管道死亡/清理都会移除登记, 看门狗静默结束。
fn on_command_written(inner: &Arc<MpvBackendInner>, out: &OutboundCmd) {
    inner
        .awaiting
        .lock()
        .unwrap()
        .insert(out.id, out.desc.clone());
    let this = inner.clone();
    let id = out.id;
    let desc = out.desc.clone();
    let timeout_ms = inner.command_timeout_ms.load(Ordering::Relaxed);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(timeout_ms)).await;
        let timed_out = this.awaiting.lock().unwrap().remove(&id).is_some();
        if timed_out {
            if let Some(sink) = this.event_sink.lock().unwrap().clone() {
                sink(MpvEvent::CommandFailed {
                    command: desc,
                    error: "命令超时".into(),
                });
            }
        }
    });
}

/// mpv 命令响应行 → (request_id, error)。事件行与无 request_id 的响应
/// (observe_property 初始订阅) 不关联。
fn extract_response(v: &serde_json::Value) -> Option<(u32, String)> {
    if v.get("event").is_some() {
        return None;
    }
    let id = v.get("request_id")?.as_u64()? as u32;
    let error = v
        .get("error")
        .and_then(|e| e.as_str())
        .unwrap_or("unknown");
    Some((id, error.to_string()))
}

/// 命令描述: 取命令数组首元素 (日志/事件呈现用)
fn command_desc(cmd: &[serde_json::Value]) -> String {
    cmd.first()
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string()
}

/// 响应派发: 移除 pending; 仅 mpv 报错时产生 CommandFailed。
/// 成功响应静默了结; 未知 id (未跟踪命令) 忽略。
fn resolve_response(
    awaiting: &mut HashMap<u32, String>,
    id: u32,
    error: &str,
) -> Option<MpvEvent> {
    let desc = awaiting.remove(&id)?;
    (error != "success").then(|| MpvEvent::CommandFailed {
        command: desc,
        error: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback::state::MpvEvent as Ev;

    #[test]
    fn extract_response_parses_request_id_and_error() {
        let v = serde_json::from_str::<serde_json::Value>(
            r#"{"event":"x"}"#,
        )
        .unwrap();
        assert!(extract_response(&v).is_none(), "事件行不是命令响应");
        let v =
            serde_json::from_str::<serde_json::Value>(r#"{"error":"success"}"#).unwrap();
        assert!(extract_response(&v).is_none(), "无 request_id 不关联");
        let v = serde_json::from_str::<serde_json::Value>(
            r#"{"request_id":3,"error":"success"}"#,
        )
        .unwrap();
        assert_eq!(extract_response(&v), Some((3, "success".to_string())));
        let v = serde_json::from_str::<serde_json::Value>(
            r#"{"request_id":9,"error":"loading failed"}"#,
        )
        .unwrap();
        assert_eq!(extract_response(&v), Some((9, "loading failed".to_string())));
    }

    #[test]
    fn command_desc_extracts_first_element() {
        let cmd = vec![
            serde_json::json!("loadfile"),
            serde_json::json!("http://x/v.m3u8"),
            serde_json::json!("replace"),
        ];
        assert_eq!(command_desc(&cmd), "loadfile");
        assert_eq!(command_desc(&[]), "unknown");
    }

    #[test]
    fn resolve_response_fires_only_on_error() {
        let mut awaiting = HashMap::new();
        awaiting.insert(3u32, "loadfile".to_string());
        // 成功响应: 移除但不产生事件
        let ev = resolve_response(&mut awaiting, 3, "success");
        assert!(ev.is_none());
        assert!(!awaiting.contains_key(&3), "响应后应移除 pending");
        // 未知 id (observe_property 等未跟踪命令): 忽略
        let ev = resolve_response(&mut awaiting, 77, "success");
        assert!(ev.is_none());
        // mpv 报错: 产生 CommandFailed
        awaiting.insert(9u32, "loadfile".to_string());
        let ev = resolve_response(&mut awaiting, 9, "loading failed");
        assert_eq!(
            ev,
            Some(Ev::CommandFailed {
                command: "loadfile".into(),
                error: "loading failed".into()
            })
        );
    }

    #[tokio::test(start_paused = true)]
    async fn command_timeout_fires_command_failed() {
        // 无真实管道: 直接走"命令已写入管道"路径 (与 writer 任务一致)
        let backend = MpvBackend::default();
        let got: Arc<StdMutex<Vec<Ev>>> = Arc::new(StdMutex::new(Vec::new()));
        let sink = got.clone();
        backend.set_command_timeout_ms(20);
        backend.set_event_sink(Arc::new(move |ev| sink.lock().unwrap().push(ev)));
        let out = OutboundCmd {
            id: 0,
            desc: "loadfile".into(),
            line: r#"{"command":["loadfile","x"],"request_id":0}"#.into(),
        };
        on_command_written(&backend.inner, &out);
        tokio::time::sleep(Duration::from_millis(50)).await;
        let events = got.lock().unwrap().clone();
        assert_eq!(
            events,
            vec![Ev::CommandFailed {
                command: "loadfile".into(),
                error: "命令超时".into()
            }]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn command_resolved_before_timeout_does_not_fire() {
        let backend = MpvBackend::default();
        let got: Arc<StdMutex<Vec<Ev>>> = Arc::new(StdMutex::new(Vec::new()));
        let sink = got.clone();
        backend.set_command_timeout_ms(20);
        backend.set_event_sink(Arc::new(move |ev| sink.lock().unwrap().push(ev)));
        let out = OutboundCmd {
            id: 0,
            desc: "seek".into(),
            line: r#"{"command":["seek",30],"request_id":0}"#.into(),
        };
        on_command_written(&backend.inner, &out);
        // 响应在超时前到达 (模拟读循环派发)
        let ev = {
            let mut awaiting = backend.inner.awaiting.lock().unwrap();
            resolve_response(&mut awaiting, 0, "success")
        };
        assert!(ev.is_none());
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(got.lock().unwrap().is_empty(), "已响应的命令超时后不得再报");
    }

    #[test]
    fn queued_command_without_pipe_does_not_time_out() {
        // 管道未连接时命令只在待发队列, 不登记 awaiting:
        // 超时语义归 ProcessDead (10s 连接失败) 管, 避免慢连接误报
        let backend = MpvBackend::default();
        backend
            .send_command(&[serde_json::json!("loadfile"), serde_json::json!("x")])
            .unwrap();
        assert!(backend.inner.awaiting.lock().unwrap().is_empty());
        assert_eq!(backend.inner.pending.lock().unwrap().len(), 1);
    }
}
