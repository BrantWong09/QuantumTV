//! V2 Phase 4 Tauri 接线层: PlaybackManager (quantumtv_core::playback) 的
//! State 托管 + playback_* 命令 + 事件翻译。
//!
//! 职责边界 (03-playback.md §2 + 06-project-structure.md 依赖方向):
//! - 本层只做参数转换与事件 emit, 不持有播放状态 (单一真相在 PlaybackManager)
//! - 新事件 playback_state / playback_time / playback_duration / playback_error
//! - 兼容期 (Phase 5 UI 切换前) 同步翻译旧 mpv-embed-event,
//!   使 play/page.tsx 现有监听不变; mpv_embed_* 命令委托到同一 manager

use serde::Serialize;
use std::sync::Arc;
use tauri::Emitter;
use tauri::Manager;

use quantumtv_core::playback::manager::PlaybackManager;
use quantumtv_core::playback::state::{MpvEvent, PlaybackState};
use quantumtv_core::playback::PlaybackStatus;

/// 播放状态序列化 (给前端 playback_* 事件)
#[derive(Debug, Serialize)]
pub struct PlaybackStateDto {
    pub status: PlaybackStatus,
    pub time: f64,
    pub duration: f64,
    pub resource_id: Option<String>,
    pub error: Option<String>,
}

impl From<&PlaybackState> for PlaybackStateDto {
    fn from(s: &PlaybackState) -> Self {
        Self {
            status: s.status,
            time: s.time,
            duration: s.duration,
            resource_id: s.resource_id.clone(),
            error: s.error.clone(),
        }
    }
}

/// 共享 PlaybackManager (mpv_embed 兼容命令与 playback_* 命令指向同一实例)
pub struct PlaybackManagerState {
    pub manager: Arc<PlaybackManager>,
}

impl PlaybackManagerState {
    pub fn new() -> Self {
        Self {
            manager: Arc::new(PlaybackManager::new()),
        }
    }
}

impl Default for PlaybackManagerState {
    fn default() -> Self {
        Self::new()
    }
}

/// Rust 侧单一事件出口: 状态快照 + 原始事件 → 新 playback_* 事件
/// + 兼容旧 mpv-embed-event (Phase 5 UI 切换后删除翻译段)。
pub fn emit_playback_event(app: &tauri::AppHandle, state: &PlaybackState, ev: &MpvEvent) {
    let dto = PlaybackStateDto::from(state);
    let _ = app.emit("playback_state", &dto);
    match ev {
        MpvEvent::Time(t) => {
            let _ = app.emit("playback_time", serde_json::json!({ "time": t }));
            let _ = app.emit(
                "mpv-embed-event",
                serde_json::json!({ "kind": "time", "time": t, "duration": state.duration }),
            );
        }
        MpvEvent::Duration(d) => {
            let _ = app.emit(
                "playback_duration",
                serde_json::json!({ "duration": d }),
            );
            let _ = app.emit(
                "mpv-embed-event",
                serde_json::json!({ "kind": "duration", "duration": d }),
            );
        }
        MpvEvent::Pause(paused) => {
            let _ = app.emit(
                "mpv-embed-event",
                serde_json::json!({ "kind": "pause", "value": paused }),
            );
        }
        MpvEvent::EofReached => {
            let _ = app.emit("mpv-embed-event", serde_json::json!({ "kind": "eof" }));
        }
        MpvEvent::FileLoaded => {
            let _ = app.emit("mpv-embed-event", serde_json::json!({ "kind": "file-loaded" }));
        }
        MpvEvent::PlaybackError(msg) => {
            let _ = app.emit("playback_error", serde_json::json!({ "error": msg }));
        }
        MpvEvent::ProcessDead => {
            let _ = app.emit("mpv-embed-event", serde_json::json!({ "kind": "dead" }));
        }
        MpvEvent::LoadStarted | MpvEvent::StoppedByUser => {}
    }
}

// ---------------------------------------------------------------------------
// Tauri 命令 (新 playback_* 接口)
// ---------------------------------------------------------------------------

