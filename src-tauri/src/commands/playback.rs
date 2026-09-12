//! V2 Phase 4 Tauri 接线层: PlaybackManager (quantumtv_core::playback) 的
//! State 托管 + playback_* 命令 + 事件翻译。
//!
//! 职责边界 (03-playback.md §2 + 06-project-structure.md 依赖方向):
//! - 本层只做参数转换与事件 emit, 不持有播放状态 (单一真相在 PlaybackManager)
//! - 事件: playback_state / playback_time / playback_duration / playback_error
//!   (旧 mpv-embed-event 兼容翻译已在 Phase 6 删除)

use serde::Serialize;
use std::sync::Arc;
use tauri::Emitter;

use quantumtv_core::media::MediaResource;
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

/// 共享 PlaybackManager (playback_* 命令指向同一实例)
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

/// Rust 侧单一事件出口: 状态快照 + 原始事件 → playback_* 事件
pub fn emit_playback_event(app: &tauri::AppHandle, state: &PlaybackState, ev: &MpvEvent) {
    let dto = PlaybackStateDto::from(state);
    let _ = app.emit("playback_state", &dto);
    match ev {
        MpvEvent::Time(t) => {
            let _ = app.emit("playback_time", serde_json::json!({ "time": t }));
        }
        MpvEvent::Duration(d) => {
            let _ = app.emit(
                "playback_duration",
                serde_json::json!({ "duration": d }),
            );
        }
        MpvEvent::Pause(_) | MpvEvent::EofReached | MpvEvent::FileLoaded => {}
        MpvEvent::PlaybackError(msg) => {
            let _ = app.emit("playback_error", serde_json::json!({ "error": msg }));
        }
        MpvEvent::CommandFailed { .. } => {
            // loadfile 失败已经状态机转 Error; 状态快照里带错误信息时上抛
            if state.status == PlaybackStatus::Error {
                let _ = app.emit(
                    "playback_error",
                    serde_json::json!({ "error": state.error.clone().unwrap_or_default() }),
                );
            }
        }
        MpvEvent::ProcessDead => {}
        MpvEvent::LoadStarted | MpvEvent::StoppedByUser => {}
    }
}

// ---------------------------------------------------------------------------
// Resolve 缓存 + SingleFlight (方案 §12/§13)
// ---------------------------------------------------------------------------

/// key = resolve:{source}:{vod_id}:{flag}:{episode_id}; TTL 10 分钟(普通 URL 5~30 分钟档)。
/// moka try_get_with: 同 key 并发调用共享同一 Future — 只执行一次解析 (SingleFlight);
/// 失败不入缓存 (下次播放自动重试)。临时签名 URL 的 expires_at 细化留待观测后调整。
static RESOLVE_CACHE: std::sync::LazyLock<moka::future::Cache<String, Arc<MediaResource>>> =
    std::sync::LazyLock::new(|| {
        moka::future::Cache::builder()
            .max_capacity(200)
            .time_to_live(std::time::Duration::from_secs(600))
            .build()
    });

