//! mpv --wid 嵌入播放 (方案 B, Windows):
//! 在主窗口客户区内创建一个 STATIC 子窗口作为 mpv 的渲染宿主
//! (mpv --wid=<hwnd>, 渲染铺满该窗口), 前端按视频区域 rect 经
//! mpv_embed_sync 同步位置/尺寸; 控制经 JSON IPC 命名管道
//! (mpv_embed_command); 播放状态经 observe_property 回传前端
//! (mpv-embed-event 事件, 时间类 200ms 节流)。
//! 子窗口盖在 WebView2 之上 (airspace): 视频区底部预留 MPV_CONTROL_BAR_H
//! 高度给 DOM 控制条, 不被子窗口遮挡。
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
    /// STATIC 宿主子窗口句柄 (原始 isize, 进程内创建一次复用)
    hwnd: StdMutex<Option<isize>>,
    /// IPC 命令发送端 (mpv 就绪后由管道任务填充)
    writer: StdMutex<Option<tokio::sync::mpsc::UnboundedSender<String>>>,
    /// 管道连接前的待发命令
    pending: StdMutex<Vec<String>>,
}

impl MpvEmbedState {
    /// 进程退出时兜底杀掉 mpv 子进程 (窗口随进程销毁, 无需显式清理)
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

/// 拉起/复用 mpv 嵌入播放进程。换源由前端经 mpv_embed_command 下发
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
        Err("mpv 嵌入播放仅支持 Windows".into())
    }
}

/// 同步 mpv 宿主子窗口的位置/尺寸 (前端 CSS 坐标 × devicePixelRatio)。
#[tauri::command]
pub async fn mpv_embed_sync(
    state: tauri::State<'_, MpvEmbedState>,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    scale: f64,
) -> Result<(), String> {
    #[cfg(windows)]
    {
        return sync_window(&state, x, y, w, h, scale);
    }
    #[cfg(not(windows))]
    {
        let _ = (&state, &x, &y, &w, &h, &scale);
        Err("mpv 嵌入播放仅支持 Windows".into())
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

/// 退出 mpv 嵌入播放: 优雅 quit → 超时 kill, 隐藏宿主窗口。
#[tauri::command]
pub async fn mpv_embed_close(state: tauri::State<'_, MpvEmbedState>) -> Result<(), String> {
    if let Some(tx) = state.writer.lock().unwrap().clone() {
        let _ = tx.send(serde_json::json!({ "command": ["quit"] }).to_string());
    }
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    if let Some(mut child) = state.child.lock().unwrap().take() {
        let exited = child.try_wait().map(|s| s.is_some()).unwrap_or(false);
        if !exited {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    state.pending.lock().unwrap().clear();
    #[cfg(windows)]
    {
        let _ = show_host_window(&state, false);
    }
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

    let hwnd_raw = ensure_host_window(app, state)?;
    let _ = show_host_window(state, true);

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

    // mpv 手册 --wid: win32 上解释为 HWND, 须按 uint32 传入;
    // --idle=yes 无文件不退出 (换源窗口常驻), --keep-open=always 播完停在
    // 末帧 (eof-reached 置 true 由前端连播), 输入全关 (交互全走 DOM/IPC)。
    let child = std::process::Command::new(&mpv)
        .args([
            format!("--wid={}", hwnd_raw as u32),
            format!("--input-ipc-server={PIPE_NAME}"),
            "--idle=yes".to_string(),
            "--keep-open=always".to_string(),
            "--osc=no".to_string(),
            "--input-default-bindings=no".to_string(),
            "--input-vo-keyboard=no".to_string(),
            "--window-dragging=no".to_string(),
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

/// 取主窗口 HWND 并创建 (一次) STATIC 黑底子窗口作为 mpv 宿主。
#[cfg(windows)]
fn ensure_host_window(
    app: &tauri::AppHandle,
    state: &MpvEmbedState,
) -> Result<isize, String> {
    if let Some(h) = *state.hwnd.lock().unwrap() {
        return Ok(h);
    }
    let win = app
        .get_webview_window("main")
        .ok_or("找不到主窗口")?;
    let parent_hwnd = win.hwnd().map_err(|e| format!("获取主窗口句柄失败: {e}"))?;
    // tauri/wry 的 HWND 与本地 windows crate 版本可能不同, 经原始指针转换
    let parent_ptr = parent_hwnd.0 as *mut std::ffi::c_void;
    let child = unsafe { create_static_child(parent_ptr) }?;
    *state.hwnd.lock().unwrap() = Some(child);
    Ok(child)
}

/// 系统预注册的 STATIC 类 + SS_BLACKRECT: 免自注册窗口类/消息循环,
/// 黑底避免 mpv 启动前的白闪。
#[cfg(windows)]
unsafe fn create_static_child(parent: *mut std::ffi::c_void) -> Result<isize, String> {
    use windows::core::w;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, SetWindowPos, SWP_NOACTIVATE, SWP_SHOWWINDOW, WINDOW_EX_STYLE,
        WINDOW_STYLE, WS_CHILD, WS_VISIBLE,
    };

    // SS_BLACKRECT (= 0x4, 系统类 STATIC 的黑底填充样式) 位于
    // Win32::System::SystemServices, 为省一个 feature 直接写数值
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("STATIC"),
        w!("QuantumTVMpvHost"),
        WS_CHILD | WS_VISIBLE | WINDOW_STYLE(4),
        0,
        0,
        16,
        9,
        HWND(parent),
        None,
        None,
        None,
    )
    .map_err(|e| format!("创建 mpv 宿主窗口失败: {e}"))?;
    // 置于同层兄弟 (WebView2) 之上
    let _ = SetWindowPos(hwnd, HWND::default(), 0, 0, 16, 9, SWP_NOACTIVATE | SWP_SHOWWINDOW);
    Ok(hwnd.0 as isize)
}

#[cfg(windows)]
fn show_host_window(state: &MpvEmbedState, show: bool) -> Result<(), String> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
    };
    let Some(raw) = *state.hwnd.lock().unwrap() else {
        return Ok(());
    };
    let flags = if show {
        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW
    } else {
        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_HIDEWINDOW
    };
    unsafe {
        SetWindowPos(
            HWND(raw as *mut std::ffi::c_void),
            HWND::default(),
            0,
            0,
            0,
            0,
            flags,
        )
        .map_err(|e| format!("设置 mpv 宿主窗口可见性失败: {e}"))?;
    }
    Ok(())
}

#[cfg(windows)]
fn sync_window(
    state: &tauri::State<'_, MpvEmbedState>,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    scale: f64,
) -> Result<(), String> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, SWP_NOACTIVATE, SWP_SHOWWINDOW,
    };
    let Some(raw) = *state.hwnd.lock().unwrap() else {
        return Err("mpv 宿主窗口未创建".into());
    };
    unsafe {
        SetWindowPos(
            HWND(raw as *mut std::ffi::c_void),
            HWND::default(),
            (x * scale).round() as i32,
            (y * scale).round() as i32,
            (w * scale).round() as i32,
            (h * scale).round() as i32,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        )
        .map_err(|e| format!("同步 mpv 宿主窗口位置失败: {e}"))?;
    }
    Ok(())
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
                                // playback-time 逐帧变化, 节流到 200ms
                                if last_time_emit.elapsed()
                                    >= std::time::Duration::from_millis(200)
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
