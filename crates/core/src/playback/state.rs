//! 播放状态 (03-playback.md §5): 单一真相, Rust 侧持有
//!
//! 状态机: Idle → Loading → (Playing ⇄ Paused) → Stopped/Ended/Error → Idle

use serde::{Deserialize, Serialize};

/// 播放器状态 (与 03-playback.md PlaybackState 一一对应)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackStatus {
    /// 未在播放
    Idle,
    /// 正在加载媒体
    Loading,
    Playing,
    Paused,
    /// 用户主动停止
    Stopped,
    /// 媒体自然播完
    Ended,
    Error,
}

/// 随状态推送给 UI 的完整快照
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybackState {
    pub status: PlaybackStatus,
    pub time: f64,
    pub duration: f64,
    /// 当前资源标识 (source+episode 定位), 不含敏感直链
    pub resource_id: Option<String>,
    /// 人可读错误信息 (status=Error 时)
    pub error: Option<String>,
}

impl PlaybackState {
    pub fn idle() -> Self {
        Self {
            status: PlaybackStatus::Idle,
            time: 0.0,
            duration: 0.0,
            resource_id: None,
            error: None,
        }
    }

    pub fn with_status(mut self, status: PlaybackStatus) -> Self {
        self.status = status;
        self
    }

    /// mpv 属性变化 → 状态机迁移 (纯函数, 单测覆盖)。
    ///
    /// 迁移规则:
    /// - file-loaded → Playing (从 Loading)
    /// - pause=true → Paused; pause=false → Playing (仅在活跃态)
    /// - eof-reached=true → Ended (从 Playing/Paused)
    /// - 管道死亡 → Idle (mpv 退出, 进程被外部关闭视为停止)
    /// - CommandFailed(loadfile) → Error (仅活跃态; 控制命令与
    ///   Idle 下的迟到超时不劫持状态)
    pub fn transition(&mut self, event: MpvEvent) {
        match event {
            MpvEvent::LoadStarted => {
                self.status = PlaybackStatus::Loading;
                self.error = None;
            }
            MpvEvent::FileLoaded => {
                if self.status == PlaybackStatus::Loading {
                    self.status = PlaybackStatus::Playing;
                }
            }
            MpvEvent::Pause(paused) => match self.status {
                PlaybackStatus::Playing if paused => self.status = PlaybackStatus::Paused,
                PlaybackStatus::Paused if !paused => self.status = PlaybackStatus::Playing,
                _ => {}
            },
            MpvEvent::EofReached => {
                if matches!(
                    self.status,
                    PlaybackStatus::Playing | PlaybackStatus::Paused
                ) {
                    self.status = PlaybackStatus::Ended;
                }
            }
            MpvEvent::ProcessDead => {
                // 主动 close (Stopped) 与意外退出都到 Idle; 细分留给错误检测层
                *self = Self::idle();
            }
            MpvEvent::PlaybackError(msg) => {
                self.status = PlaybackStatus::Error;
                self.error = Some(msg);
            }
            MpvEvent::CommandFailed { command, error } => {
                // loadfile 失败/超时: 活跃态 → Error (用户可见, 可重试)。
                // 控制命令 (seek/pause/...) 失败不劫持播放状态;
                // Idle 下的迟到超时 (stop 后 quit 无响应) 同样忽略。
                if command == "loadfile"
                    && matches!(
                        self.status,
                        PlaybackStatus::Loading | PlaybackStatus::Playing | PlaybackStatus::Paused
                    )
                {
                    self.status = PlaybackStatus::Error;
                    self.error = Some(format!("mpv 命令失败: {error}"));
                }
            }
            MpvEvent::StoppedByUser => {
                *self = Self::idle();
            }
            MpvEvent::Time(_)
            | MpvEvent::Duration(_) => {}
        }
    }
}

/// mpv 侧事件 (backend → 状态机 的输入)
#[derive(Debug, Clone, PartialEq)]
pub enum MpvEvent {
    /// loadfile 已下发
    LoadStarted,
    /// file-loaded
    FileLoaded,
    /// pause 属性变化
    Pause(bool),
    /// eof-reached=true
    EofReached,
    /// playback-time / duration 属性变化
    Time(f64),
    Duration(f64),
    /// mpv 上报播放错误 (end-file with error)
    PlaybackError(String),
    /// IPC 命令超时或 mpv 回错误响应 (Phase 7 命令级超时)。
    /// 仅 loadfile 在活跃态下推进状态机, 其余只用于日志。
    CommandFailed {
        command: String,
        error: String,
    },
    /// 管道关闭 = mpv 进程退出
    ProcessDead,
    /// 用户/UI 主动 stop
    StoppedByUser,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playing() -> PlaybackState {
        PlaybackState::idle().with_status(PlaybackStatus::Playing)
    }