pub(crate) async fn cached_resolve_with<F, Fut>(
    key: String,
    fetch: F,
) -> Result<Arc<MediaResource>, String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<MediaResource, String>>,
{
    RESOLVE_CACHE
        .try_get_with(key, async { fetch().await.map(Arc::new) })
        .await
        .map_err(|e| (*e).clone())
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
    state.manager.reset_crash_restarts();
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
pub async fn playback_set_volume(
    state: tauri::State<'_, PlaybackManagerState>,
    volume: f64,
) -> Result<(), String> {
    state.manager.set_volume(volume)
}

#[tauri::command]
pub async fn playback_set_speed(
    state: tauri::State<'_, PlaybackManagerState>,
    speed: f64,
) -> Result<(), String> {
    state.manager.set_speed(speed)
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

/// V2 Phase 5: 播放编排收口到 Rust —— 前端只传"哪一集", 不再自己解析/包装。
///
/// 编排链 (07-migration.md 最终形态):
/// ```text
/// source+flag+episodeId
///   → ResolverManager (spider raw id 解析 / 直链直通)
///   → proxy_required ? PlaybackGateway.wrap_resource (opaque token)
///   → PlaybackManager.play_resource (loadfile + 续播)
/// ```
/// 非 spider 直链源也走这里 (DirectResolver 直通), 前端 isDirectPlayableUrl 判定删除。
#[tauri::command]
pub async fn playback_play_episode(
    app: tauri::AppHandle,
    state: tauri::State<'_, PlaybackManagerState>,
    source: String,
    flag: String,
    episode_id: String,
    title: Option<String>,
    episode: Option<String>,
    start_at: Option<f64>,
    storage: tauri::State<'_, crate::storage::StorageManager>,
    db: tauri::State<'_, crate::db::db_client::Db>,
) -> Result<serde_json::Value, String> {
    // 站点信息: spider 站点需要类名与 site_type 判定
    let config =
        crate::commands::config::get_config_with_db_sources(&storage, &db)?;
    let site = crate::commands::video::resolve_enabled_source(&config, &source)
        .ok_or_else(|| format!("Source not found or disabled: {}", source))?;
    let site_type = site.site_type.unwrap_or(1);

    // ResolverManager 组装 (spider raw id 解析 / 直链直通统一解析链)
    let resource = if site_type == 3 {
        let class_name = site.api.strip_prefix("csp_").unwrap_or(&site.api);
        let Some(bridge_url) = quantumtv_core::bridge::effective_url() else {
            return Err("桥接未就绪".into());
        };
        let manager = quantumtv_core::resolver::ResolverManager::with_defaults(Arc::new(
            quantumtv_core::spider::BridgeSpiderPlayFetcher {
                bridge_url: bridge_url.clone(),
            },
        ));
        manager
            .resolve(&quantum_core_resolve_input_spider(
                &source,
                &flag,
                &episode_id,
                class_name,
            ))
            .await
            .map_err(|e| e.to_string())?
    } else {
        // 直链源: episode_id 即 url, DirectResolver 直通 (带 header 归一化)
        let manager = quantumtv_core::resolver::ResolverManager::with_defaults(Arc::new(
            quantumtv_core::spider::BridgeSpiderPlayFetcher {
                bridge_url: String::new(),
            },
        ));
        manager
            .resolve(&quantumtv_core::resolver::ResolveInput::direct(
                source.clone(),
                episode_id.clone(),
            ))
            .await
            .map_err(|e| e.to_string())?
    };

    // 展示元数据 (窗口标题)
    let mut resource = resource;
    resource.metadata.title = title;
    resource.metadata.episode = episode;

    // proxy_required 资源经 PlaybackGateway 包装 (opaque token, 隐藏直链+带请求头)
    if resource.proxy_required {
        match quantumtv_core::gateway::wrap_resource(&resource).await {
            Ok(url) => resource.url = url,
            Err(e) => {
                log::warn!(
                    "[播放编排] Gateway 包装失败, 回退旧网盘代理路径: {}",
                    quantumtv_core::spider::trunc(&e, 120)
                );
                let port = quantumtv_core::netdisk_proxy::ensure_started().await?;
                resource.url = quantumtv_core::netdisk_proxy::wrap_proxy_url(
                    &resource.url,
                    resource.user_agent.as_deref(),
                    port,
                );
            }
        }
    }

    let manager = state.manager.clone();
    manager.reset_crash_restarts();
    let app_cb = app.clone();
    let result = manager
        .play_resource(&resource, start_at, move |s, e| {
            emit_playback_event(&app_cb, s, e);
        })
        .await?;
    log::info!(
        "[播放编排] playback_play_episode 完成: source={} episode_id={} reused={}",
        source,
        quantumtv_core::spider::trunc(&episode_id, 60),
        result.reused
    );
    Ok(serde_json::json!({
        "launched": result.launched,
        "reused": result.reused,
    }))
}

/// ResolveInput::spider 的简写包装 (避免长调用)
fn quantum_core_resolve_input_spider(
    source: &str,
    flag: &str,
    episode_id: &str,
    class_name: &str,
) -> quantumtv_core::resolver::ResolveInput {
    quantumtv_core::resolver::ResolveInput::spider(source, flag, episode_id, class_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use quantumtv_core::media::ResourceType;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn uniq_key(tag: &str) -> String {
        format!(
            "resolve:{}:{}:{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }

    #[tokio::test]
    async fn resolve_cache_hit_skips_second_resolve() {
        // 方案 §12: Resolve 缓存命中不得再触发解析
        let key = uniq_key("cache");
        let counter = Arc::new(AtomicU32::new(0));
        let r1 = cached_resolve_with(key.clone(), {
            let c = counter.clone();
            move || {
                let c = c.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, String>(quantumtv_core::media::MediaResource::new(
                        "ep-1",
                        "http://example.com/1.m3u8",
                        ResourceType::Hls,
                    ))
                }
            }
        })
        .await
        .unwrap();
        let r2 = cached_resolve_with(key, {
            let c = counter.clone();
            move || {
                let c = c.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, String>(quantumtv_core::media::MediaResource::new(
                        "ep-x",
                        "http://example.com/x.m3u8",
                        ResourceType::Hls,
                    ))
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(r1.url, r2.url);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn resolve_singleflight_concurrent_callers_share_one_execution() {
        // 方案 §13/§45: 同一 Episode 并发 5 次 → 实际 Resolve = 1 次
        let key = uniq_key("flight");
        let counter = Arc::new(AtomicU32::new(0));
        let mut handles = Vec::new();
        for _ in 0..5 {
            let c = counter.clone();
            let k = key.clone();
            handles.push(tokio::spawn(async move {
                cached_resolve_with(k, move || {
                    let c = c.clone();
                    async move {
                        c.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        Ok::<_, String>(quantumtv_core::media::MediaResource::new(
                            "ep-2",
                            "http://example.com/2.m3u8",
                            ResourceType::Hls,
                        ))
                    }
                })
                .await
            }));
        }
        for h in handles {
            h.await.unwrap().unwrap();
        }
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn resolve_error_not_cached() {
        // 解析失败不入缓存: 下次播放重试
        let key = uniq_key("err");
        let counter = Arc::new(AtomicU32::new(0));
        for _ in 0..2 {
            let c = counter.clone();
            let r = cached_resolve_with(key.clone(), move || {
                let c = c.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Err::<quantumtv_core::media::MediaResource, String>("resolve failed".into())
                }
            })
            .await;
            assert!(r.is_err());
        }
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }
}
