//! PlaybackManager: 播放器控制唯一入口 + 状态单一真相
//!
//! 职责 (03-playback.md §2): 启动/销毁 mpv、loadfile、play/pause/stop/seek/
//! volume/状态同步/错误处理。UI 不再直接操作 mpv (mpv_embed_command 在
//! Phase 5 UI 清理时删除)。
//!
//! 状态推送: 事件回调返回 (PlaybackState 快照, MpvEvent 原始事件)。
//! Tauri 层把快照 emit 成 playback_state / playback_time / playback_duration /
//! playback_error; 兼容期同时翻译成旧 mpv-embed-event (mpv_embed.rs 并存)。
//!
//! mpv 交互细节在 MpvBackend (mpv 子进程 + JSON IPC 命名管道, 方案 C 独立窗口)。

use super::mpv_backend::{MpvBackend, MpvLaunchResult};
use super::state::{MpvEvent, PlaybackState, PlaybackStatus};
use crate::media::MediaResource;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub struct PlaybackManager {
    backend: Arc<MpvBackend>,
    state: Arc<Mutex<PlaybackState>>,
    /// 进程被用户手动关闭 (区别于 crash) → 不自动重拉, 由上层抑制兜底
    closed_by_user: Arc<Mutex<bool>>,
    /// 连续 crash 重拉次数 (launch 成功后清零)
    crash_restarts: Mutex<u32>,
    /// crash recovery 重拉上限 (07-migration Phase 6: 自动重启)
    pub max_crash_restarts: u32,
    /// 进度保存节流: 上次保存时刻 (Rust 侧驱动, 前端不再节流编排)
    last_save: Mutex<std::time::Instant>,
    /// 进度保存间隔 (player_tick 的 5s 决策沿用)
    pub save_interval_secs: f64,
    /// app_data 目录 (mpv.exe 查找), Tauri 层注入; None 则只查 env/exe/PATH
    app_data_dir: Mutex<Option<PathBuf>>,
}

impl Default for PlaybackManager {
    fn default() -> Self {
        Self::new()
    }
}

impl PlaybackManager {
    pub fn new() -> Self {
        Self {
            backend: Arc::new(MpvBackend::default()),
            state: Arc::new(Mutex::new(PlaybackState::idle())),
            closed_by_user: Arc::new(Mutex::new(false)),
            crash_restarts: Mutex::new(0),
            max_crash_restarts: 1,
            last_save: Mutex::new(std::time::Instant::now() - std::time::Duration::from_secs(60)),
            save_interval_secs: 5.0,
            app_data_dir: Mutex::new(None),
        }
    }

    pub fn backend(&self) -> Arc<MpvBackend> {
        self.backend.clone()
    }

    /// 注入 app_data 目录 (mpv.exe 查找), Tauri setup 时调用一次
    pub fn set_app_data_dir(&self, dir: PathBuf) {
        *self.app_data_dir.lock().unwrap() = Some(dir);
    }

    /// 当前状态快照
    pub fn state(&self) -> PlaybackState {
        self.state.lock().unwrap().clone()
    }