    #[test]
    fn idle_to_loading_to_playing() {
        let mut s = PlaybackState::idle();
        s.transition(MpvEvent::LoadStarted);
        assert_eq!(s.status, PlaybackStatus::Loading);
        s.transition(MpvEvent::FileLoaded);
        assert_eq!(s.status, PlaybackStatus::Playing);
    }

    #[test]
    fn play_pause_cycle() {
        let mut s = playing();
        s.transition(MpvEvent::Pause(true));
        assert_eq!(s.status, PlaybackStatus::Paused);
        s.transition(MpvEvent::Pause(false));
        assert_eq!(s.status, PlaybackStatus::Playing);
    }

    #[test]
    fn pause_ignored_outside_active_states() {
        // Loading 时的 pause 事件不改变状态 (mpv 启动期 pause 属性会抖动)
        let mut s = PlaybackState::idle().with_status(PlaybackStatus::Loading);
        s.transition(MpvEvent::Pause(true));
        assert_eq!(s.status, PlaybackStatus::Loading);
    }

    #[test]
    fn eof_only_from_active() {
        let mut s = playing();
        s.transition(MpvEvent::EofReached);
        assert_eq!(s.status, PlaybackStatus::Ended);
        // Idle 时 eof 不再生效
        let mut idle = PlaybackState::idle();
        idle.transition(MpvEvent::EofReached);
        assert_eq!(idle.status, PlaybackStatus::Idle);
    }

    #[test]
    fn process_dead_resets_to_idle() {
        let mut s = playing();
        s.time = 42.0;
        s.duration = 100.0;
        s.transition(MpvEvent::ProcessDead);
        assert_eq!(s.status, PlaybackStatus::Idle);
        assert_eq!(s.time, 0.0);
        assert_eq!(s.resource_id, None);
    }

    #[test]
    fn error_state_carries_message() {
        let mut s = playing();
        s.transition(MpvEvent::PlaybackError("dlink 过期".into()));
        assert_eq!(s.status, PlaybackStatus::Error);
        assert_eq!(s.error.as_deref(), Some("dlink 过期"));
    }

    #[test]
    fn time_events_do_not_change_status() {
        let mut s = playing();
        s.transition(MpvEvent::Time(12.5));
        s.transition(MpvEvent::Duration(90.0));
        assert_eq!(s.status, PlaybackStatus::Playing);
    }

    #[test]
    fn command_failed_loadfile_from_active_goes_error() {
        // loadfile 超时/mpv 报错: 活跃态 → Error (用户可见, 可重试)
        let mut s = PlaybackState::idle().with_status(PlaybackStatus::Loading);
        s.transition(MpvEvent::CommandFailed {
            command: "loadfile".into(),
            error: "命令超时".into(),
        });
        assert_eq!(s.status, PlaybackStatus::Error);
        assert_eq!(s.error.as_deref(), Some("mpv 命令失败: 命令超时"));
    }

    #[test]
    fn command_failed_non_loadfile_ignored() {
        // seek/pause 等控制命令失败不劫持播放状态 (慢流时避免误伤)
        let mut s = playing();
        s.transition(MpvEvent::CommandFailed {
            command: "seek".into(),
            error: "命令超时".into(),
        });
        assert_eq!(s.status, PlaybackStatus::Playing);
        assert_eq!(s.error, None);
    }

    #[test]
    fn command_failed_ignored_from_idle() {
        // 用户 stop 后迟到的 loadfile 超时不把 Idle 翻成 Error
        let mut s = PlaybackState::idle();
        s.transition(MpvEvent::CommandFailed {
            command: "loadfile".into(),
            error: "命令超时".into(),
        });
        assert_eq!(s.status, PlaybackStatus::Idle);
    }

    #[test]
    fn status_snake_case_serialization() {
        // TS 侧 status: 'playing' | ... 与 Rust 一致
        assert_eq!(
            serde_json::to_string(&PlaybackStatus::Playing).unwrap(),
            "\"playing\""
        );
        assert_eq!(
            serde_json::to_string(&PlaybackStatus::Ended).unwrap(),
            "\"ended\""
        );
    }
}
