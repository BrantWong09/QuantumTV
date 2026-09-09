//! mpv 受控播放 (方案 C, Windows):
//! mpv 以**独立窗口**运行, 宿主不经手任何窗口操作 (无 --wid、无子窗口、
//! 无 rect 同步) —— airspace/焦点/z 序三类问题从结构上不存在。
//! 控制经 JSON IPC 命名管道 (mpv_embed_command), 播放状态经
//! observe_property 回传前端 (mpv-embed-event, 时间类 500ms 节流)。
//! 方案 B (--wid 嵌入) 实测七轮仍无法在 WebView2 环境稳定工作
//! (内层渲染窗口截获输入/分层子窗口创建失败), 详见
//! docs/research-mpv-integration.md §8。
//! 非 Windows 平台保留 stub, 调用返回明确错误。

use serde::Serialize;
use std::sync::Mutex as StdMutex;
use tauri::Manager;

pub const PIPE_NAME: &str = r"\\.\pipe\quantumtv-mpv-embed";

#[derive(Debug, Serialize)]
pub struct MpvLaunchResult {
    pub launched: bool,
    pub reused: bool,
    pub mpv_path: String,
}

#[derive(Default)]
#[cfg_attr(not(windows), allow(dead_code))]
pub struct MpvEmbedState {
    /// mpv 子进程 (同一时间一个)
    child: StdMutex<Option<std::process::Child>>,
    /// IPC 命令发送端 (mpv 就绪后由管道任务填充)
    writer: StdMutex<Option<tokio::sync::mpsc::UnboundedSender<String>>>,
    /// 管道连接前的待发命令
    pending: StdMutex<Vec<String>>,
}