    /// 播放 MediaResource: proxy_required 的资源 URL 应已是 Gateway 地址
    /// (Phase 3 wrap_resource), loadfile 直接用。start_at 为续播秒数。
    pub async fn play_resource(
        &self,
        resource: &MediaResource,
        start_at: Option<f64>,
        on_event: impl Fn(&PlaybackState, &MpvEvent) + Send + Sync + 'static,
    ) -> Result<MpvLaunchResult, String> {
        {
            let mut state = self.state.lock().unwrap();
            *state = PlaybackState {
                status: PlaybackStatus::Loading,
                time: 0.0,
                duration: 0.0,
                resource_id: Some(resource.id.clone()),
                error: None,
            };
        }
        *self.closed_by_user.lock().unwrap() = false;
        *self.crash_restarts.lock().unwrap() = 0;
        on_event(&self.state(), &MpvEvent::LoadStarted);

        let url = resource.url.clone();
        let state_ref = self.state.clone();
        let cb = Arc::new(move |ev: MpvEvent| {
            let mut state = state_ref.lock().unwrap();
            // Time/Duration 同步到快照
            match &ev {
                MpvEvent::Time(t) => state.time = *t,
                MpvEvent::Duration(d) => state.duration = *d,
                _ => {}
            }
            state.transition(ev.clone());
            on_event(&state, &ev);
        });

        let data_dir = self.app_data_dir.lock().unwrap().clone();
        let result = self
            .backend
            .launch(data_dir.as_deref(), move |ev| cb(ev))
            .await?;
        // 续播: loadfile replace start=<秒> (进程复用时也适用)
        let start = start_at.filter(|s| *s > 1.0);
        let mut cmd = vec![
            serde_json::json!("loadfile"),
            serde_json::json!(url),
            serde_json::json!("replace"),
        ];
        if let Some(s) = start {
            cmd.push(serde_json::json!(format!("start={s}")));
        }
        self.backend.send_command(&cmd)?;
        if let Some(title) = &resource.metadata.title {
            let ep = resource
                .metadata
                .episode
                .clone()
                .map(|e| format!("{title} {e}"))
                .unwrap_or_else(|| title.clone());
            let _ = self.backend.send_command(&[
                serde_json::json!("set_property"),
                serde_json::json!("force-media-title"),
                serde_json::json!(ep),
            ]);
        }
        Ok(result)
    }

    /// mpv crash recovery (07-migration Phase 6): 进程意外退出且非用户主动
    /// 关闭时, 在 max_crash_restarts 内自动重拉并从断点续播。
    /// 返回 Some(重拉完成后的 LoadStarted) 表示已触发恢复。
    pub async fn recover_after_crash(
        &self,
        resource: &MediaResource,
        on_event: impl Fn(&PlaybackState, &MpvEvent) + Send + Sync + 'static,
    ) -> Option<Result<MpvLaunchResult, String>> {
        if *self.closed_by_user.lock().unwrap() {
            return None;
        }
        // crash 判定: 之前在活跃态播放且本次 play_resource 未先 stop
        let was_active = {
            let state = self.state.lock().unwrap();
            matches!(
                state.status,
                PlaybackStatus::Playing
                    | PlaybackStatus::Paused
                    | PlaybackStatus::Loading
                    | PlaybackStatus::Error
            ) && state.resource_id.is_some()
        };
        if !was_active {
            return None;
        }
        let mut restarts = self.crash_restarts.lock().unwrap();
        if *restarts >= self.max_crash_restarts {
            return None;
        }
        *restarts += 1;
        let resume_at = {
            let state = self.state.lock().unwrap();
            (state.time > 1.0 && state.duration > 0.0 && state.time < state.duration - 1.0)
                .then_some(state.time)
        };
        log::warn!("mpv 意外退出, 尝试 crash recovery 第 {} 次重拉", *restarts);
        Some(self.play_resource(resource, resume_at, on_event).await)
    }

    /// 用户主动关闭 mpv 窗口 (前端 dead 事件路径调用): 抑制 crash recovery
    pub fn mark_closed_by_user(&self) {
        *self.closed_by_user.lock().unwrap() = true;
    }

    pub fn pause(&self) -> Result<(), String> {
        self.backend.send_command(&[serde_json::json!("cycle"), serde_json::json!("pause")])
    }

    pub fn set_paused(&self, paused: bool) -> Result<(), String> {
        self.backend.send_command(&[
            serde_json::json!("set_property"),
            serde_json::json!("pause"),
            serde_json::json!(paused),
        ])
    }

    /// seek: 相对秒 (负值回退) 或绝对定位
    pub fn seek_relative(&self, secs: f64) -> Result<(), String> {
        self.backend.send_command(&[
            serde_json::json!("seek"),
            serde_json::json!(secs),
        ])
    }