/// 播放 MediaResource (JSON 形态): proxy_required 资源的 url 应先经
/// PlaybackGateway wrap_resource (Phase 3), 这里直接 loadfile。
#[tauri::command]
pub async fn playback_play(
    app: tauri::AppHandle,
    state: tauri::State<'_, PlaybackManagerState>,
    resource: quantumtv_core::media::MediaResource,
    start_at: Option<f64>,
) -> Result<serde_json::Value, String> {
    let manager = state.manager.clone();
    let app_cb = app.clone();
    let result = manager
        .play_resource(&resource, start_at, move |s, e| {
            emit_playback_event(&app_cb, s, e);
        })
        .await?;
    Ok(serde_json::json!({
        "launched": result.launched,
        "reused": result.reused,
    }))
}

#[tauri::command]
pub async fn playback_pause(state: tauri::State<'_, PlaybackManagerState>) -> Result<(), String> {
    state.manager.pause()
}

#[tauri::command]
pub async fn playback_set_paused(
    state: tauri::State<'_, PlaybackManagerState>,
    paused: bool,
) -> Result<(), String> {
    state.manager.set_paused(paused)
}

#[tauri::command]
pub async fn playback_seek(
    state: tauri::State<'_, PlaybackManagerState>,
    secs: f64,
    absolute: Option<bool>,
) -> Result<(), String> {
    if absolute.unwrap_or(false) {
        state.manager.seek_absolute(secs)
    } else {
        state.manager.seek_relative(secs)
    }
}

#[tauri::command]
pub async fn playback_add_volume(
    state: tauri::State<'_, PlaybackManagerState>,
    delta: i64,
) -> Result<(), String> {
    state.manager.add_volume(delta)
}

#[tauri::command]
pub async fn playback_stop(state: tauri::State<'_, PlaybackManagerState>) -> Result<(), String> {
    state.manager.stop()
}

/// 当前状态快照 (前端轮询/初始化用, 事件丢失时兜底)
#[tauri::command]
pub async fn playback_state(
    state: tauri::State<'_, PlaybackManagerState>,
) -> Result<PlaybackStateDto, String> {
    Ok(PlaybackStateDto::from(&state.manager.state()))
}

// ---------------------------------------------------------------------------
// 旧 mpv_embed_* 命令 → 委托 PlaybackManager (兼容期, Phase 5 删除)
//
// 注意: tauri::command 以命令名注册宏符号, 不能与 mpv_embed.rs 的同名命令
// 同时存在。旧 mpv_embed.rs 实现已不再注册 (lib.rs 引用本模块版本),
// 其文件保留到 Phase 5 一并删除。
// ---------------------------------------------------------------------------

/// 旧 mpv_embed_launch: 委托 manager 拉起/复用 mpv; 实际 loadfile 由前端
/// mpv_embed_command 下发 (旧行为保持)。
#[tauri::command]
pub async fn mpv_embed_launch(
    app: tauri::AppHandle,
    state: tauri::State<'_, PlaybackManagerState>,
) -> Result<serde_json::Value, String> {
    let manager = state.manager.clone();
    let app_cb = app.clone();
    // 旧行为: spawn 时空载 (--idle=yes), url 由前端 loadfile 下发。
    // 复用 launch 的进程管理, 保持旧接口语义 (loadfile 由调用方负责)。
    let result = manager
        .backend()
        .launch(
            crate::commands::playback::app_data_dir(&app).as_deref(),
            move |ev: MpvEvent| {
                let snapshot = manager.state();
                emit_playback_event(&app_cb, &snapshot, &ev);
            },
        )
        .await?;
    Ok(serde_json::json!({
        "launched": result.launched,
        "reused": result.reused,
        "mpv_path": result.mpv_path,
    }))
}

/// 旧 mpv_embed_command: 透传 mpv JSON IPC 命令 (前端 Phase 5 前继续使用)
#[tauri::command]
pub async fn mpv_embed_command(
    state: tauri::State<'_, PlaybackManagerState>,
    cmd: Vec<serde_json::Value>,
) -> Result<(), String> {
    state.manager.backend().send_command(&cmd)
}

/// 旧 mpv_embed_close: 委托 manager stop (状态复位 + crash recovery 抑制)
#[tauri::command]
pub async fn mpv_embed_close(state: tauri::State<'_, PlaybackManagerState>) -> Result<(), String> {
    state.manager.stop()
}

pub fn app_data_dir(app: &tauri::AppHandle) -> Option<std::path::PathBuf> {
    app.path().app_data_dir().ok()
}