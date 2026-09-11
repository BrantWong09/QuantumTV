//! PlaybackManager (V2 Phase 4, 见 docs/architecture/03-playback.md)
//!
//! Rust Core 中唯一负责播放器控制的组件: 启动/销毁 mpv、loadfile、
//! play/pause/stop/seek/volume、状态同步、错误处理。
//!
//! 播放状态只存在这里一份 (单一真相来源), UI 经 Tauri Event 订阅:
//! playback_state / playback_time / playback_duration / playback_error
//! (旧 mpv-embed-event 兼容期并存, Phase 5 UI 切换后删除)。
//!
//! mpv 交互细节在 MpvBackend (mpv 子进程 + JSON IPC 命名管道, 方案 C 独立窗口)。
//! Core 不依赖 tauri: 事件经回调上抛, mpv 查找目录由 Tauri 层注入。

pub mod manager;
pub mod mpv_backend;
pub mod state;

pub use manager::PlaybackManager;
pub use state::{PlaybackState, PlaybackStatus};
