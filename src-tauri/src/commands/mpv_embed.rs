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
/// 必须在主线程执行 SetWindowPos: 跨线程对别的线程所属窗口调用会撞上
/// 该线程阻塞的窗口过程 (mpv 渲染循环), 表现为整个应用未响应。
/// 首次同步会把创建时隐藏的子窗口定位到视频区并显示 —— rect 之外的页面
/// 区域从此可正常点击 (窗口只盖住视频区, 不挡整页)。
#[tauri::command]
pub async fn mpv_embed_sync(
    app: tauri::AppHandle,
    state: tauri::State<'_, MpvEmbedState>,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    scale: f64,
) -> Result<(), String> {
    #[cfg(windows)]
    {
        let _ = &state;
        let hwnd = app
            .state::<MpvEmbedState>()
            .hwnd
            .lock()
            .unwrap()
            .clone();
        let Some(raw) = hwnd else {
            return Err("mpv 宿主窗口未创建".into());
        };
        let (fx, fy, fw, fh) = (
            (x * scale).round() as i32,
            (y * scale).round() as i32,
            (w * scale).round() as i32,
            (h * scale).round() as i32,
        );
        app.run_on_main_thread(move || {
            let _ = sync_window_raw(raw, fx, fy, fw, fh);
        })
        .map_err(|e| format!("调度主线程失败: {e}"))?;
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        let _ = (&app, &state, &x, &y, &w, &h, &scale);
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
pub async fn mpv_embed_close(
    app: tauri::AppHandle,
    state: tauri::State<'_, MpvEmbedState>,
) -> Result<(), String> {
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
        let hwnd = state.hwnd.lock().unwrap().clone();
        if let Some(raw) = hwnd {
            let _ = app.run_on_main_thread(move || {
                let _ = show_host_window_raw(raw, false);
            });
        }
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

    // 子窗口的创建/显示/定位必须全部在主线程执行: CreateWindowExW 会同步
    // 发消息给父窗口 (主线程), 在 async 命令线程调用同样会互撞消息循环;
    // 且 CreateWindowExW 是阻塞调用, 不能放进 run_on_main_thread 后同步等
    // 结果 —— 先在当前线程创建好"占位计划", 由主线程闭包完成创建并回填句柄。
    // 用 channel 等待创建完成 (限 5s), 失败即报错。
    let app2 = app.clone();
    let (tx, rx) = tokio::sync::oneshot::channel::<Result<isize, String>>();
    if state.hwnd.lock().unwrap().is_none() {
        let parent_hwnd = {
            let win = app
                .get_webview_window("main")
                .ok_or("找不到主窗口")?;
            win.hwnd().map_err(|e| format!("获取主窗口句柄失败: {e}"))?
        };
        let parent_raw = parent_hwnd.0 as isize;
        let _ = app2.run_on_main_thread(move || {
            let r = unsafe { create_static_child(parent_raw as *mut std::ffi::c_void) };
            let _ = tx.send(r);
        });
        let created = tokio::time::timeout(std::time::Duration::from_secs(5), rx)
            .await
            .map_err(|_| "创建 mpv 宿主窗口超时".to_string())
            .and_then(|r| r.map_err(|e| format!("创建任务失败: {e}")))?;
        let hwnd = created?;
        *state.hwnd.lock().unwrap() = Some(hwnd);
    }
    // 复用路径不主动 show: 窗口保持上次的可见状态 (visible 时本来就在
    // 视频区 rect 上, 不可见时等前端下一次 sync 显示), 避免整客户区遮挡。
    let hwnd_raw = state
        .hwnd
        .lock()
        .unwrap()
        .ok_or("mpv 宿主窗口句柄缺失")?;

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

/// 系统预注册的 STATIC 类 + SS_BLACKRECT: 免自注册窗口类/消息循环,
/// 黑底避免 mpv 启动前的白闪。仅在主线程调用。
#[cfg(windows)]
unsafe fn create_static_child(parent: *mut std::ffi::c_void) -> Result<isize, String> {
    use windows::core::w;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, WINDOW_STYLE, WS_CHILD, WS_EX_NOACTIVATE,
    };

    // SS_BLACKRECT (= 0x4, 系统类 STATIC 的黑底填充样式) 位于
    // Win32::System::SystemServices, 为省一个 feature 直接写数值。
    // WS_EX_NOACTIVATE: 点击 mpv 画面不抢键盘焦点, 避免页面交互"卡死"体感。
    // 创建时隐藏 (不带 WS_VISIBLE, 且 SWP_HIDEWINDOW): 此刻前端还没同步过
    // 视频区 rect, 若直接 16x9 或整客户区可见, 会挡住 WebView2 的鼠标事件
    // —— 页面除视频区外全部点不了。首次 mpv_embed_sync 到位后才显示。
    let hwnd = CreateWindowExW(
        WS_EX_NOACTIVATE,
        w!("STATIC"),
        w!("QuantumTVMpvHost"),
        WS_CHILD | WINDOW_STYLE(4),
        0,
        0,
        0,
        0,
        HWND(parent),
        None,
        None,
        None,
    )
    .map_err(|e| format!("创建 mpv 宿主窗口失败: {e}"))?;
    Ok(hwnd.0 as isize)
}

#[cfg(windows)]
fn show_host_window_raw(raw: isize, show: bool) -> Result<(), String> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
        SWP_SHOWWINDOW,
    };
    let flags = if show {
        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER | SWP_SHOWWINDOW
    } else {
        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER | SWP_HIDEWINDOW
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
fn sync_window_raw(
    raw: isize,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) -> Result<(), String> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER, SWP_SHOWWINDOW,
    };
    unsafe {
        SetWindowPos(
            HWND(raw as *mut std::ffi::c_void),
            HWND::default(),
            x,
            y,
            w,
            h,
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
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
