//! MpvBackend: mpv 子进程生命周期 + JSON IPC 命名管道封装
//!
//! V2 Phase 4: 从 src-tauri/commands/mpv_embed.rs (方案 C) 迁入 playback 域。
//! 独立窗口 + JSON IPC (--input-ipc-server 命名管道), 非 Windows stub。
//! 与 mpv_embed.rs 的差异: 不依赖 tauri (依赖方向 Core ⊅ tauri), 只暴露
//! [`MpvBackend`] 给 PlaybackManager; 事件统一经回调上抛, 事件发送由
//! Tauri 接线层完成。mpv.exe 查找用 [`locate_mpv`], 由调用方传入目录。

use super::state::MpvEvent;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::mpsc;

/// Windows 命名管道名 (保持与旧 mpv_embed 一致, 兼容期同一进程只有一个 mpv)
pub const PIPE_NAME: &str = r"\\.\pipe\quantumtv-mpv-embed";

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

#[derive(Default)]
pub(crate) struct MpvBackendInner {
    child: StdMutex<Option<std::process::Child>>,
    writer: StdMutex<Option<mpsc::UnboundedSender<String>>>,
    pending: StdMutex<Vec<String>>,
    pipe_connected: StdMutex<bool>,
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
        on_event: impl Fn(MpvEvent) + Send + 'static,
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
        _on_event: impl Fn(MpvEvent) + Send + 'static,
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

    /// 发送 mpv JSON IPC 命令; 管道未就绪时进待发队列
    pub fn send_command(&self, cmd: &[serde_json::Value]) -> Result<(), String> {
        let line = serde_json::json!({ "command": cmd }).to_string();
        if let Some(tx) = self.inner.writer.lock().unwrap().clone() {
            tx.send(line).map_err(|_| "mpv IPC 已断开".to_string())
        } else {
            self.inner.pending.lock().unwrap().push(line);
            Ok(())
        }
    }

    /// 优雅 quit → 超时 kill
    pub fn shutdown(&self) {
        if let Some(tx) = self.inner.writer.lock().unwrap().clone() {
            let _ = tx.send(serde_json::json!({ "command": ["quit"] }).to_string());
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

    /// 连接命名管道: 订阅播放状态属性 → 独立写任务消费命令通道 →
    /// 读循环把 mpv 事件翻译成 MpvEvent 经回调上抛。管道关闭 = mpv 退出。
    #[cfg(windows)]
    fn spawn_pipe_task(&self, on_event: impl Fn(MpvEvent) + Send + 'static) {
        // 读写任务需要 'static 的 backend 引用
        let this = self.inner.clone();
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

            let (tx, mut rx) = mpsc::unbounded_channel::<String>();
            *this.writer.lock().unwrap() = Some(tx);
            *this.pipe_connected.lock().unwrap() = true;
            let writer_task = tokio::spawn(async move {
                while let Some(line) = rx.recv().await {
                    if write_half.write_all(line.as_bytes()).await.is_err() {
                        break;
                    }
                    if write_half.write_all(b"\n").await.is_err() {
                        break;
                    }
                    let _ = write_half.flush().await;
                }
            });

            // 补发连接前积压的命令 (如首个 loadfile)
            {
                let mut pending = this.pending.lock().unwrap();
                if let Some(tx) = this.writer.lock().unwrap().clone() {
                    for line in pending.drain(..) {
                        let _ = tx.send(line);
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
