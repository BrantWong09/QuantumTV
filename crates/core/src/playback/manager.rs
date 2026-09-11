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
    /// 连续 crash 重拉次数 (用户主动发起新播放时清零)
    crash_restarts: Mutex<u32>,
    /// crash recovery 重拉上限 (07-migration Phase 6: 自动重启)
    pub max_crash_restarts: u32,
    /// 进程死亡前的最后快照: ProcessDead 事件会把 state 重置为 Idle,
    /// crash 判定 (是否活跃/断点位置) 只能看这份快照
    pre_dead: Arc<Mutex<Option<PlaybackState>>>,
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
            pre_dead: Arc::new(Mutex::new(None)),
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

    /// 用户主动发起新播放时清零 crash 重拉计数 (recovery 重拉不得清零,
    /// 否则持续崩溃的坏文件会绕过 max_crash_restarts 无限循环)
    pub fn reset_crash_restarts(&self) {
        *self.crash_restarts.lock().unwrap() = 0;
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
        on_event(&self.state(), &MpvEvent::LoadStarted);

        let url = resource.url.clone();
        let state_ref = self.state.clone();
        let pre_dead_ref = self.pre_dead.clone();
        let cb = Arc::new(move |ev: MpvEvent| {
            let mut state = state_ref.lock().unwrap();
            // Time/Duration 同步到快照
            match &ev {
                MpvEvent::Time(t) => state.time = *t,
                MpvEvent::Duration(d) => state.duration = *d,
                _ => {}
            }
            // 进程死亡会清空 state, 先留快照给 crash 判定
            if matches!(ev, MpvEvent::ProcessDead) {
                *pre_dead_ref.lock().unwrap() = Some(state.clone());
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

    /// crash 判定 (纯同步, 不拉起进程): 满足恢复条件时消耗一次重拉额度
    /// 并返回断点秒数; 不满足返回 None。与实际重拉分离以便单测。
    fn consume_crash_restart(&self) -> Option<Option<f64>> {
        if *self.closed_by_user.lock().unwrap() {
            return None;
        }
        // crash 判定看死亡前快照: ProcessDead 已把 state 清回 Idle,
        // 活跃态判定与断点位置都从 pre_dead 取 (Phase 6 修复: 原实现读
        // 已被重置的 state, 活跃态条件永假, recovery 实际不触发)
        let snapshot = self.pre_dead.lock().unwrap().clone()?;
        let was_active = matches!(
            snapshot.status,
            PlaybackStatus::Playing
                | PlaybackStatus::Paused
                | PlaybackStatus::Loading
                | PlaybackStatus::Error
        ) && snapshot.resource_id.is_some();
        if !was_active {
            return None;
        }
        let mut restarts = self.crash_restarts.lock().unwrap();
        if *restarts >= self.max_crash_restarts {
            return None;
        }
        *restarts += 1;
        let resume_at = (snapshot.time > 1.0
            && snapshot.duration > 0.0
            && snapshot.time < snapshot.duration - 1.0)
            .then_some(snapshot.time);
        log::warn!(
            "mpv 意外退出 (死亡前状态 {:?}, 进度 {:.1}s), crash recovery 第 {} 次重拉",
            snapshot.status,
            snapshot.time,
            *restarts
        );
        Some(resume_at)
    }

    /// mpv crash recovery (07-migration Phase 6): 进程意外退出且非用户主动
    /// 关闭时, 在 max_crash_restarts 内自动重拉并从断点续播。
    /// 返回 Some(重拉完成后的 LoadStarted) 表示已触发恢复。
    pub async fn recover_after_crash(
        &self,
        resource: &MediaResource,
        on_event: impl Fn(&PlaybackState, &MpvEvent) + Send + Sync + 'static,
    ) -> Option<Result<MpvLaunchResult, String>> {
        let resume_at = self.consume_crash_restart()?;
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

    /// 播放速度 (mpv speed 属性)
    pub fn set_speed(&self, speed: f64) -> Result<(), String> {
        self.backend.send_command(&[
            serde_json::json!("set_property"),
            serde_json::json!("speed"),
            serde_json::json!(speed),
        ])
    }

    /// 音量绝对值 (mpv volume 属性, 0..=volume-max)
    pub fn set_volume(&self, volume: f64) -> Result<(), String> {
        self.backend.send_command(&[
            serde_json::json!("set_property"),
            serde_json::json!("volume"),
            serde_json::json!(volume),
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
    fn crash_recovery_requires_pre_dead_snapshot() {
        // 无 ProcessDead 快照 (如从未播放) → 不重拉
        let manager = PlaybackManager::new();
        assert!(manager.consume_crash_restart().is_none());
    }

    #[test]
    fn crash_recovery_requires_active_pre_dead_state() {
        // 死亡前是 Idle (空载退出) → 不是 crash, 不重拉
        let manager = PlaybackManager::new();
        *manager.pre_dead.lock().unwrap() = Some(PlaybackState::idle());
        assert!(manager.consume_crash_restart().is_none());
    }

    #[test]
    fn crash_recovery_triggers_from_pre_dead_snapshot() {
        // 死亡前在 Playing → 触发重拉, 断点取自快照 time
        let manager = PlaybackManager::new();
        let mut snap = PlaybackState::idle().with_status(PlaybackStatus::Playing);
        snap.resource_id = Some("s1:1".into());
        snap.time = 30.0;
        snap.duration = 100.0;
        *manager.pre_dead.lock().unwrap() = Some(snap);
        let resume = manager.consume_crash_restart().unwrap();
        assert_eq!(resume, Some(30.0));
        assert_eq!(*manager.crash_restarts.lock().unwrap(), 1);
        // 上限 1: 再次崩溃 (未经用户发起新播放清零) → 不再重拉,
        // 防止持续崩溃的坏文件绕过上限无限循环
        assert!(manager.consume_crash_restart().is_none());
    }

    #[test]
    fn crash_recovery_skips_resume_near_end() {
        // 快照时间接近片尾 (>duration-1s) → 重拉但不续播
        let manager = PlaybackManager::new();
        let mut snap = PlaybackState::idle().with_status(PlaybackStatus::Playing);
        snap.resource_id = Some("s1:1".into());
        snap.time = 99.5;
        snap.duration = 100.0;
        *manager.pre_dead.lock().unwrap() = Some(snap);
        assert_eq!(manager.consume_crash_restart().unwrap(), None);
    }

    #[test]
    fn crash_recovery_limit() {
        let mut manager = PlaybackManager::new();
        let mut snap = PlaybackState::idle().with_status(PlaybackStatus::Playing);
        snap.resource_id = Some("s1:1".into());
        snap.time = 30.0;
        snap.duration = 100.0;
        *manager.pre_dead.lock().unwrap() = Some(snap);
        manager.max_crash_restarts = 0;
        // 上限 0 → 不重拉
        assert!(manager.consume_crash_restart().is_none());
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