impl MpvEmbedState {
    /// 进程退出时兜底杀掉 mpv 子进程
    pub fn shutdown(&self) {
        let Some(mut child) = self.child.lock().unwrap().take() else {
            return;
        };
        let alive = child.try_wait().map(|s| s.is_none()).unwrap_or(false);
        if alive {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

// ---------------------------------------------------------------------------
// Tauri 命令
// ---------------------------------------------------------------------------

/// 拉起/复用 mpv 播放进程。换源由前端经 mpv_embed_command 下发
/// loadfile (进程活着就复用, 不重复 spawn)。
#[tauri::command]
pub async fn mpv_embed_launch(
    app: tauri::AppHandle,
    state: tauri::State<'_, MpvEmbedState>,
    url: String,
) -> Result<MpvLaunchResult, String> {
    #[cfg(windows)]
    {
        return embed_launch(&app, &state, &url).await;
    }
    #[cfg(not(windows))]
    {
        let _ = (&app, &state, &url);
        Err("mpv 播放仅支持 Windows".into())
    }
}

/// 透传 mpv JSON IPC 命令, 如 ["loadfile", url, "replace", "start=12.5"]、
/// ["cycle","pause"]、["seek",10] 等。管道未就绪时命令进待发队列。
#[tauri::command]
pub async fn mpv_embed_command(
    state: tauri::State<'_, MpvEmbedState>,
    cmd: Vec<serde_json::Value>,
) -> Result<(), String> {
    let line = serde_json::json!({ "command": cmd }).to_string();
    if let Some(tx) = state.writer.lock().unwrap().clone() {
        tx.send(line).map_err(|_| "mpv IPC 已断开".to_string())
    } else {
        state.pending.lock().unwrap().push(line);
        Ok(())
    }
}

/// 退出 mpv 播放: 优雅 quit → 超时 kill。
#[tauri::command]
pub async fn mpv_embed_close(state: tauri::State<'_, MpvEmbedState>) -> Result<(), String> {
    if let Some(tx) = state.writer.lock().unwrap().clone() {
        let _ = tx.send(serde_json::json!({ "command": ["quit"] }).to_string());
    }
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    if let Some(mut child) = state.child.lock().unwrap().take() {
        let exited = child.try_wait().map(|s| s.is_some()).unwrap_or(false);
        if !exited {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    state.pending.lock().unwrap().clear();
    Ok(())
}

// ---------------------------------------------------------------------------
// Windows 实现
// ---------------------------------------------------------------------------

#[cfg(windows)]
async fn embed_launch(
    app: &tauri::AppHandle,
    state: &tauri::State<'_, MpvEmbedState>,
    url: &str,
) -> Result<MpvLaunchResult, String> {
    let mpv = crate::commands::mpv_player::locate_mpv(app).ok_or_else(|| {
        "未找到 mpv 播放器。请把 mpv.exe 放到应用数据目录 mpv/ 下".to_string()
    })?;

    // 进程活着 → 前端直接经 IPC loadfile 换源, 不重复 spawn
    {
        let mut guard = state.child.lock().unwrap();
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

    // 独立窗口模式: --force-window=immediate 保证有窗口可被脚本控制;
    // --keep-open=always 播完停在末帧 (eof-reached 置 true 由前端连播);
    // --ontop 播放期间置顶便于跟随应用; 输入保留 mpv 默认键位 (独立窗口
    // 需要本地操作能力), 控制主力仍是 DOM 遥控面板 + IPC。
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
            url.to_string(),
        ])
        .spawn()
        .map_err(|e| format!("启动 mpv 失败 ({}): {e}", mpv.display()))?;
    *state.child.lock().unwrap() = Some(child);
    spawn_pipe_task(app.clone());
    Ok(MpvLaunchResult {
        launched: true,
        reused: false,
        mpv_path: mpv.display().to_string(),
    })
}

/// 连接 mpv 的命名管道: 先发 observe_property 订阅播放状态, 再起独立写任务
/// 消费命令通道 (含连接前积压命令)。管道关闭 = mpv 退出 → 通知前端。
#[cfg(windows)]
fn spawn_pipe_task(app: tauri::AppHandle) {
    use tauri::Emitter;

    tauri::async_runtime::spawn(async move {
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
        let state = app.state::<MpvEmbedState>();
        let Some(pipe) = pipe else {
            *state.writer.lock().unwrap() = None;
            state.pending.lock().unwrap().clear();
            let _ = app.emit("mpv-embed-event", serde_json::json!({ "kind": "dead" }));
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

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        *state.writer.lock().unwrap() = Some(tx);
        let writer_task = tauri::async_runtime::spawn(async move {
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
            let mut pending = state.pending.lock().unwrap();
            if let Some(tx) = state.writer.lock().unwrap().clone() {
                for line in pending.drain(..) {
                    let _ = tx.send(line);
                }
            }
        }

        let app2 = app.clone();
        let mut last_time_emit =
            std::time::Instant::now() - std::time::Duration::from_secs(1);
        let mut last_duration: f64 = 0.0;
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
                                // playback-time 逐条事件推送, 节流到 500ms:
                                // 每条都 emit → 前端 setMpvState → 播放页
                                // (3000+ 行组件树) 全量重渲, 是卡顿主因之一
                                if last_time_emit.elapsed()
                                    >= std::time::Duration::from_millis(500)
                                {
                                    last_time_emit = std::time::Instant::now();
                                    let _ = app2.emit(
                                        "mpv-embed-event",
                                        serde_json::json!({
                                            "kind": "time",
                                            "time": t,
                                            "duration": last_duration
                                        }),
                                    );
                                }
                            }
                        }
                        "duration" => {
                            if let Some(d) = v.get("data").and_then(|d| d.as_f64()) {
                                last_duration = d;
                                let _ = app2.emit(
                                    "mpv-embed-event",
                                    serde_json::json!({ "kind": "duration", "duration": d }),
                                );
                            }
                        }
                        "pause" => {
                            let paused =
                                v.get("data").and_then(|d| d.as_bool()).unwrap_or(false);
                            let _ = app2.emit(
                                "mpv-embed-event",
                                serde_json::json!({ "kind": "pause", "value": paused }),
                            );
                        }
                        "eof-reached" => {
                            if v.get("data").and_then(|d| d.as_bool()).unwrap_or(false) {
                                let _ = app2.emit(
                                    "mpv-embed-event",
                                    serde_json::json!({ "kind": "eof" }),
                                );
                            }
                        }
                        _ => {}
                    }
                }
                Some("file-loaded") => {
                    let _ = app2.emit(
                        "mpv-embed-event",
                        serde_json::json!({ "kind": "file-loaded" }),
                    );
                }
                _ => {}
            }
        }

        // 管道关闭 = mpv 退出
        writer_task.abort();
        *state.writer.lock().unwrap() = None;
        state.pending.lock().unwrap().clear();
        let _ = app2.emit("mpv-embed-event", serde_json::json!({ "kind": "dead" }));
    });
}

#[cfg(not(windows))]
fn spawn_pipe_task(_app: tauri::AppHandle) {
    let _ = _app;
}