    pub fn seek_absolute(&self, secs: f64) -> Result<(), String> {
        self.backend.send_command(&[
            serde_json::json!("seek"),
            serde_json::json!(secs),
            serde_json::json!("absolute"),
        ])
    }

    pub fn add_volume(&self, delta: i64) -> Result<(), String> {
        self.backend.send_command(&[
            serde_json::json!("add"),
            serde_json::json!("volume"),
            serde_json::json!(delta),
        ])
    }

    pub fn stop(&self) -> Result<(), String> {
        *self.closed_by_user.lock().unwrap() = true;
        let mut state = self.state.lock().unwrap();
        state.transition(MpvEvent::StoppedByUser);
        drop(state);
        self.backend.shutdown();
        Ok(())
    }

    /// Rust 侧进度保存节流决策 (吸收 player_tick 的 should_save 逻辑):
    /// 每 save_interval_secs 允许保存一次; 跳过检测仍由前端按 skip 配置触发
    /// (skip 配置属于 Playback 域边缘, Phase 5 UI 清理时收口)。
    pub fn should_save_progress(&self) -> bool {
        let mut last = self.last_save.lock().unwrap();
        if last.elapsed().as_secs_f64() >= self.save_interval_secs {
            *last = std::time::Instant::now();
            true
        } else {
            false
        }
    }

    /// app 退出兜底
    pub fn shutdown(&self) {
        self.backend.kill_now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_save_progress_throttles() {
        let mut manager = PlaybackManager::new();
        manager.save_interval_secs = 5.0;
        // 首次: last_save 初始化在 60s 前 → 允许
        assert!(manager.should_save_progress());
        // 间隔内重复: 拒绝
        assert!(!manager.should_save_progress());
        assert!(!manager.should_save_progress());
    }

    #[test]
    fn stop_resets_to_idle_and_marks_user_closed() {
        let manager = PlaybackManager::new();
        let _ = manager.stop();
        assert_eq!(manager.state().status, PlaybackStatus::Idle);
        // 用户主动 stop 后 crash recovery 不生效
        assert!(manager.recover_after_crash(&test_resource(), |_, _| {}).now_or_never().unwrap().is_none());
    }

    #[test]
    fn crash_recovery_requires_active_state() {
        // Idle 状态下管道死亡不是 crash (无内容在播), 不重拉
        let manager = PlaybackManager::new();
        let r = manager
            .recover_after_crash(&test_resource(), |_, _| {})
            .now_or_never()
            .unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn crash_recovery_limit() {
        let mut manager = PlaybackManager::new();
        {
            let mut state = manager.state.lock().unwrap();
            state.status = PlaybackStatus::Playing;
            state.resource_id = Some("s1:1".into());
            state.time = 30.0;
            state.duration = 100.0;
        }
        manager.max_crash_restarts = 0;
        // 上限 0 → 不重拉
        let r = manager
            .recover_after_crash(&test_resource(), |_, _| {})
            .now_or_never()
            .unwrap();
        assert!(r.is_none());
    }

    fn test_resource() -> MediaResource {
        MediaResource::new(
            "test:1",
            "https://example.com/v.mp4",
            crate::media::ResourceType::File,
        )
    }

    trait NowOrNever {
        type Output;
        fn now_or_never(self) -> Option<Self::Output>;
    }

    impl<F: std::future::Future> NowOrNever for F {
        type Output = F::Output;
        fn now_or_never(self) -> Option<Self::Output> {
            let mut fut = Box::pin(self);
            let waker = std::task::Waker::noop();
            let mut cx = std::task::Context::from_waker(waker);
            match fut.as_mut().poll(&mut cx) {
                std::task::Poll::Ready(v) => Some(v),
                std::task::Poll::Pending => None,
            }
        }
    }
}
