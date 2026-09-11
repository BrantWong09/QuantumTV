//! 外部 mpv 播放: WebView2 缺 HEVC 解码扩展时, 网盘源(HEVC-MKV)黑屏有声。
//! mpv 内置全部解码器, 用独立窗口播放网盘直链(经本地 netdisk_proxy, 已带 UA)。
//! mpv.exe 按约定放在应用数据目录 mpv/mpv.exe, 缺失时返回可操作错误。

use serde::Serialize;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::Manager;

/// 运行中的 mpv 子进程 (同一时间只保留一个网盘播放窗口)
static MPV_CHILD: Mutex<Option<std::process::Child>> = Mutex::new(None);

#[derive(Debug, Serialize)]
pub struct MpvLaunchResult {
    pub launched: bool,
    pub mpv_path: String,
    pub reused: bool,
}

/// mpv.exe 查找顺序 (V2 Phase 4 起委托 quantumtv_core::playback::mpv_backend::locate_mpv,
/// 与 PlaybackManager 同一实现): env QUANTUMTV_MPV_PATH → <app_data>/mpv →
/// <exe_dir>/mpv → PATH
pub fn locate_mpv(app: &tauri::AppHandle) -> Option<PathBuf> {
    quantumtv_core::playback::mpv_backend::locate_mpv(app.path().app_data_dir().ok().as_deref())
}

/// 用 mpv 独立窗口播放 url。
/// url 已是本地 netdisk_proxy 包装地址(带 UA 参数), mpv 直接 GET 即可;
/// title 用于窗口标题(片名+集数)。
#[tauri::command]
pub async fn launch_mpv(
    app: tauri::AppHandle,
    url: String,
    title: Option<String>,
    start_at: Option<f64>,
) -> Result<MpvLaunchResult, String> {
    let Some(mpv) = locate_mpv(&app) else {
        return Err(format!(
            "未找到 mpv 播放器。请把 mpv.exe 放到: {}",
            app.path()
                .app_data_dir()
                .map(|d| d.join("mpv").join("mpv.exe").display().to_string())
                .unwrap_or_else(|_| "<应用数据目录>\\mpv\\mpv.exe".into())
        ));
    };

    // 同一子进程实例: mpv 收到新 URL 会复用窗口换源, 不再起新进程
    {
        let mut guard = MPV_CHILD.lock().unwrap();
        if let Some(child) = guard.as_mut() {
            if child.try_wait().map(|s| s.is_none()).unwrap_or(false) {
                drop(guard);
                spawn_to_mpv(&mpv, &url, &title, start_at, true)?;
                return Ok(MpvLaunchResult {
                    launched: true,
                    mpv_path: mpv.display().to_string(),
                    reused: true,
                });
            }
            *guard = None;
        }
    }

    let child = spawn_to_mpv(&mpv, &url, &title, start_at, false)?;
    *MPV_CHILD.lock().unwrap() = Some(child);
    Ok(MpvLaunchResult {
        launched: true,
        mpv_path: mpv.display().to_string(),
        reused: false,
    })
}

fn spawn_to_mpv(
    mpv: &std::path::Path,
    url: &str,
    title: &Option<String>,
    start_at: Option<f64>,
    reuse: bool,
) -> Result<std::process::Child, String> {
    let mut cmd = std::process::Command::new(mpv);
    // --input-ipc-server 不需要: 复用走 mpv 自身的单实例 (解锁已有窗口换源)
    if reuse {
        cmd.arg("--force-window=immediate");
    }
    if let Some(t) = title {
        cmd.arg(format!("--force-media-title={}", t));
        cmd.arg(format!("--title={}", t));
    }
    if let Some(s) = start_at {
        if s > 1.0 {
            cmd.arg(format!("--start={}", s));
        }
    }
    // 网盘大文件: 合理缓存(后端 64MB), 退出时不保存位置到网盘语义
    cmd.args([
        "--cache=yes",
        "--demuxer-max-bytes=64MiB",
        "--demuxer-readahead-secs=20",
        "--hwdec=auto-safe",
        "--no-save-position-on-quit",
        url,
    ]);
    cmd.spawn()
        .map_err(|e| format!("启动 mpv 失败 ({}): {}", mpv.display(), e))
}

#[tauri::command]
pub async fn mpv_available(app: tauri::AppHandle) -> Result<bool, String> {
    Ok(locate_mpv(&app).is_some())
}
