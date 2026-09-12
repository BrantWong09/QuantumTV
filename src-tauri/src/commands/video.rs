use crate::commands::config::get_config_with_db_sources;
use crate::commands::recommendation::{invalidate_recommendation_cache, RecommendationEngine};
use crate::commands::source_intelligence::SourceIntelligenceManager;
use crate::storage::StorageManager;
use image::{GenericImageView, ImageOutputFormat};
use moka::future::Cache;
use quantumtv_core::media_filter::{SkipAction, SkipDetection};
use quantumtv_core::types::{PlayGroup, SearchResult};
use quantumtv_core::{
    prefer_best_source, test_video_source, SourceTestResult as CoreSourceTestResult,
};
use regex::Regex;
use reqwest::header::{
    HeaderMap, HeaderValue, ACCEPT, ACCEPT_LANGUAGE, RANGE, REFERER, USER_AGENT,
};
use rusqlite::params;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::io::Cursor;
use std::net::IpAddr;
use std::sync::{Arc, OnceLock};
use tauri::{Emitter, Manager, State};
use tokio::sync::Semaphore;
use tokio::time::{timeout, Duration};
use url::Url;
use uuid::Uuid;

/// 缓存统计信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheStats {
    pub entry_count: u64,
    pub weighted_size: u64,
}

pub struct VideoCacheManager {
    pub cache: Cache<String, Vec<u8>>,
    pub semaphore: Arc<Semaphore>, // 并发控制
}

impl VideoCacheManager {
    pub fn new() -> Self {
        // 优化缓存配置：
        // - 最大 800 条目（从500增加到800，支持更多预加载）
        // - TTL 20分钟（从15分钟增加到20分钟，减少重复下载）
        // 假设每个 ts 片段约 1-2MB，800个约 800MB-1.6GB
        let cache = Cache::builder()
            .max_capacity(800)
            .time_to_live(std::time::Duration::from_secs(1200))
            .build();

        // 并发限制：最多同时下载30个片段
        let semaphore = Arc::new(Semaphore::new(30));

        Self { cache, semaphore }
    }

    pub async fn get(&self, url: &str) -> Option<Vec<u8>> {
        let result = self.cache.get(url).await;
        if result.is_some() {
            log::debug!("视频缓存命中: {}", url);
        }
        result
    }

    pub async fn set(&self, url: String, data: Vec<u8>) {
        log::debug!("视频缓存写入: {} ({} bytes)", url, data.len());
        self.cache.insert(url, data).await;
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats {
            entry_count: self.cache.entry_count(),
            weighted_size: self.cache.weighted_size(),
        }
    }
}

pub struct SearchCacheManager {
    pub cache: Cache<String, Vec<SearchResult>>,
}

impl SearchCacheManager {
    pub fn new() -> Self {
        // 最大 1000 条搜索结果缓存，TTL 3600 秒（1 小时）
        let cache = Cache::builder()
            .max_capacity(1000)
            .time_to_live(std::time::Duration::from_secs(3600))
            .build();
        Self { cache }
    }

    pub async fn get(&self, query: &str) -> Option<Vec<SearchResult>> {
        let key = Self::normalize_key(query);
        let result = self.cache.get(&key).await;
        if result.is_some() {
            log::debug!("搜索缓存命中: {}", query);
        }
        result
    }

    pub async fn set(&self, query: String, results: Vec<SearchResult>) {
        let key = Self::normalize_key(&query);
        log::debug!("搜索缓存写入: {} ({} 条结果)", query, results.len());
        self.cache.insert(key, results).await;
    }

    fn normalize_key(query: &str) -> String {
        query.trim().to_lowercase()
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats {
            entry_count: self.cache.entry_count(),
            weighted_size: self.cache.weighted_size(),
        }
    }
}

/// Bridge/Spider 站点级搜索缓存 (方案 §8: key=spider_id+keyword, TTL 60s)。
/// 用户"搜索→详情→返回→再搜索"与换关键词回退场景不重复打 Android Spider;
/// try_get_with 兼带请求级 SingleFlight, 失败结果不入缓存。
pub(crate) struct BridgeSearchCache {
    cache: Cache<String, Arc<Vec<SearchResult>>>,
}

impl BridgeSearchCache {
    pub fn new() -> Self {
        Self {
            cache: Cache::builder()
                .max_capacity(500)
                .time_to_live(std::time::Duration::from_secs(60))
                .build(),
        }
    }

    pub async fn get_or_insert_with<F, Fut>(
        &self,
        key: String,
        fetch: F,
    ) -> Result<Vec<SearchResult>, String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<Vec<SearchResult>, String>>,
    {
        self.cache
            .try_get_with(key, async { fetch().await.map(Arc::new) })
            .await
            .map(|v| (*v).clone())
            .map_err(|e| (*e).clone())
    }
}

pub(crate) static BRIDGE_SEARCH_CACHE: std::sync::LazyLock<BridgeSearchCache> =
    std::sync::LazyLock::new(BridgeSearchCache::new);

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GetVideoDetailOptimizedResponse {
    pub detail: SearchResult,
    pub other_sources: Vec<SearchResult>,
}

/// 播放器初始化状态响应
/// 包含播放器启动所需的所有数据，减少 IPC 通信次数
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PlayerInitialState {
    /// 视频详情
    pub detail: SearchResult,
    /// 其他可用源
    pub other_sources: Vec<SearchResult>,
    /// 播放记录（集数索引和播放时间）
    pub play_record: Option<PlayRecordInfo>,
    /// 初始化集数索引（0-based）
    pub initial_episode_index: i32,
    /// 初始化播放时间（秒）
    pub resume_time: Option<i32>,
    /// 是否已收藏
    pub is_favorited: bool,
    /// 跳过配置
    pub skip_config: Option<SkipConfigInfo>,
    /// 去广告开关
    pub block_ad_enabled: bool,
    /// 优选开关
    pub optimization_enabled: bool,
}

/// 播放记录信息
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PlayRecordInfo {
    /// 0-based index for player usage
    pub episode_index: i32,
    pub play_time: i32,
}

/// 跳过配置信息
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SkipConfigInfo {
    pub enable: bool,
    pub intro_time: i32,
    pub outro_time: i32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SkipConfigPayload {
    pub enable: bool,
    pub intro_time: f64,
    pub outro_time: f64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ChangePlaySourceRequest {
    pub current_source: Option<String>,
    pub current_id: Option<String>,
    pub new_source: String,
    pub new_id: String,
    pub available_sources: Vec<SearchResult>,
    pub current_episode_index: i32,
    pub current_play_time: f64,
    pub resume_time: Option<f64>,
    pub skip_config: Option<SkipConfigPayload>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChangePlaySourceResponse {
    pub detail: SearchResult,
    pub target_episode_index: i32,
    pub resume_time: f64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SavePlayProgressRequest {
    pub source: String,
    pub id: String,
    pub title: String,
    pub source_name: String,
    pub year: String,
    pub cover: String,
    pub episode_index: i32,
    pub total_episodes: i32,
    pub play_time: f64,
    pub total_time: f64,
    pub search_title: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct InitializePlayerByQueryRequest {
    pub query: String,
    pub filter_title: String,
    pub year: Option<String>,
    pub search_type: Option<String>,
    pub prefer_best: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct InitializePlayerByQueryResponse {
    pub results: Vec<SearchResult>,
    pub test_results: Vec<(String, CoreSourceTestResult)>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct PlayRecordMeta {
    episode_index: i32,
    play_time: i32,
    title: String,
    year: String,
    total_episodes: i32,
    search_title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchTypeFilter {
    Tv,
    Movie,
}

fn normalize_episode_index(record_episode_index: i32, total_episodes: usize) -> i32 {
    if total_episodes == 0 {
        return 0;
    }
    let zero_based = if record_episode_index <= 0 {
        0
    } else {
        record_episode_index - 1
    };
    let max_index = total_episodes.saturating_sub(1) as i32;
    zero_based.clamp(0, max_index)
}

fn resolve_initial_playback_state(play_record: Option<&PlayRecordInfo>) -> (i32, Option<i32>) {
    match play_record {
        Some(record) => (record.episode_index, Some(record.play_time)),
        None => (0, None),
    }
}

fn normalize_title_for_match(title: &str) -> String {
    title.replace(' ', "").to_lowercase()
}

fn filter_sources_for_fallback(
    results: &[SearchResult],
    title: &str,
    year: Option<&str>,
    search_type: Option<SearchTypeFilter>,
) -> Vec<SearchResult> {
    let normalized_title = normalize_title_for_match(title);
    let normalized_year = year.map(|y| y.trim().to_lowercase());

    results
        .iter()
        .filter(|result| {
            if normalize_title_for_match(&result.title) != normalized_title {
                return false;
            }

            if let Some(ref y) = normalized_year {
                if !y.is_empty() {
                    let result_year = result.year.as_deref().unwrap_or("").to_lowercase();
                    if result_year != *y {
                        return false;
                    }
                }
            }

            match search_type {
                Some(SearchTypeFilter::Tv) => result.episodes.len() > 1,
                Some(SearchTypeFilter::Movie) => result.episodes.len() == 1,
                None => true,
            }
        })
        .cloned()
        .collect()
}

fn parse_search_type_filter(search_type: Option<&str>) -> Option<SearchTypeFilter> {
    match search_type {
        Some(value) if value.eq_ignore_ascii_case("tv") => Some(SearchTypeFilter::Tv),
        Some(value) if value.eq_ignore_ascii_case("movie") => Some(SearchTypeFilter::Movie),
        _ => None,
    }
}

fn reorder_results_with_best(best: &SearchResult, results: Vec<SearchResult>) -> Vec<SearchResult> {
    let mut ordered = Vec::with_capacity(results.len());
    ordered.push(best.clone());
    ordered.extend(
        results
            .into_iter()
            .filter(|item| !(item.source == best.source && item.id == best.id)),
    );
    ordered
}

fn source_lookup_key(result: &SearchResult) -> String {
    format!("{}-{}", result.source, result.id)
}

fn reorder_results_with_source_intelligence(
    results: Vec<SearchResult>,
    manager: &SourceIntelligenceManager,
) -> Vec<SearchResult> {
    if results.len() <= 1 {
        return results;
    }

    let mut source_keys = Vec::new();
    for result in &results {
        if !source_keys.iter().any(|key| key == &result.source) {
            source_keys.push(result.source.clone());
        }
    }

    let ranked_keys = manager.rank_sources(source_keys);
    let rank_map: HashMap<String, usize> = ranked_keys
        .into_iter()
        .enumerate()
        .map(|(index, key)| (key, index))
        .collect();

    let mut indexed_results: Vec<(usize, SearchResult)> = results.into_iter().enumerate().collect();
    indexed_results.sort_by(|a, b| {
        let a_rank = rank_map.get(&a.1.source).copied().unwrap_or(usize::MAX);
        let b_rank = rank_map.get(&b.1.source).copied().unwrap_or(usize::MAX);
        a_rank.cmp(&b_rank).then(a.0.cmp(&b.0))
    });

    indexed_results
        .into_iter()
        .map(|(_, result)| result)
        .collect()
}

fn has_source_intelligence(results: &[SearchResult], manager: &SourceIntelligenceManager) -> bool {
    results
        .iter()
        .any(|result| manager.has_stats(&result.source))
}

fn persist_source_test_results(
    manager: &SourceIntelligenceManager,
    db: &crate::db::db_client::Db,
    results: &[SearchResult],
    test_results: &[(String, CoreSourceTestResult)],
) {
    let lookup_map: HashMap<String, String> = results
        .iter()
        .map(|result| (source_lookup_key(result), result.source.clone()))
        .collect();

    for (lookup_key, test_result) in test_results {
        if let Some(source_key) = lookup_map.get(lookup_key) {
            let _ = manager.record_runtime_test_result_persisted(
                db,
                source_key.clone(),
                !test_result.has_error,
                test_result.ping_time,
                if test_result.has_error {
                    Some("temporary source test failed".to_string())
                } else {
                    None
                },
            );
        }
    }
}

fn persist_encountered_sources(
    manager: &SourceIntelligenceManager,
    db: &crate::db::db_client::Db,
    results: &[SearchResult],
) {
    let source_keys = results
        .iter()
        .map(|result| result.source.clone())
        .collect::<Vec<_>>();

    let _ = manager.ensure_sources_persisted(db, source_keys);
}

async fn probe_and_persist_source_health(
    manager: &SourceIntelligenceManager,
    db: &crate::db::db_client::Db,
    detail: &SearchResult,
    episode_index: usize,
) {
    let probe_url = detail
        .episodes
        .get(episode_index)
        .or_else(|| detail.episodes.get(0));

    if let Some(url) = probe_url {
        let client = get_video_client();
        match test_video_source(client, url).await {
            Ok(result) => {
                let _ = manager.record_runtime_test_result_persisted(
                    db,
                    detail.source.clone(),
                    !result.has_error,
                    result.ping_time,
                    if result.has_error {
                        Some("playback probe failed".to_string())
                    } else {
                        None
                    },
                );
            }
            Err(_) => {
                let _ = manager.record_runtime_test_result_persisted(
                    db,
                    detail.source.clone(),
                    false,
                    0,
                    Some("playback probe failed".to_string()),
                );
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ResolvedSourceChange {
    target_episode_index: i32,
    resume_time: f64,
}

fn resolve_source_change(
    detail: &SearchResult,
    current_episode_index: i32,
    current_play_time: f64,
    resume_time: Option<f64>,
) -> ResolvedSourceChange {
    let total_episodes = detail.episodes.len() as i32;
    if total_episodes <= 0 {
        return ResolvedSourceChange {
            target_episode_index: 0,
            resume_time: 0.0,
        };
    }

    let max_index = total_episodes - 1;
    let out_of_range = current_episode_index < 0 || current_episode_index > max_index;
    let target_episode_index = if out_of_range {
        0
    } else {
        current_episode_index
    };
    let resume_time = if out_of_range {
        0.0
    } else if resume_time.unwrap_or(0.0) > 0.0 {
        resume_time.unwrap_or(0.0)
    } else if current_play_time > 1.0 {
        current_play_time
    } else {
        0.0
    };

    ResolvedSourceChange {
        target_episode_index,
        resume_time,
    }
}

fn migrate_play_source_state(
    db: &crate::db::db_client::Db,
    old_key: Option<&str>,
    new_key: &str,
    skip_config: Option<&SkipConfigPayload>,
) -> Result<(), String> {
    db.with_conn(|conn| {
        if let Some(old_key) = old_key {
            conn.execute("DELETE FROM play_records WHERE key = ?1", params![old_key])?;
            conn.execute("DELETE FROM skip_configs WHERE key = ?1", params![old_key])?;
        }

        if let Some(config) = skip_config {
            conn.execute(
                "INSERT OR REPLACE INTO skip_configs (key, enable, intro_time, outro_time) VALUES (?1, ?2, ?3, ?4)",
                params![
                    new_key,
                    if config.enable { 1 } else { 0 },
                    config.intro_time,
                    config.outro_time,
                ],
            )?;
        }

        Ok(())
    })
}

fn save_play_progress_inner(
    db: &crate::db::db_client::Db,
    request: SavePlayProgressRequest,
) -> Result<bool, String> {
    if request.source.is_empty()
        || request.id.is_empty()
        || request.title.trim().is_empty()
        || request.source_name.trim().is_empty()
    {
        return Ok(false);
    }

    let play_time = request.play_time.floor() as i32;
    let total_time = request.total_time.floor() as i32;
    if play_time < 1 || total_time <= 0 {
        return Ok(false);
    }

    let total_episodes = if request.total_episodes <= 0 {
        1
    } else {
        request.total_episodes
    };
    let episode_index = request.episode_index.max(0) + 1;
    let search_title = request.search_title.unwrap_or_default();
    let save_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs() as i32;
    let key = format!("{}+{}", request.source, request.id);

    db.with_conn(|conn| {
        conn.execute(
            "INSERT OR REPLACE INTO play_records (key, title, source_name, year, cover, episode_index, total_episodes, play_time, total_time, save_time, search_title)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                key,
                request.title,
                request.source_name,
                request.year,
                request.cover,
                episode_index,
                total_episodes,
                play_time,
                total_time,
                save_time,
                search_title,
            ],
        )?;
        Ok(())
    })?;

    Ok(true)
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SearchStreamEvent {
    pub results: Vec<SearchResult>,
    pub source: String,
    pub source_name: String,
    pub total_sources: i32,
    pub completed_sources: i32,
    /// 本次搜索的代际: 前端只接受 generation == 最新代际的事件 (方案 §7)
    pub generation: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ApiSite {
    pub key: String,
    pub api: String,
    pub name: String,
    pub detail: Option<String>,
    pub is_adult: Option<bool>,
    #[serde(default)]
    pub site_type: Option<i32>,
    #[serde(default)]
    pub spider: Option<String>,
    #[serde(default)]
    pub searchable: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ApiSearchItem {
    pub vod_id: Value, // Can be int or string
    pub vod_name: String,
    pub vod_pic: String,
    pub vod_remarks: Option<String>,
    pub vod_play_url: Option<String>,
    /// 多组源头名 (如 "百度网盘$$$夸克网盘"), 与 vod_play_url 中 $$$ 分组一一对应
    #[serde(default)]
    pub vod_play_from: Option<String>,
    pub vod_class: Option<String>,
    pub vod_year: Option<String>,
    pub vod_content: Option<String>,
    pub vod_douban_id: Option<Value>,
    pub type_name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiSearchResponse {
    pub list: Vec<ApiSearchItem>,
    pub pagecount: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SourceCategoryItem {
    pub type_id: Value,
    pub type_name: String,
    pub type_pid: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SourceCategoryResponse {
    pub class: Option<Vec<SourceCategoryItem>>,
    pub code: Option<i32>,
    pub msg: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PlayerTickRequest {
    pub current_time: f64,
    pub total_duration: f64,
    pub now_ms: i64,
    pub last_save_at_ms: i64,
    pub save_interval_ms: i64,
    pub last_skip_check_at_ms: i64,
    pub skip_enabled: bool,
    pub intro_time: f64,
    pub outro_time: f64,
    pub source: Option<String>,
    pub id: Option<String>,
    pub current_episode: Option<u32>,
    pub total_episodes: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PlayerTickDecision {
    pub should_save_progress: bool,
    pub next_last_save_at_ms: i64,
    pub next_last_skip_check_at_ms: i64,
    pub skip_action: Option<SkipAction>,
    pub did_preload: bool,
}

const PLAYER_SKIP_CHECK_INTERVAL_MS: i64 = 1500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TickTimingDecision {
    should_save_progress: bool,
    should_check_skip: bool,
    next_last_save_at_ms: i64,
    next_last_skip_check_at_ms: i64,
}

fn allow_lan_sources_from_config(config: &Value) -> bool {
    config
        .get("PlayerConfig")
        .and_then(|player_config| player_config.get("allow_lan_sources"))
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn is_local_hostname(host: &str) -> bool {
    let normalized = host.trim().trim_end_matches('.').to_ascii_lowercase();
    normalized == "localhost"
        || normalized.ends_with(".localhost")
        || normalized.ends_with(".local")
        || normalized.ends_with(".internal")
        || normalized.ends_with(".home.arpa")
        || !normalized.contains('.')
}

fn is_local_or_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(addr) => {
            addr.is_private()
                || addr.is_loopback()
                || addr.is_link_local()
                || addr.is_broadcast()
                || addr.is_documentation()
                || addr.is_unspecified()
        }
        IpAddr::V6(addr) => {
            addr.is_loopback()
                || addr.is_unique_local()
                || addr.is_unicast_link_local()
                || addr.is_unspecified()
        }
    }
}

fn validate_remote_url(url: &str, allow_lan_sources: bool) -> Result<Url, String> {
    let parsed = Url::parse(url).map_err(|e| format!("Invalid URL: {}", e))?;

    match parsed.scheme() {
        "http" | "https" => {}
        scheme => return Err(format!("Unsupported URL scheme: {}", scheme)),
    }

    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("URLs with embedded credentials are not allowed".to_string());
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| "URL must include a host".to_string())?;

    if !allow_lan_sources {
        if let Ok(ip) = host.parse::<IpAddr>() {
            if is_local_or_private_ip(ip) {
                return Err("LAN and localhost URLs are disabled".to_string());
            }
        } else if is_local_hostname(host) {
            return Err("LAN and localhost URLs are disabled".to_string());
        }
    }

    Ok(parsed)
}

fn validate_remote_url_against_config(url: &str, config: &Value) -> Result<Url, String> {
    validate_remote_url(url, allow_lan_sources_from_config(config))
}

pub(crate) fn resolve_enabled_source(config: &Value, source_key: &str) -> Option<ApiSite> {
    config
        .get("SourceConfig")
        .and_then(|v| v.as_array())
        .and_then(|sources| {
            sources.iter().find_map(|s| {
                let key = s.get("key")?.as_str()?;
                let disabled = s.get("disabled").and_then(|d| d.as_bool()).unwrap_or(false);
                if key != source_key || disabled {
                    return None;
                }
                let site = ApiSite {
                    key: key.to_string(),
                    api: s.get("api")?.as_str()?.to_string(),
                    name: s.get("name")?.as_str()?.to_string(),
                    detail: s
                        .get("detail")
                        .and_then(|v| v.as_str())
                        .map(|v| v.to_string()),
                    is_adult: s.get("is_adult").and_then(|v| v.as_bool()),
                    site_type: s.get("site_type").and_then(|v| v.as_i64()).map(|v| v as i32),
                    spider: s
                        .get("spider")
                        .and_then(|v| v.as_str())
                        .map(|v| v.to_string()),
                    searchable: s.get("searchable").and_then(|v| v.as_i64()).map(|v| v as i32),
                };
                // Spider 站点(type=3)的 api 是 csp_ 类名而非 URL，跳过 URL 校验
                if site.site_type != Some(3) {
                    validate_remote_url_against_config(&site.api, config).ok()?;
                }
                Some(site)
            })
        })
}

pub(crate) fn source_url(base_api: &str, query: &str) -> String {
    if base_api.ends_with('/') {
        format!("{base_api}{query}")
    } else {
        format!("{base_api}/{query}")
    }
}

pub(crate) fn parse_source_categories(body: &str) -> Result<Vec<SourceCategoryItem>, String> {
    let parsed = serde_json::from_str::<SourceCategoryResponse>(body).map_err(|e| e.to_string())?;
    Ok(parsed.class.unwrap_or_default())
}

pub(crate) fn parse_source_videos(body: &str) -> Result<Vec<ApiSearchItem>, String> {
    let parsed = serde_json::from_str::<ApiSearchResponse>(body).map_err(|e| e.to_string())?;
    Ok(parsed.list)
}

fn decide_tick_timing(request: &PlayerTickRequest) -> TickTimingDecision {
    let should_save_progress =
        request.now_ms - request.last_save_at_ms >= request.save_interval_ms.max(500);
    let should_check_skip =
        request.now_ms - request.last_skip_check_at_ms >= PLAYER_SKIP_CHECK_INTERVAL_MS;

    TickTimingDecision {
        should_save_progress,
        should_check_skip,
        next_last_save_at_ms: if should_save_progress {
            request.now_ms
        } else {
            request.last_save_at_ms
        },
        next_last_skip_check_at_ms: if should_check_skip {
            request.now_ms
        } else {
            request.last_skip_check_at_ms
        },
    }
}

// Douban Related Structs
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DoubanCelebrity {
    pub id: String,
    pub name: String,
    pub alt: Option<String>,
    pub avatars: Option<DoubanAvatars>,
    pub roles: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DoubanAvatars {
    pub small: String,
    pub medium: String,
    pub large: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DoubanRating {
    pub max: f32,
    pub average: f32,
    pub stars: String,
    pub min: f32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DoubanMovieDetail {
    pub id: String,
    pub title: String,
    pub original_title: Option<String>,
    pub alt: Option<String>,
    pub rating: Option<DoubanRating>,
    pub ratings_count: Option<i32>,
    pub images: Option<DoubanAvatars>,
    pub subtype: Option<String>,
    pub directors: Option<Vec<DoubanCelebrity>>,
    pub casts: Option<Vec<DoubanCelebrity>>,
    pub writers: Option<Vec<DoubanCelebrity>>,
    pub pubdates: Option<Vec<String>>,
    pub year: Option<String>,
    pub genres: Option<Vec<String>>,
    pub countries: Option<Vec<String>>,
    pub mainland_pubdate: Option<String>,
    pub aka: Option<Vec<String>>,
    pub summary: Option<String>,
    pub durations: Option<Vec<String>>,
    pub seasons_count: Option<i32>,
    pub episodes_count: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DoubanAuthor {
    pub id: String,
    pub uid: String,
    pub name: String,
    pub avatar: String,
    pub alt: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DoubanComment {
    pub id: String,
    pub created_at: String,
    pub content: String,
    pub useful_count: i32,
    pub rating: Option<DoubanRatingShort>,
    pub author: DoubanAuthor,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DoubanRatingShort {
    pub max: i32,
    pub value: f32,
    pub min: i32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DoubanCommentsResponse {
    pub start: i32,
    pub count: i32,
    pub total: i32,
    pub comments: Vec<DoubanComment>,
}
static VIDEO_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

/// 搜索代际: 每次 search_with_cache_hit 开始时自增。
/// 旧的搜索任务发现自己的代际落后即中止(点播放/发起新搜索时使旧搜索尽快断掉,
/// 不再继续向桥接发搜索请求)。
static SEARCH_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 使当前进行中的搜索全部失效(供"点播放即断搜索"命令调用)
#[tauri::command]
pub fn abort_active_search() {
    SEARCH_GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

/// 当前代际值 (缓存命中/空查询路径回填 generation 用)
pub(crate) fn current_search_generation() -> u64 {
    SEARCH_GENERATION.load(std::sync::atomic::Ordering::SeqCst)
}

/// 检查指定代际的搜索是否已被新的代际取代
fn search_generation_is_stale(my_gen: u64) -> bool {
    SEARCH_GENERATION.load(std::sync::atomic::Ordering::SeqCst) != my_gen
}

pub(crate) fn get_video_client() -> &'static reqwest::Client {
    VIDEO_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            // 大幅增加连接池大小，允许更高并发
            .pool_max_idle_per_host(150) // 从100增加到150
            .pool_idle_timeout(std::time::Duration::from_secs(180)) // 从120增加到180秒
            // 开启 TCP_NODELAY，减少小包延迟
            .tcp_nodelay(true)
            .tcp_keepalive(std::time::Duration::from_secs(60))
            // 开启自适应窗口，解决跨国高延迟下的吞吐量瓶颈
            .http2_adaptive_window(true)
            // 保持 H2 连接活跃，防止中间设备切断
            .http2_keep_alive_interval(std::time::Duration::from_secs(30))
            .http2_keep_alive_timeout(std::time::Duration::from_secs(20))
            // 增加超时时间，适应跨国慢速网络
            .timeout(std::time::Duration::from_secs(40)) // 从30增加到40秒
            .connect_timeout(std::time::Duration::from_secs(15)) // 从10增加到15秒
            // 烂证书 野鸡CDN 连接问题
            .danger_accept_invalid_certs(true) // 忽略证书无效/过期/自签名
            .danger_accept_invalid_hostnames(true) // 忽略域名不匹配
            .no_proxy() // (可选) 避免被系统代理设置干扰，直连
            // 禁用重定向限制（某些CDN可能有多次重定向）
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()
            .expect("Failed to create global video client")
    })
}
fn is_playable_m3u8(url: &str) -> bool {
    url.to_lowercase().contains(".m3u8")
}

fn clean_html_tags(html: &str) -> String {
    // Basic cleaning, more advanced can be added if needed
    html.replace("<p>", "")
        .replace("</p>", "")
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<div>", "")
        .replace("</div>", "")
}

const YELLOW_WORDS: &[&str] = &[
    "伦理片",
    "成人",
    "情色",
    "福利",
    "三上",
    "里番动漫",
    "门事件",
    "萝莉少女",
    "制服诱惑",
    "国产传媒",
    "cosplay",
    "黑丝诱惑",
    "无码",
    "日本无码",
    "有码",
    "cosplay",
    "swag",
    "av",
    "三级片",
    "日本有码",
    "SWAG",
    "网红主播",
    "色情片",
    "同性片",
    "福利视频",
    "福利片",
    "写真热舞",
    "倫理片",
    "理论片",
    "韩国伦理",
    "港台三级",
    "电影解说",
    "伦理",
    "写真",
    "诱惑",
];

/// 解析单组 "第1集$url#第2集$url" 的选集列表
/// spider_mode=true 时收录非 http 的网盘集 id (播放前经 playerContent 二次解析)
fn parse_episode_group_items(group: &str, spider_mode: bool) -> (Vec<String>, Vec<String>) {
    let mut episodes = Vec::new();
    let mut titles = Vec::new();
    let items = group.split('#');
    for item in items {
        let parts: Vec<&str> = item.split('$').collect();
        if parts.len() == 2 && (is_playable_m3u8(parts[1]) || (spider_mode && !parts[1].is_empty())) {
            titles.push(parts[0].to_string());
            episodes.push(parts[1].to_string());
        } else if parts.len() == 1 && is_playable_m3u8(parts[0]) {
            titles.push((episodes.len() + 1).to_string());
            episodes.push(parts[0].to_string());
        }
    }
    (episodes, titles)
}

/// spider_mode=true 时收录非 http 的网盘集 id (播放前经 playerContent 二次解析)
fn parse_episodes_for(play_url: &str, spider_mode: bool) -> (Vec<String>, Vec<String>) {
    let mut episodes = Vec::new();
    let mut titles = Vec::new();

    let groups = play_url.split("$$$");
    for group in groups {
        let (group_episodes, group_titles) = parse_episode_group_items(group, spider_mode);
        if group_episodes.len() > episodes.len() {
            episodes = group_episodes;
            titles = group_titles;
        }
    }
    (episodes, titles)
}

/// 拆分 vod_play_url 的全部 $$$ 线路组, 组名对齐 vod_play_from (如 "百度网盘$$$夸克网盘")
/// 返回 (默认组集数, 默认组集名, 全部线路组); 默认组 = 集数最多的一组(等长取第一组, 兼容旧行为)
fn parse_episode_groups(
    play_url: &str,
    play_from: Option<&str>,
    spider_mode: bool,
) -> (Vec<String>, Vec<String>, Vec<PlayGroup>) {
    let flags: Vec<&str> = play_from
        .unwrap_or("")
        .split("$$$")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    let mut default_episodes = Vec::new();
    let mut default_titles = Vec::new();
    let mut groups = Vec::new();

    for (idx, group) in play_url.split("$$$").enumerate() {
        let (group_episodes, group_titles) = parse_episode_group_items(group, spider_mode);
        if group_episodes.is_empty() {
            continue;
        }
        let flag = flags
            .get(idx)
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("线路{}", groups.len() + 1));
        groups.push(PlayGroup {
            flag,
            episodes: group_episodes.clone(),
            episodes_titles: group_titles.clone(),
            episodes_raw: if spider_mode {
                group_episodes.clone()
            } else {
                Vec::new()
            },
        });
        if group_episodes.len() > default_episodes.len() {
            default_episodes = group_episodes;
            default_titles = group_titles;
        }
    }

    (default_episodes, default_titles, groups)
}


pub(crate) async fn search_site_results(
    site: &ApiSite,
    query: &str,
    client: &reqwest::Client,
    cache_root: &std::path::Path,
) -> Result<Vec<SearchResult>, String> {
    if site.site_type.unwrap_or(1) == 3 {
        if site.searchable.unwrap_or(1) != 1 {
            return Ok(vec![]);
        }
        let class_name = site.api.strip_prefix("csp_").unwrap_or(&site.api);
        // 方案 §8: 站点级搜索缓存 key = spider_id + keyword (闭包须 'static, 借用字段全部 clone)
        let cache_key = format!("bridge-search:{}:{}", site.key, query);
        let class_owned = class_name.to_string();
        let query_owned = query.to_string();
        let source_key = site.key.clone();
        let source_name = site.name.clone();
        let source_site_type = site.site_type;
        let spider_owned = site.spider.clone().unwrap_or_default();
        let cache_root_owned = cache_root.to_path_buf();

        BRIDGE_SEARCH_CACHE
            .get_or_insert_with(cache_key, || async move {
                let items = if quantumtv_core::spider::is_bridge_class(&class_owned) {
                    // wex Guard 类: 走 Android 桥接 (含 OLLVM/DexNative 保护, JVM 无法加载)
                    let Some(bridge_url) = quantumtv_core::bridge::effective_url() else {
                        return Err("桥接未就绪".to_string());
                    };
                    quantumtv_core::spider::spider_bridge_search(
                        &class_owned, &query_owned, &bridge_url,
                    )
                    .await?
                } else {
                    quantumtv_core::spider::spider_search(
                        &source_key,
                        &query_owned,
                        &class_owned,
                        &spider_owned,
                        &cache_root_owned,
                    )
                    .await?
                };

                Ok(items
                    .into_iter()
                    .map(|item| {
                        let play_url = item.vod_play_url.as_deref().unwrap_or("");
                        let (episodes, episodes_titles, play_groups) =
                            parse_episode_groups(play_url, item.vod_play_from.as_deref(), true);
                        SearchResult {
                            id: match item.vod_id {
                                Value::String(s) => s,
                                Value::Number(n) => n.to_string(),
                                _ => "".to_string(),
                            },
                            title: item.vod_name.trim().to_string(),
                            poster: item.vod_pic,
                            episodes,
                            episodes_titles,
                            play_groups,
                            source: source_key.clone(),
                            source_name: source_name.clone(),
                            class: item.vod_class,
                            year: item.vod_year,
                            desc: item.vod_content.map(|c| clean_html_tags(&c)),
                            type_name: item.type_name,
                            douban_id: item
                                .vod_douban_id
                                .and_then(|v| v.as_i64())
                                .map(|v| v as i32),
                            source_site_type,
                            login_hint: None,
                            episodes_raw: Vec::new(),
                        }
                    })
                    .collect::<Vec<SearchResult>>())
            })
            .await
    } else {
        let search_url = format!(
            "{}?ac=videolist&wd={}",
            site.api,
            urlencoding::encode(query)
        );
        let resp = timeout(Duration::from_secs(6), client.get(&search_url).send())
            .await
            .map_err(|_| "CMS timeout".to_string())?
            .map_err(|e| format!("CMS request failed: {}", e))?;
        if !resp.status().is_success() {
            return Ok(vec![]);
        }
        let body = timeout(Duration::from_secs(5), resp.text())
            .await
            .map_err(|_| "CMS read timeout".to_string())?
            .map_err(|e| format!("CMS read failed: {}", e))?;

        let mut results = Vec::new();
        if let Ok(search_res) = serde_json::from_str::<ApiSearchResponse>(&body) {
            results = search_res
                .list
                .into_iter()
                .map(|item| {
                    let play_url = item.vod_play_url.as_deref().unwrap_or("");
                    let (episodes, episodes_titles, play_groups) =
                        parse_episode_groups(play_url, item.vod_play_from.as_deref(), false);
                    SearchResult {
                        id: match item.vod_id {
                            Value::String(s) => s,
                            Value::Number(n) => n.to_string(),
                            _ => "".to_string(),
                        },
                        title: item.vod_name.trim().to_string(),
                        poster: item.vod_pic,
                        episodes,
                        episodes_titles,
                        play_groups,
                        source: site.key.clone(),
                        source_name: site.name.clone(),
                        class: item.vod_class,
                        year: item.vod_year,
                        desc: item.vod_content.map(|c| clean_html_tags(&c)),
                        type_name: item.type_name,
                        douban_id: item
                            .vod_douban_id
                            .and_then(|v| v.as_i64())
                            .map(|v| v as i32),
                        source_site_type: site.site_type,
                        login_hint: None,
                        episodes_raw: Vec::new(),
                        }
                    })
                    .collect();
            }
        Ok(results)
    }
}

pub(crate) async fn search_with_cache_hit(
    query: String,
    app_handle: tauri::AppHandle,
    storage: State<'_, StorageManager>,
    cache: State<'_, SearchCacheManager>,
    db: &crate::db::db_client::Db,
) -> Result<(Vec<SearchResult>, bool, u64), String> {
    // 首先尝试从缓存获取结果
    if let Some(cached_results) = cache.get(&query).await {
        // 缓存命中不产生新代际: 返回当前值供前端对齐守卫
        return Ok((cached_results, true, current_search_generation()));
    }

    let config = get_config_with_db_sources(&storage, db)?;

    // 读取 FluidSearch 配置，判断是否启用流式搜索（从 UserPreferences 读取）
    let fluid_search = config
        .get("UserPreferences")
        .and_then(|v| v.get("fluid_search"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    // 仅在启用 FluidSearch 时才使用流式输出
    let use_streaming = fluid_search;

    let mut sites =
        if let Some(source_config) = config.get("SourceConfig").and_then(|v| v.as_array()) {
            source_config
                .iter()
                .filter_map(|s| {
                    if s.get("disabled").and_then(|d| d.as_bool()).unwrap_or(false) {
                        return None;
                    }
                    let api = s.get("api")?.as_str()?.to_string();
                    // Spider 站点(type=3)的 api 是 csp_ 类名而非 URL，跳过 URL 校验
                    let site_type3 = s.get("site_type").and_then(|v| v.as_i64()).map(|v| v as i32) == Some(3);
                    if !site_type3 {
                        validate_remote_url_against_config(&api, &config).ok()?;
                    }
                    Some(ApiSite {
                        key: s.get("key")?.as_str()?.to_string(),
                        api,
                        name: s.get("name")?.as_str()?.to_string(),
                        detail: s
                            .get("detail")
                            .and_then(|v| v.as_str())
                            .map(|v| v.to_string()),
                        is_adult: s.get("is_adult").and_then(|v| v.as_bool()),
                        site_type: s.get("site_type").and_then(|v| v.as_i64()).map(|v| v as i32),
                        spider: s
                            .get("spider")
                            .and_then(|v| v.as_str())
                            .map(|v| v.to_string()),
                        searchable: s
                            .get("searchable")
                            .and_then(|v| v.as_i64())
                            .map(|v| v as i32),
                    })
                })
                .collect::<Vec<ApiSite>>()
        } else {
            vec![]
        };

    if sites.is_empty() {
        return Ok((vec![], false, current_search_generation()));
    }

    // 读取过滤配置
    let disable_yellow_filter = config
        .get("UserPreferences")
        .and_then(|v| v.get("disable_yellow_filter"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // 如果启用过滤（disable_yellow_filter=false），在搜索前就过滤掉18+的源
    if !disable_yellow_filter {
        sites.retain(|site| !site.is_adult.unwrap_or(false));
    }

    // 过滤后如果没有源了，直接返回
    if sites.is_empty() {
        return Ok((vec![], false, current_search_generation()));
    }

    let total_sources = sites.len() as i32;

    // 缓存根目录(用于 spider JAR 缓存)
    let cache_root = app_handle
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."));

    // 本次搜索的代际(每次搜索自增, 旧代际任务在检查点自行退出)
    let my_generation = SEARCH_GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;

    let client = get_video_client();
    // 串行搜索: 每次只请求一个站点, 避免并发请求风暴触发站点限流
    // (此前 20 并发曾在几分钟内打出 200+ 请求, 被 Cloudflare 1020 封禁)
    let completed = std::cell::Cell::new(0i32);
    let app_handle_opt = if use_streaming {
        Some(app_handle.clone())
    } else {
        None
    };

    // 流式搜索失败/完成时发送事件(统一处理)
    let emit_stream_event = |completed: &std::cell::Cell<i32>,
                             site_key: &str,
                             site_name: &str,
                             results: Vec<SearchResult>| {
        if let Some(app_handle) = &app_handle_opt {
            let window = app_handle
                .get_webview_window("main")
                .or_else(|| app_handle.webview_windows().values().next().cloned());
            if let Some(window) = window {
                completed.set(completed.get() + 1);
                let _ = window.emit(
                    "search-stream-result",
                    SearchStreamEvent {
                        results,
                        source: site_key.to_string(),
                        source_name: site_name.to_string(),
                        total_sources,
                        completed_sources: completed.get(),
                        generation: my_generation,
                    },
                );
            }
        }
    };

    let mut all_results = Vec::new();
    for site in &sites {
        // 代际检查点 1: 已有更新的搜索启动(如用户点了播放), 立即停止, 不再请求后续站点
        if search_generation_is_stale(my_generation) {
            return Ok((Vec::new(), false, my_generation));
        }

        // 按 site_type 分流搜索,统一产出 Vec<SearchResult>
        let mut source_results = match search_site_results(
            site,
            &query,
            &client,
            &cache_root,
        )
        .await
        {
            Ok(results) => results,
            Err(_) => {
                // 代际已过期: 静默退出(不emit)
                if search_generation_is_stale(my_generation) {
                    return Ok((Vec::new(), false, my_generation));
                }
                emit_stream_event(&completed, &site.key, &site.name, vec![]);
                continue;
            }
        };

        // 代际检查点 2: 搜索期间用户点播放/发起新搜索, 丢弃结果并停止流式事件
        if search_generation_is_stale(my_generation) {
            return Ok((Vec::new(), false, my_generation));
        }

        // 流式输出前进行内容关键词过滤(源已经在搜索前过滤了)
        if !disable_yellow_filter {
            source_results.retain(|res| {
                let type_name = res.type_name.as_deref().unwrap_or("");
                !YELLOW_WORDS.iter().any(|w| type_name.contains(w))
            });
        }

        emit_stream_event(&completed, &site.key, &site.name, source_results.clone());
        all_results.extend(source_results);
    }

    // 代际已过期: 本轮搜索已被取代, 不聚合/不缓存/不发完成事件
    if search_generation_is_stale(my_generation) {
        return Ok((Vec::new(), false, my_generation));
    }

    // Filter duplicates
    let mut unique_results = Vec::new();
    let mut seen = std::collections::HashSet::new();
    // 记录 Spider 站点 key(其结果免"必须有集数"过滤)
    let spider_site_keys: std::collections::HashSet<String> = sites
        .iter()
        .filter(|s| s.site_type.unwrap_or(1) == 3)
        .map(|s| s.key.clone())
        .collect();
    for res in all_results {
        let key = format!("{}|{}", res.source, res.id);
        if seen.insert(key) {
            // 相关性过滤: 只保留与查询词严格相关的结果(双向包含), 滤除各站返回的无关填充项
            if !quantumtv_core::search_aggregation::is_relevant_result(
                &res.title,
                &query,
                None,
            ) {
                continue;
            }
            // 按关键词筛选成人内容
            if !disable_yellow_filter {
                let type_name = res.type_name.as_deref().unwrap_or("");
                if YELLOW_WORDS.iter().any(|w| type_name.contains(w)) {
                    continue;
                }
            }
            // Spider 站点(type=3)搜索结果通常不含播放地址(详情阶段才返回集数),
            // 不能按"必须有集数"过滤,否则结果被团灭; CMS 站点保持原有过滤
            let is_spider_site = spider_site_keys.contains(&res.source);
            if is_spider_site || !res.episodes.is_empty() {
                unique_results.push(res);
            }
        }
    }

    // Basic ranking
    unique_results.sort_by(|a, b| {
        let a_match = a.title.contains(&query);
        let b_match = b.title.contains(&query);
        if a_match && !b_match {
            std::cmp::Ordering::Less
        } else if !a_match && b_match {
            std::cmp::Ordering::Greater
        } else {
            a.title.len().cmp(&b.title.len())
        }
    });

    // 如果启用了流式搜索，发送搜索完成事件
    if use_streaming {
        // 尝试获取窗口 - 兼容桌面端和移动端
        let window = app_handle
            .get_webview_window("main")
            .or_else(|| app_handle.webview_windows().values().next().cloned());

        if let Some(window) = window {
            let _ = window.emit(
                "search-stream-completed",
                serde_json::json!({
                    "total": unique_results.len(),
                    "query": query,
                    "generation": my_generation
                }),
            );
        }
    }

    // 缓存搜索结果(空结果不缓存: spider 站点可用性波动大, 避免空结果污染缓存 1 小时)
    if !unique_results.is_empty() {
        cache.set(query, unique_results.clone()).await;
    }

    Ok((unique_results, false, my_generation))
}

#[tauri::command]
pub async fn search(
    query: String,
    app_handle: tauri::AppHandle,
    storage: State<'_, StorageManager>,
    cache: State<'_, SearchCacheManager>,
    db: State<'_, crate::db::db_client::Db>,
) -> Result<Vec<SearchResult>, String> {
    let (results, _cache_hit, _generation) =
        search_with_cache_hit(query, app_handle, storage, cache, &db).await?;
    Ok(results)
}

/// 详情缓存 (方案 §9: key = source_id + vod_id, TTL 10 分钟; 详情变化频率远低于搜索)。
/// 播放页挂载/返回再进/播放中 preload 共享同一份, 不再每次打详情接口。
static DETAIL_CACHE: std::sync::LazyLock<Cache<String, Arc<ApiSearchItem>>> =
    std::sync::LazyLock::new(|| {
        Cache::builder()
            .max_capacity(300)
            .time_to_live(std::time::Duration::from_secs(600))
            .build()
    });

/// Detail 缓存包装: 同 key 并发调用共享一次执行 (兼 SingleFlight), 失败不入缓存
async fn cached_detail_with<F, Fut>(key: String, fetch: F) -> Result<ApiSearchItem, String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<ApiSearchItem, String>>,
{
    DETAIL_CACHE
        .try_get_with(key, async { fetch().await.map(Arc::new) })
        .await
        .map(|v| (*v).clone())
        .map_err(|e| (*e).clone())
}

/// 按 site_type 分流获取详情: type=3 走 core spider_detail, type=1 走 CMS HTTP。
/// 经 DETAIL_CACHE (方案 §9/§10): 播放页返回再进/播放中 preload 不重复拉详情。
async fn fetch_detail_item(
    site: &ApiSite,
    id: &str,
    cache_root: &std::path::Path,
) -> Result<ApiSearchItem, String> {
    let key = format!("detail:{}:{}", site.key, id);
    // 闭包须 'static: 借用字段全部 clone 进 async move
    let site_type = site.site_type.unwrap_or(1);
    let class_owned = site.api.strip_prefix("csp_").unwrap_or(&site.api).to_string();
    let api_owned = site.api.clone();
    let spider_owned = site.spider.clone().unwrap_or_default();
    let source_key = site.key.clone();
    let id_owned = id.to_string();
    let cache_root_owned = cache_root.to_path_buf();

    cached_detail_with(key, move || async move {
        let item = if site_type == 3 {
            let detail = if quantumtv_core::spider::is_bridge_class(&class_owned) {
                // wex Guard 类: 走 Android 桥接
                let Some(bridge_url) = quantumtv_core::bridge::effective_url() else {
                    return Err("桥接未就绪".to_string());
                };
                quantumtv_core::spider::spider_bridge_detail(&class_owned, &id_owned, &bridge_url)
                    .await?
            } else {
                quantumtv_core::spider::spider_detail(
                    &source_key,
                    &id_owned,
                    &class_owned,
                    &spider_owned,
                    &cache_root_owned,
                )
                .await?
            };

            ApiSearchItem {
                vod_id: serde_json::Value::String(id_owned),
                vod_name: detail.vod_name,
                vod_pic: detail.vod_pic,
                vod_remarks: detail.vod_remarks,
                vod_play_url: detail.vod_play_url,
                vod_play_from: detail.vod_play_from,
                vod_class: detail.vod_class,
                vod_year: detail.vod_year,
                vod_content: detail.vod_content,
                vod_douban_id: detail.vod_douban_id,
                type_name: detail.type_name,
            }
        } else {
            let client = get_video_client();
            let url = format!("{}?ac=videolist&ids={}", api_owned, id_owned);
            let resp = timeout(Duration::from_secs(8), client.get(&url).send())
                .await
                .map_err(|_| "Failed to fetch detail: timeout".to_string())?
                .map_err(|e| format!("Failed to fetch detail: {}", e))?;
            if !resp.status().is_success() {
                return Err(format!("Failed to fetch detail: {}", resp.status()));
            }
            let body = timeout(Duration::from_secs(5), resp.text())
                .await
                .map_err(|_| "Failed to read response: timeout".to_string())?
                .map_err(|e| format!("Failed to read response: {}", e))?;
            let search_res = serde_json::from_str::<ApiSearchResponse>(&body)
                .map_err(|e| format!("Parse error: {}, body: {}", e, body))?;
            search_res
                .list
                .into_iter()
                .next()
                .ok_or_else(|| "Video not found".to_string())?
        };
        Ok(item)
    })
    .await
}
#[tauri::command]
pub async fn get_video_detail(
    source: String,
    id: String,
    storage: State<'_, StorageManager>,
    db: State<'_, crate::db::db_client::Db>,
) -> Result<SearchResult, String> {
    let config = get_config_with_db_sources(&storage, &db)?;
    let site = resolve_enabled_source(&config, &source)
        .ok_or_else(|| format!("Source not found or disabled: {}", source))?;
    let cache_root = storage
        .data_dir()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let item = fetch_detail_item(&site, &id, &cache_root).await?;

    let spider_mode = site.site_type.unwrap_or(1) == 3;
    let (episodes, episodes_titles, play_groups) = parse_episode_groups(
        item.vod_play_url.as_deref().unwrap_or(""),
        item.vod_play_from.as_deref(),
        spider_mode,
    );
    let episodes_raw = episodes.clone();

    Ok(SearchResult {
        id: match item.vod_id {
            Value::String(s) => s,
            Value::Number(n) => n.to_string(),
            _ => "".to_string(),
        },
        title: item.vod_name.trim().to_string(),
        poster: item.vod_pic,
        episodes: episodes.clone(),
        episodes_titles,
        play_groups,
        episodes_raw,
        login_hint: None,
        source: site.key,
        source_name: site.name,
        class: item.vod_class,
        year: item.vod_year,
        desc: item.vod_content.map(|c| clean_html_tags(&c)),
        type_name: item.type_name,
        douban_id: item
            .vod_douban_id
            .and_then(|v| v.as_i64())
            .map(|v| v as i32),
        source_site_type: site.site_type,
    })
}

#[tauri::command]
pub async fn get_video_detail_optimized(
    source: String,
    id: String,
    storage: State<'_, StorageManager>,
    cache: State<'_, SearchCacheManager>,
    db: State<'_, crate::db::db_client::Db>,
    also_search_similar: Option<bool>,
) -> Result<GetVideoDetailOptimizedResponse, String> {
    let config = get_config_with_db_sources(&storage, &db)?;
    let site = resolve_enabled_source(&config, &source)
        .ok_or_else(|| format!("Source not found or disabled: {}", source))?;
    let cache_root = storage
        .data_dir()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let item = fetch_detail_item(&site, &id, &cache_root).await?;

    let spider_mode = site.site_type.unwrap_or(1) == 3;
    let (episodes, episodes_titles, play_groups) = parse_episode_groups(
        item.vod_play_url.as_deref().unwrap_or(""),
        item.vod_play_from.as_deref(),
        spider_mode,
    );
    let episodes_raw = episodes.clone();

    let mut detail = SearchResult {
        id: match item.vod_id {
            Value::String(s) => s,
            Value::Number(n) => n.to_string(),
            _ => "".to_string(),
        },
        title: item.vod_name.trim().to_string(),
        poster: item.vod_pic.clone(),
        episodes: episodes.clone(),
        episodes_titles,
        play_groups,
        episodes_raw,
        login_hint: None,
        source: site.key.clone(),
        source_name: site.name.clone(),
        class: item.vod_class.clone(),
        year: item.vod_year.clone(),
        desc: item.vod_content.as_ref().map(|c| clean_html_tags(c)),
        type_name: item.type_name.clone(),
        douban_id: item
            .vod_douban_id
            .and_then(|v| v.as_i64())
            .map(|v| v as i32),
        source_site_type: site.site_type,
    };

    // Spider 网盘源首集直链化: 集是网盘分享 id(非 http), 须经 playerContent 解析为真实直链
    // (与 initialize_player_by_query 的补全路径一致; 直连 source+id 入口也必须走这一步)
    if spider_mode && !detail.episodes.is_empty() {
        enrich_first_episode_direct(&mut detail, &site).await;
    }

    // 如果需要搜索相似源，尝试从缓存快速获取
    let other_sources = if also_search_similar.unwrap_or(false) {
        // 尝试从缓存获取搜索结果
        if let Some(cached_results) = cache.get(&detail.title).await {
            // 过滤掉当前源，返回其他源
            cached_results
                .into_iter()
                .filter(|r| !(r.source == source && r.id == id))
                .collect()
        } else {
            vec![]
        }
    } else {
        vec![]
    };

    Ok(GetVideoDetailOptimizedResponse {
        detail,
        other_sources,
    })
}

#[tauri::command]
pub async fn get_source_categories(
    source_key: String,
    storage: State<'_, StorageManager>,
    db: State<'_, crate::db::db_client::Db>,
) -> Result<Vec<SourceCategoryItem>, String> {
    let config = get_config_with_db_sources(&storage, &db)?;
    let source = resolve_enabled_source(&config, &source_key)
        .ok_or_else(|| format!("Source not found or disabled: {}", source_key))?;

    let url = source_url(&source.api, "?ac=class");
    let body = get_video_client()
        .get(url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;

    parse_source_categories(&body)
}

#[tauri::command]
pub async fn get_source_videos_by_type(
    source_key: String,
    type_id: String,
    page: Option<u32>,
    storage: State<'_, StorageManager>,
    db: State<'_, crate::db::db_client::Db>,
) -> Result<Vec<ApiSearchItem>, String> {
    let config = get_config_with_db_sources(&storage, &db)?;
    let source = resolve_enabled_source(&config, &source_key)
        .ok_or_else(|| format!("Source not found or disabled: {}", source_key))?;
    let page = page.unwrap_or(1).max(1);
    let encoded_type = urlencoding::encode(type_id.trim());
    let query = format!("?ac=videolist&t={}&pg={}", encoded_type, page);
    let url = source_url(&source.api, &query);

    let body = get_video_client()
        .get(url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;

    parse_source_videos(&body)
}

#[tauri::command]
pub async fn proxy_image(
    url: String,
    title: Option<String>,
    source_name: Option<String>,
    year: Option<String>,
    category: Option<String>,
    rating: Option<f64>,
    storage: State<'_, StorageManager>,
    cache_manager: State<'_, crate::db::image_cache::ImageCacheManager>,
) -> Result<Vec<u8>, String> {
    let data = storage.get_data()?;
    validate_remote_url_against_config(&url, &data.config)?;

    // 1. 先尝试从 SQLite 缓存获取
    match cache_manager.get(&url) {
        Ok(Some(data)) => {
            return Ok(data);
        }
        Ok(None) => {
            // 缓存未命中，继续请求
        }
        Err(e) => {
            eprintln!("Failed to get cached image: {}", e);
            // 缓存读取失败，继续请求
        }
    }

    // 2. 使用全局 Client 获取图片
    let client = get_video_client();

    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/122.0.0.0 Safari/537.36"));
    headers.insert(
        ACCEPT,
        HeaderValue::from_static(
            "image/avif,image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8",
        ),
    );
    headers.insert(
        reqwest::header::ACCEPT_ENCODING,
        HeaderValue::from_static("gzip, deflate, br"),
    );

    if url.contains("doubanio.com") {
        headers.insert(REFERER, HeaderValue::from_static("https://www.douban.com/"));
    }

    let fetch_result = async {
        let resp = client
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if !resp.status().is_success() {
            return Err(format!("Failed to fetch image: {}", resp.status()));
        }

        resp.bytes().await.map_err(|e| e.to_string())
    }
    .await;

    let bytes = match fetch_result {
        Ok(bytes) => bytes,
        Err(e) => {
            // 上游 URL 已失效（例如豆瓣签名过期）。回退到过期但仍存在的缓存数据，
            // 避免历史记录/推荐位封面陷入无限加载。
            if let Ok(Some(stale)) = cache_manager.get_stale(&url) {
                eprintln!(
                    "Image fetch failed ({}), serving stale cache for {}",
                    e, url
                );
                return Ok(stale);
            }
            return Err(e);
        }
    };

    // 3. 压缩图片
    let process_result = tokio::task::spawn_blocking(move || {
        let img = image::load_from_memory(&bytes).map_err(|e| format!("图片解码失败: {}", e))?;
        let (width, height) = img.dimensions();
        let processed_img = if width > 800 {
            img.resize(
                800,
                800 * height / width,
                image::imageops::FilterType::Triangle,
            )
        } else {
            img
        };

        let mut buf = Vec::new();
        let mut cursor = Cursor::new(&mut buf);
        processed_img
            .write_to(&mut cursor, ImageOutputFormat::Jpeg(70))
            .map_err(|e| format!("图片编码失败: {}", e))?;

        Ok::<Vec<u8>, String>(buf)
    })
    .await
    .map_err(|e| e.to_string())?;

    let compressed_bytes = match process_result {
        Ok(buf) => buf,
        Err(e) => {
            if let Ok(Some(stale)) = cache_manager.get_stale(&url) {
                eprintln!(
                    "Image decode failed ({}), serving stale cache for {}",
                    e, url
                );
                return Ok(stale);
            }
            return Err(e);
        }
    };

    // 4. 保存到 SQLite 缓存（带元数据）
    if let Err(e) = cache_manager.set_with_metadata(
        &url,
        &compressed_bytes,
        title.as_deref(),
        source_name.as_deref(),
        year.as_deref(),
        category.as_deref(),
        rating,
    ) {
        eprintln!("Failed to save image to cache: {}", e);
    }

    Ok(compressed_bytes)
}

// 带重试和指数退避的请求 重试3次
async fn fetch_with_retry(
    url: &str,
    method: reqwest::Method,
    headers: HeaderMap,
) -> Result<reqwest::Response, String> {
    let client = get_video_client();
    let mut retries = 3; // 增加到3次
    let mut delay_ms = 300; // 初始延迟300ms

    loop {
        let req = client
            .request(method.clone(), url)
            .headers(headers.clone())
            .timeout(std::time::Duration::from_secs(20)); // 增加到20秒超时

        match req.send().await {
            Ok(resp) => {
                if resp.status().is_success() {
                    return Ok(resp);
                } else if resp.status().as_u16() == 404 {
                    // 404不需要重试
                    return Err(format!("404 Not Found: {}", url));
                }
                // 其他错误状态继续重试
                retries -= 1;
                if retries == 0 {
                    return Err(format!("HTTP {}: {}", resp.status(), url));
                }
            }
            Err(e) => {
                retries -= 1;
                if retries == 0 {
                    return Err(format!("Network error: {} - {}", e, url));
                }
            }
        }

        // 指数退避
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        delay_ms = (delay_ms * 2).min(3000); // 最大延迟3秒
    }
}
#[derive(Debug, Serialize, Deserialize)]
pub struct FetchBinaryResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

#[tauri::command]
pub async fn fetch_binary(
    url: String,
    method: Option<String>,
    headers_opt: Option<std::collections::HashMap<String, String>>,
    storage: State<'_, StorageManager>,
    cache_manager: State<'_, VideoCacheManager>,
) -> Result<FetchBinaryResponse, String> {
    let data = storage.get_data()?;
    validate_remote_url_against_config(&url, &data.config)?;

    let method_str = method.unwrap_or_else(|| "GET".to_string());
    let is_get = method_str.to_uppercase() == "GET";

    // 1. 尝试从缓存获取 (仅无 Range 的整片请求; 带 Range 的网盘直链每次区段不同, 不走缓存)
    if is_get {
        if let Some(cached_data) = cache_manager.get(&url).await {
            return Ok(FetchBinaryResponse {
                status: 200,
                body: cached_data,
            });
        }
    }

    // 2. 准备 Headers
    let mut final_headers = HeaderMap::new();
    if let Some(h) = headers_opt.clone() {
        for (k, v) in h {
            if let Ok(name) = reqwest::header::HeaderName::from_bytes(k.as_bytes()) {
                if let Ok(value) = HeaderValue::from_str(&v) {
                    final_headers.insert(name, value);
                }
            }
        }
    }
    if !final_headers.contains_key(USER_AGENT) {
        final_headers.insert(USER_AGENT, HeaderValue::from_static("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/122.0.0.0 Safari/537.36"));
    }
    if url.contains("doubanio.com") || url.contains("douban.com") {
        final_headers.insert(REFERER, HeaderValue::from_static("https://www.douban.com/"));
    }
    if url.contains(".ts") {
        // 添加 Range 头
        final_headers.insert(RANGE, HeaderValue::from_static("bytes=0-"));
    }
    // 网盘直链 (百度 PCS 等): UA 校验严格, headers_opt 显式传入的 UA 优先
    // (上面已合并 headers_opt; 此处仅兜底补一个 Android 播放器形态的 UA)
    let is_netdisk_direct = url.contains("baidupcs.com")
        || url.contains(".pcs.baidu.com")
        || url.contains("pcsdata.baidu.com");
    if is_netdisk_direct && !final_headers.contains_key(USER_AGENT) {
        final_headers.insert(
            USER_AGENT,
            HeaderValue::from_static(
                "com.android.chrome/131.0.6778.200 (Linux;Android 10) AndroidXMedia3/1.5.1",
            ),
        );
    }
    let req_method = match method_str.to_uppercase().as_str() {
        "POST" => reqwest::Method::POST,
        "HEAD" => reqwest::Method::HEAD,
        _ => reqwest::Method::GET,
    };
    // 网盘直链是 Range 分段请求, fetch_with_retry 的 20s 超时对 4MB 分块偏紧且
    // 403 (签名过期) 重试无意义 → 网盘直链单独走一次性请求
    let resp = if is_netdisk_direct {
        let client = get_video_client();
        client
            .request(reqwest::Method::GET, &url)
            .headers(final_headers)
            .timeout(std::time::Duration::from_secs(60))
            .send()
            .await
            .map_err(|e| format!("netdisk fetch error: {} - {}", e, url))?
    } else {
        // 3. 执行带重试的网络请求
        fetch_with_retry(&url, req_method, final_headers).await?
    };
    let status = resp.status().as_u16();
    let body_bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    let body = body_bytes.to_vec();

    // 4. 只有成功的 GET 请求才存入缓存并触发预取
    // (网盘直链 206 部分响应不入缓存; 403/416 是签名过期, 直接把状态透传给前端换链)
    if is_get && status == 200 && !is_netdisk_direct {
        cache_manager.set(url.clone(), body.clone()).await;

        if url.contains(".ts") {
            let cache_clone = cache_manager.cache.clone();
            let semaphore_clone = cache_manager.semaphore.clone();
            let headers_clone = headers_opt.clone();

            tokio::spawn(async move {
                prefetch_next_segments(url, headers_clone, cache_clone, semaphore_clone).await;
            });
        }
    }

    Ok(FetchBinaryResponse {
        status,
        body,
    })
}

/// 获取 M3U8 内容并可选地进行去广告处理
///
/// # 参数
/// - `url`: M3U8 文件的 URL
/// - `enable_ad_block`: 是否启用去广告功能，默认为 false
/// - `headers_opt`: 可选的自定义 HTTP 请求头
///
/// # 返回
/// 处理后的 M3U8 文本内容
#[tauri::command]
pub async fn fetch_m3u8(
    url: String,
    enable_ad_block: Option<bool>,
    headers_opt: Option<std::collections::HashMap<String, String>>,
    storage: State<'_, StorageManager>,
) -> Result<String, String> {
    let data = storage.get_data()?;
    validate_remote_url_against_config(&url, &data.config)?;

    // 准备 HTTP 请求头
    let mut final_headers = HeaderMap::new();
    if let Some(h) = headers_opt {
        for (k, v) in h {
            if let Ok(name) = reqwest::header::HeaderName::from_bytes(k.as_bytes()) {
                if let Ok(value) = HeaderValue::from_str(&v) {
                    final_headers.insert(name, value);
                }
            }
        }
    }

    // 添加默认 User-Agent
    if !final_headers.contains_key(USER_AGENT) {
        final_headers.insert(
            USER_AGENT,
            HeaderValue::from_static("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/122.0.0.0 Safari/537.36")
        );
    }

    // 为特定域名添加 Referer
    if url.contains("doubanio.com") || url.contains("douban.com") {
        final_headers.insert(REFERER, HeaderValue::from_static("https://www.douban.com/"));
    }

    // 执行带重试的 HTTP 请求
    let resp = fetch_with_retry(&url, reqwest::Method::GET, final_headers).await?;
    let body_bytes = resp.bytes().await.map_err(|e| e.to_string())?;

    // 解码为 UTF-8 文本
    let content = String::from_utf8(body_bytes.to_vec())
        .map_err(|e| format!("无法将 M3U8 内容解码为 UTF-8: {}", e))?;

    // 如果启用了去广告，则调用 core 中的过滤函数
    let result = if enable_ad_block.unwrap_or(false) {
        quantumtv_core::filter_ads_from_m3_u8(&content)
    } else {
        content
    };

    Ok(result)
}

// 预测并预取后续分片（优化版：更多并发+更多预取）
async fn prefetch_next_segments(
    current_url: String,
    headers: Option<HashMap<String, String>>,
    cache: Cache<String, Vec<u8>>,
    semaphore: Arc<Semaphore>,
) {
    // 简单的数字预测 logic: 查找末尾连续的数字
    // 如 segment_01.ts -> segment_02.ts
    let re = Regex::new(r"(\d+)(\.ts.*)$").unwrap();
    let Some(caps) = re.captures(&current_url) else {
        return;
    };

    let num_str = caps.get(1).unwrap().as_str();
    let suffix = caps.get(2).unwrap().as_str();
    let prefix = &current_url[..caps.get(1).unwrap().start()];

    let Ok(current_num) = num_str.parse::<u64>() else {
        return;
    };
    let padding = num_str.len();

    // 使用全局 Client
    let client = get_video_client();

    // 优化的 HTTP 客户端配置
    let mut final_headers = HeaderMap::new();
    if let Some(h) = headers {
        for (k, v) in h {
            if let Ok(name) = reqwest::header::HeaderName::from_bytes(k.as_bytes()) {
                if let Ok(value) = HeaderValue::from_str(&v) {
                    final_headers.insert(name, value);
                }
            }
        }
    }
    if !final_headers.contains_key(USER_AGENT) {
        final_headers.insert(USER_AGENT, HeaderValue::from_static("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/122.0.0.0 Safari/537.36"));
    }
    // 直接加上 Range
    final_headers.insert(RANGE, HeaderValue::from_static("bytes=0-"));

    // 预取接下来的 25 个分片（从15增加到25）
    let mut handles = Vec::new();
    for i in 1..=25 {
        let next_num = current_num + i;
        let next_num_str = format!("{:0width$}", next_num, width = padding);
        let next_url = format!("{}{}{}", prefix, next_num_str, suffix);

        // 已有缓存，跳过
        if cache.contains_key(&next_url) {
            continue;
        }

        // 并发下载（使用信号量控制）
        let client_clone = client.clone();
        let request_headers = final_headers.clone();
        let cache_clone = cache.clone();
        let next_url_clone = next_url.clone();
        let semaphore_clone = semaphore.clone();

        let handle = tokio::spawn(async move {
            // 获取信号量许可
            let _permit = semaphore_clone.acquire().await.ok();

            // 增强的重试逻辑：3次重试
            let mut retries = 3;
            let mut delay_ms = 200;

            while retries > 0 {
                let resp = timeout(
                    Duration::from_secs(15), // 15秒超时
                    client_clone
                        .get(&next_url_clone)
                        .headers(request_headers.clone())
                        .send(),
                )
                .await;

                match resp {
                    Ok(Ok(r)) if r.status().is_success() => {
                        if let Ok(data) = r.bytes().await {
                            // moka 会自动处理 LRU 淘汰和 TTL 过期
                            cache_clone
                                .insert(next_url_clone.clone(), data.to_vec())
                                .await;
                            log::debug!("✅ 预取成功: {} ({} bytes)", next_url_clone, data.len());
                        }
                        break;
                    }
                    Ok(Ok(r)) if r.status().as_u16() == 404 => {
                        // 404说明后续片段不存在，停止预取
                        log::debug!("⏹️ 预取停止（404）: {}", next_url_clone);
                        break;
                    }
                    _ => {
                        retries -= 1;
                        if retries > 0 {
                            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                            delay_ms = (delay_ms * 2).min(2000);
                        } else {
                            log::debug!("❌ 预取失败: {}", next_url_clone);
                        }
                    }
                }
            }
        });

        handles.push(handle);
    }

    // 等待所有预取任务完成
    for handle in handles {
        let _ = handle.await;
    }
}

#[tauri::command]
pub async fn get_douban_data(
    subject_id: String,
    data_type: String, // "full" or "comments"
    start: Option<i32>,
    count: Option<i32>,
) -> Result<Value, String> {
    // 使用全局 Client 请求
    let client = get_video_client();
    let url = if data_type == "comments" {
        format!(
            "https://movie.douban.com/subject/{}/comments?status=P&sort=new_score&start={}&count={}",
            subject_id,
            start.unwrap_or(0),
            count.unwrap_or(20)
        )
    } else {
        format!("https://movie.douban.com/subject/{}/", subject_id)
    };

    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/122.0.0.0 Safari/537.36"));
    headers.insert(ACCEPT, HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8"));
    headers.insert(
        ACCEPT_LANGUAGE,
        HeaderValue::from_static("zh-CN,zh;q=0.9,en;q=0.8"),
    );
    headers.insert(
        REFERER,
        HeaderValue::from_static("https://movie.douban.com/"),
    );
    headers.insert(
        reqwest::header::ACCEPT_ENCODING,
        HeaderValue::from_static("gzip, deflate, br"),
    );

    let resp = client
        .get(&url)
        .headers(headers)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        return Err(format!(
            "Douban request failed with status: {}",
            resp.status()
        ));
    }

    let html_content = resp.text().await.map_err(|e| e.to_string())?;
    let document = Html::parse_document(&html_content);

    if data_type == "comments" {
        let mut comments = Vec::new();
        let comment_selector = Selector::parse(".comment-item").unwrap();
        let avatar_selector = Selector::parse(".avatar a img").unwrap();
        let user_link_selector = Selector::parse(".comment-info a").unwrap();
        let rating_selector = Selector::parse(".comment-info .rating").unwrap();
        let short_selector = Selector::parse(".short").unwrap();
        let time_selector = Selector::parse(".comment-time").unwrap();
        let vote_selector = Selector::parse(".vote-count").unwrap();

        for element in document.select(&comment_selector) {
            let avatar_url = element
                .select(&avatar_selector)
                .next()
                .and_then(|img| img.value().attr("src"))
                .unwrap_or("")
                .replace("/u/pido/", "/u/")
                .replace("s_ratio", "m_ratio");

            let user_element = element.select(&user_link_selector).next();
            let user_name = user_element
                .map(|a| a.text().collect::<String>().trim().to_string())
                .unwrap_or_default();
            let user_link = user_element
                .and_then(|a| a.value().attr("href"))
                .unwrap_or("");
            let user_id = user_link
                .split('/')
                .filter(|s| !s.is_empty())
                .last()
                .unwrap_or("")
                .to_string();

            let rating_value = element
                .select(&rating_selector)
                .next()
                .and_then(|span| span.value().attr("class"))
                .and_then(|c| {
                    let re = Regex::new(r"allstar(\d+)").unwrap();
                    re.captures(c)
                        .and_then(|cap| cap.get(1))
                        .map(|m| m.as_str().parse::<f32>().unwrap_or(0.0) / 10.0)
                })
                .unwrap_or(0.0);

            let content = element
                .select(&short_selector)
                .next()
                .map(|s| s.text().collect::<String>().trim().to_string())
                .unwrap_or_default();

            let time_element = element.select(&time_selector).next();
            let mut time = String::new();
            if let Some(t) = time_element {
                if let Some(title_attr) = t.value().attr("title") {
                    time = title_attr.to_string();
                } else {
                    time = t.text().collect::<String>().trim().to_string();
                }
            }

            let useful_count = element
                .select(&vote_selector)
                .next()
                .map(|v| {
                    v.text()
                        .collect::<String>()
                        .trim()
                        .parse::<i32>()
                        .unwrap_or(0)
                })
                .unwrap_or(0);

            let comment_id = element
                .value()
                .attr("data-cid")
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("sc_{}", Uuid::new_v4()));

            if !content.is_empty() {
                comments.push(DoubanComment {
                    id: comment_id,
                    created_at: time,
                    content,
                    useful_count,
                    rating: if rating_value > 0.0 {
                        Some(DoubanRatingShort {
                            max: 5,
                            value: rating_value,
                            min: 0,
                        })
                    } else {
                        None
                    },
                    author: DoubanAuthor {
                        id: user_id,
                        uid: user_name.clone(),
                        name: user_name,
                        avatar: avatar_url,
                        alt: Some(user_link.to_string()),
                    },
                });
            }
        }

        let total_selector = Selector::parse(".mod-hd h2 span").unwrap();
        let total_text = document
            .select(&total_selector)
            .next()
            .map(|s| s.text().collect::<String>())
            .unwrap_or_default();
        let re = Regex::new(r"(\d+)").unwrap();
        let total = re
            .captures(&total_text)
            .and_then(|cap| cap.get(1))
            .map(|m| m.as_str().parse::<i32>().unwrap_or(0))
            .unwrap_or(comments.len() as i32);

        let res = DoubanCommentsResponse {
            start: start.unwrap_or(0),
            count: count.unwrap_or(comments.len() as i32),
            total,
            comments,
        };
        Ok(serde_json::to_value(res).unwrap())
    } else {
        // Full subject data
        let title_selector = Selector::parse("span[property='v:itemreviewed']").unwrap();
        let title = document
            .select(&title_selector)
            .next()
            .map(|s| s.text().collect::<String>().trim().to_string())
            .unwrap_or_else(|| {
                let t_selector = Selector::parse("title").unwrap();
                document
                    .select(&t_selector)
                    .next()
                    .map(|s| {
                        s.text()
                            .collect::<String>()
                            .split(' ')
                            .next()
                            .unwrap_or("")
                            .to_string()
                    })
                    .unwrap_or_default()
            });

        let year_selector = Selector::parse("span.year").unwrap();
        let year = document.select(&year_selector).next().map(|s| {
            s.text()
                .collect::<String>()
                .replace(['(', ')'], "")
                .trim()
                .to_string()
        });

        let rating_selector = Selector::parse("strong.rating_num").unwrap();
        let rating_avg = document
            .select(&rating_selector)
            .next()
            .and_then(|s| s.text().collect::<String>().trim().parse::<f32>().ok())
            .unwrap_or(0.0);

        let votes_selector = Selector::parse("span[property='v:votes']").unwrap();
        let rating_count = document
            .select(&votes_selector)
            .next()
            .and_then(|s| s.text().collect::<String>().trim().parse::<i32>().ok())
            .unwrap_or(0);

        let genre_selector = Selector::parse("span[property='v:genre']").unwrap();
        let genres: Vec<String> = document
            .select(&genre_selector)
            .map(|s| s.text().collect::<String>().trim().to_string())
            .collect();

        let duration_selector = Selector::parse("span[property='v:runtime']").unwrap();
        let durations: Vec<String> = document
            .select(&duration_selector)
            .map(|s| s.text().collect::<String>().trim().to_string())
            .collect();

        let summary_selector = Selector::parse("span[property='v:summary']").unwrap();
        let summary_hidden_selector = Selector::parse("span.all.hidden").unwrap();
        let summary = document
            .select(&summary_hidden_selector)
            .next()
            .or_else(|| document.select(&summary_selector).next())
            .map(|s| {
                s.text()
                    .collect::<String>()
                    .trim()
                    .replace('\n', " ")
                    .to_string()
            });

        let poster_selector = Selector::parse("#mainpic img").unwrap();
        let poster = document
            .select(&poster_selector)
            .next()
            .and_then(|img| img.value().attr("src"))
            .unwrap_or("")
            .to_string();

        let mut directors = Vec::new();
        let director_selector = Selector::parse("a[rel='v:directedBy']").unwrap();
        for el in document.select(&director_selector) {
            let name = el.text().collect::<String>().trim().to_string();
            let href = el.value().attr("href").unwrap_or("");
            let id = href
                .split('/')
                .filter(|s| !s.is_empty())
                .last()
                .unwrap_or("")
                .to_string();
            if !name.is_empty() {
                directors.push(DoubanCelebrity {
                    id,
                    name,
                    alt: Some(href.to_string()),
                    avatars: None,
                    roles: Some(vec!["导演".to_string()]),
                });
            }
        }

        let mut casts = Vec::new();
        let actor_selector = Selector::parse("a[rel='v:starring']").unwrap();
        for el in document.select(&actor_selector) {
            let name = el.text().collect::<String>().trim().to_string();
            let href = el.value().attr("href").unwrap_or("");
            let id = href
                .split('/')
                .filter(|s| !s.is_empty())
                .last()
                .unwrap_or("")
                .to_string();
            if !name.is_empty() {
                casts.push(DoubanCelebrity {
                    id,
                    name,
                    alt: Some(href.to_string()),
                    avatars: None,
                    roles: None,
                });
            }
        }

        let detail = DoubanMovieDetail {
            id: subject_id,
            title,
            original_title: None, // Simplified
            alt: Some(url),
            rating: if rating_avg > 0.0 {
                Some(DoubanRating {
                    max: 10.0,
                    average: rating_avg,
                    stars: "".to_string(),
                    min: 0.0,
                })
            } else {
                None
            },
            ratings_count: Some(rating_count),
            images: Some(DoubanAvatars {
                small: poster.clone(),
                medium: poster.clone(),
                large: poster,
            }),
            subtype: Some("movie".to_string()),
            directors: Some(directors),
            casts: Some(casts),
            writers: None,
            pubdates: None,
            year,
            genres: Some(genres),
            countries: None, // Parsing from text is complex, skip for now
            mainland_pubdate: None,
            aka: None,
            summary,
            durations: Some(durations),
            seasons_count: None,
            episodes_count: None,
        };
        Ok(serde_json::to_value(detail).unwrap())
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PreferBestSourceResponse {
    pub best_source: SearchResult,
    pub test_results: Vec<(String, CoreSourceTestResult)>,
}

/// 从多个播放源中选择最佳源
#[tauri::command]
pub async fn prefer_best_source_command(
    sources: Vec<SearchResult>,
    db: State<'_, crate::db::db_client::Db>,
    source_manager: State<'_, SourceIntelligenceManager>,
) -> Result<PreferBestSourceResponse, String> {
    let client = get_video_client();
    let (best_source, test_results) = prefer_best_source(client, sources.clone()).await?;
    persist_source_test_results(&source_manager, &db, &sources, &test_results);

    Ok(PreferBestSourceResponse {
        best_source,
        test_results,
    })
}

/// 测试单个视频源质量
#[tauri::command]
pub async fn test_video_source_command(
    m3u8_url: String,
    source_key: Option<String>,
    db: State<'_, crate::db::db_client::Db>,
    source_manager: State<'_, SourceIntelligenceManager>,
) -> Result<CoreSourceTestResult, String> {
    let client = get_video_client();
    let result = test_video_source(client, &m3u8_url).await?;

    if let Some(source_key) = source_key.filter(|key| !key.trim().is_empty()) {
        let _ = source_manager.record_runtime_test_result_persisted(
            &db,
            source_key,
            !result.has_error,
            result.ping_time,
            if result.has_error {
                Some("manual source test failed".to_string())
            } else {
                None
            },
        );
    }

    Ok(result)
}

#[tauri::command]
pub async fn initialize_player_by_query(
    request: InitializePlayerByQueryRequest,
    app_handle: tauri::AppHandle,
    storage: State<'_, StorageManager>,
    cache: State<'_, SearchCacheManager>,
    db: State<'_, crate::db::db_client::Db>,
    source_manager: State<'_, SourceIntelligenceManager>,
) -> Result<InitializePlayerByQueryResponse, String> {
    let query = request.query.trim();
    if query.is_empty() {
        return Err("Missing query".to_string());
    }

    let (results, _, _generation) =
        search_with_cache_hit(query.to_string(), app_handle.clone(), storage.clone(), cache, &db)
            .await?;
    let filter_title = request.filter_title.trim();
    let filter_year = request
        .year
        .as_deref()
        .map(|v| v.trim())
        .filter(|v| !v.is_empty());
    let search_type = parse_search_type_filter(request.search_type.as_deref());

    let mut filtered = if filter_title.is_empty() {
        results
    } else {
        filter_sources_for_fallback(&results, filter_title, filter_year, search_type)
    };

    filtered = reorder_results_with_source_intelligence(filtered, &source_manager);
    persist_encountered_sources(&source_manager, &db, &filtered);

    let mut test_results = Vec::new();
    if request.prefer_best
        && filtered.len() > 1
        && !has_source_intelligence(&filtered, &source_manager)
    {
        let client = get_video_client();
        let (best, tests) = prefer_best_source(client, filtered.clone()).await?;
        persist_source_test_results(&source_manager, &db, &filtered, &tests);
        test_results = tests;
        filtered = reorder_results_with_best(&best, filtered);
    }

    // Spider 站点(type=3)的搜索结果不带集数, 播放前须调详情接口补全首选源
    if let Some(first) = filtered.first() {
        if first.episodes.is_empty() {
            let config = get_config_with_db_sources(&storage, &db)?;
            let cache_root = app_handle
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."));
            if let Some(site) = resolve_enabled_source(&config, &first.source) {
                if site.site_type.unwrap_or(1) == 3 {
                    match fetch_detail_item(&site, &first.id, &cache_root).await {
                        Ok(item) => {
                            let (episodes, episodes_titles, play_groups) = parse_episode_groups(
                                item.vod_play_url.as_deref().unwrap_or(""),
                                item.vod_play_from.as_deref(),
                                true,
                            );
                            filtered[0] = SearchResult {
                                episodes: episodes.clone(),
                                episodes_titles,
                                play_groups,
                                source_site_type: Some(3),
                                login_hint: None,
                                episodes_raw: episodes,
                                ..first.clone()
                            };
                            // 首集直链化: 网盘 id → playerContent → 直链; 未登录时置 login_hint
                            enrich_first_episode_direct(&mut filtered[0], &site).await;
                        }
                        Err(e) => {
                            log::warn!("[播放] 补全 spider 首选源集数失败 ({}): {}", first.source, e);
                        }
                    }
                }
            }
        }
    }

    Ok(InitializePlayerByQueryResponse {
        results: filtered,
        test_results,
    })
}

/// Spider 网盘源首集直链化: playerContent 解析第 1 集, 剩余集经 ResolverManager 按需解析 (playback_play_episode)
/// 解析为空直链(未登录网盘)时置 login_hint, 前端展示引导
async fn enrich_first_episode_direct(result: &mut SearchResult, site: &ApiSite) {
    let class_name = site.api.strip_prefix("csp_").unwrap_or(&site.api);
    let Some(bridge_url) = quantumtv_core::bridge::effective_url() else {
        result.login_hint = Some("桥接未就绪, 无法解析网盘资源".into());
        return;
    };
    let Some(first_raw) = result.episodes_raw.first().cloned() else {
        return;
    };
    // 与 playback_play_episode 共用 Resolve 缓存 + SingleFlight (方案 §10/§13):
    // 详情阶段解析过首集后, 点击播放第 1 集直接命中缓存, 不再发第二次 playerContent。
    // flag 取默认组 (前端 activeGroupIndex=0 即 play_groups[0].flag), 保证 key 对齐;
    // wex 系 playerContent 的 flag 对网盘组不敏感(组序已在 id 内编码)
    let flag = result
        .play_groups
        .first()
        .map(|g| g.flag.clone())
        .unwrap_or_default();
    let resolve_key = format!("resolve:{}:{}:{}:{}", site.key, result.id, flag, first_raw);
    let class_c = class_name.to_string();
    let source_c = site.key.clone();
    let first_c = first_raw.clone();
    let bridge_c = bridge_url.clone();
    let flag_c = flag.clone();
    let resolved = crate::commands::playback::cached_resolve_with(resolve_key, move || async move {
        let manager = quantumtv_core::resolver::ResolverManager::with_defaults(Arc::new(
            quantumtv_core::spider::BridgeSpiderPlayFetcher { bridge_url: bridge_c },
        ));
        manager
            .resolve(&quantumtv_core::resolver::ResolveInput::spider(
                source_c, flag_c, first_c, &class_c,
            ))
            .await
            .map_err(|e| e.to_string())
    })
    .await;
    match resolved {
        Ok(resource) => {
            let resource = (*resource).clone();
            // V2 Phase 3: 网盘直链经 PlaybackGateway 包装 (opaque token)
            let final_url = match quantumtv_core::gateway::wrap_resource(&resource).await {
                Ok(url) => url,
                Err(e) => {
                    log::warn!(
                        "[播放解析] 首集 Gateway 包装失败, 回退旧代理路径: {}",
                        quantumtv_core::spider::trunc(&e, 120)
                    );
                    match quantumtv_core::netdisk_proxy::ensure_started().await {
                        Ok(port) => quantumtv_core::netdisk_proxy::wrap_proxy_url(
                            &resource.url,
                            resource.user_agent.as_deref(),
                            port,
                        ),
                        Err(_) => resource.url.clone(),
                    }
                }
            };
            log::info!(
                "[播放解析] 首集直链化成功 ({}): url={}",
                site.key,
                quantumtv_core::spider::trunc(&final_url, 100)
            );
            if !result.episodes.is_empty() {
                result.episodes[0] = final_url; // 第 1 集已是可播地址
            }
            // 其余集保持 raw id, 前端切集时逐集解析
        }
        Err(err) => {
            // 解析失败: 保留 raw id(前端会再试), 并置提示
            log::warn!("[播放解析] 首集直链化失败 ({}): {}", site.key, quantumtv_core::spider::trunc(&err.to_string(), 200));
            result.login_hint = Some(err.to_string());
        }
    }
}

/// 按站点 key+视频 id 调详情接口, 返回补全集数后的 SearchResult (Spider 站点播放前必需)
async fn fetch_detail_for_source_key(
    source_key: &str,
    id: &str,
    storage: &StorageManager,
    db: &crate::db::db_client::Db,
) -> Result<SearchResult, String> {
    let config = get_config_with_db_sources(storage, db)?;
    let site = resolve_enabled_source(&config, source_key)
        .ok_or_else(|| format!("Source not found or disabled: {}", source_key))?;
    let cache_root = std::env::temp_dir();
    let item = fetch_detail_item(&site, id, &cache_root).await?;

    let (episodes, episodes_titles, play_groups) = parse_episode_groups(
        item.vod_play_url.as_deref().unwrap_or(""),
        item.vod_play_from.as_deref(),
        site.site_type.unwrap_or(1) == 3,
    );
    Ok(SearchResult {
        id: match item.vod_id {
            Value::String(s) => s,
            Value::Number(n) => n.to_string(),
            _ => id.to_string(),
        },
        title: item.vod_name.trim().to_string(),
        poster: item.vod_pic,
        episodes,
        episodes_titles,
        play_groups,
        source: site.key,
        source_name: site.name,
        class: item.vod_class,
        year: item.vod_year,
        desc: item.vod_content.map(|c| clean_html_tags(&c)),
        type_name: item.type_name,
        douban_id: item
            .vod_douban_id
            .and_then(|v| v.as_i64())
            .map(|v| v as i32),
        source_site_type: site.site_type,
        login_hint: None,
        episodes_raw: Vec::new(),
    })
}

#[tauri::command]
pub async fn change_play_source(
    request: ChangePlaySourceRequest,
    storage: State<'_, StorageManager>,
    db: State<'_, crate::db::db_client::Db>,
    source_manager: State<'_, SourceIntelligenceManager>,
) -> Result<ChangePlaySourceResponse, String> {
    let ordered_sources =
        reorder_results_with_source_intelligence(request.available_sources, &source_manager);
    let requested_is_degraded = source_manager.should_skip_source(&request.new_source);
    let detail = if requested_is_degraded {
        ordered_sources
            .iter()
            .find(|source| source.source != request.new_source)
            .cloned()
            .or_else(|| {
                ordered_sources
                    .iter()
                    .find(|source| {
                        source.source == request.new_source && source.id == request.new_id
                    })
                    .cloned()
            })
    } else {
        ordered_sources
            .iter()
            .find(|source| source.source == request.new_source && source.id == request.new_id)
            .cloned()
    }
    .ok_or_else(|| "未找到匹配结果".to_string())?;

    // 切到 Spider 站点(type=3)时, 搜索结果不带集数, 须调详情接口补全
    let detail = if detail.episodes.is_empty() {
        match fetch_detail_for_source_key(&detail.source, &detail.id, &storage, &db).await {
            Ok(filled) => filled,
            Err(e) => {
                log::warn!("[播放] 切源补全 spider 集数失败 ({}): {}", detail.source, e);
                detail
            }
        }
    } else {
        detail
    };

    let old_key = match (
        request.current_source.as_deref(),
        request.current_id.as_deref(),
    ) {
        (Some(source), Some(id)) if !source.is_empty() && !id.is_empty() => {
            Some(format!("{}+{}", source, id))
        }
        _ => None,
    };
    let new_key = format!("{}+{}", detail.source, detail.id);

    migrate_play_source_state(
        &db,
        old_key.as_deref(),
        &new_key,
        request.skip_config.as_ref(),
    )?;

    let resolved = resolve_source_change(
        &detail,
        request.current_episode_index,
        request.current_play_time,
        request.resume_time,
    );
    probe_and_persist_source_health(
        &source_manager,
        &db,
        &detail,
        resolved.target_episode_index.max(0) as usize,
    )
    .await;

    Ok(ChangePlaySourceResponse {
        detail,
        target_episode_index: resolved.target_episode_index,
        resume_time: resolved.resume_time,
    })
}

#[tauri::command]
pub fn save_play_progress(
    request: SavePlayProgressRequest,
    app: tauri::AppHandle,
    db: State<'_, crate::db::db_client::Db>,
) -> Result<bool, String> {
    let saved = save_play_progress_inner(&db, request)?;
    if saved {
        let engine = app.state::<RecommendationEngine>();
        invalidate_recommendation_cache(&engine);
        let _ = app.emit("playRecordsUpdated", ());
    }
    Ok(saved)
}

#[tauri::command]
pub async fn player_tick(
    request: PlayerTickRequest,
    storage: State<'_, StorageManager>,
    cache: State<'_, SearchCacheManager>,
    db: State<'_, crate::db::db_client::Db>,
) -> Result<PlayerTickDecision, String> {
    let timing = decide_tick_timing(&request);

    let skip_action =
        if request.skip_enabled && request.total_duration > 0.0 && timing.should_check_skip {
            let detector = SkipDetection::new(request.intro_time, request.outro_time.abs());
            Some(detector.check_skip_action(request.current_time, request.total_duration))
        } else {
            None
        };

    let mut did_preload = false;
    if let (Some(source), Some(id), Some(current_episode), Some(total_episodes)) = (
        request.source.clone(),
        request.id.clone(),
        request.current_episode,
        request.total_episodes,
    ) {
        if !source.trim().is_empty() && !id.trim().is_empty() {
            did_preload = crate::commands::preload::preload_next_episode_if_needed(
                source,
                id,
                current_episode,
                total_episodes,
                request.current_time,
                request.total_duration,
                storage,
                cache,
                db,
            )
            .await?
            .did_preload;
        }
    }

    Ok(PlayerTickDecision {
        should_save_progress: timing.should_save_progress,
        next_last_save_at_ms: timing.next_last_save_at_ms,
        next_last_skip_check_at_ms: timing.next_last_skip_check_at_ms,
        skip_action,
        did_preload,
    })
}
/// 初始化播放器视图 - 聚合所有初始化数据
///
/// 一次性返回播放器启动所需的所有数据，减少 IPC 通信次数
///
/// # 参数
/// - `source`: 视频源标识
/// - `id`: 视频 ID
/// - `title`: 视频标题（用于搜索相似源）
///
/// # 返回
/// PlayerInitialState 包含：
/// - 视频详情
/// - 其他可用源
/// - 播放记录
/// - 收藏状态
/// - 跳过配置
/// - 播放器配置（去广告、优选开关）
#[tauri::command]
#[allow(unused_variables)]
pub async fn initialize_player_view(
    source: String,
    id: String,
    title: Option<String>,
    app_handle: tauri::AppHandle,
    storage: State<'_, StorageManager>,
    cache: State<'_, SearchCacheManager>,
    db: State<'_, crate::db::db_client::Db>,
    source_manager: State<'_, SourceIntelligenceManager>,
) -> Result<PlayerInitialState, String> {
    // 生成 storage key
    let key = format!("{}+{}", source, id);

    // 并行执行所有数据获取操作
    let (detail_result, play_record_meta, is_favorited, skip_config, player_config) = tokio::join!(
        // 1. 获取视频详情和其他源
        get_video_detail_optimized(
            source.clone(),
            id.clone(),
            storage.clone(),
            cache.clone(),
            db.clone(),
            Some(true)
        ),
        // 2. 读取播放记录
        async {
            db.with_conn(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT episode_index, play_time, title, year, total_episodes, search_title FROM play_records WHERE key = ?1",
                )?;

                let result = stmt.query_row(params![&key], |row| {
                    Ok(PlayRecordMeta {
                        episode_index: row.get(0)?,
                        play_time: row.get(1)?,
                        title: row.get(2)?,
                        year: row.get(3)?,
                        total_episodes: row.get(4)?,
                        search_title: row.get(5)?,
                    })
                });

                match result {
                    Ok(record) => Ok(Some(record)),
                    Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                    Err(e) => Err(e),
                }
            })
        },
        // 3. 检查收藏状态
        async {
            db.with_conn(|conn| {
                let mut stmt = conn.prepare("SELECT COUNT(*) FROM favorites WHERE key = ?1")?;

                let count: i32 = stmt.query_row(params![&key], |row| row.get(0))?;

                Ok(count > 0)
            })
        },
        // 4. 读取跳过配置
        async {
            db.with_conn(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT enable, intro_time, outro_time FROM skip_configs WHERE key = ?1",
                )?;

                let result = stmt.query_row(params![&key], |row| {
                    Ok(SkipConfigInfo {
                        enable: row.get::<_, i32>(0)? != 0,
                        intro_time: row.get::<_, f64>(1)? as i32,
                        outro_time: row.get::<_, f64>(2)? as i32,
                    })
                });

                match result {
                    Ok(config) => Ok(Some(config)),
                    Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                    Err(e) => Err(e),
                }
            })
        },
        // 5. 读取播放器配置
        async {
            let data = storage.get_data().map_err(|e| e.to_string())?;

            // 尝试从配置中获取播放器配置
            if let Some(player_config) = data.config.get("PlayerConfig") {
                if let Ok(config) = serde_json::from_value::<crate::commands::config::PlayerConfig>(
                    player_config.clone(),
                ) {
                    return Ok::<(bool, bool), String>((
                        config.block_ad_enabled,
                        config.optimization_enabled,
                    ));
                }
            }

            // 返回默认配置
            Ok::<(bool, bool), String>((true, true))
        }
    );

    // 处理视频详情结果
    let play_record_meta = play_record_meta?;
    let is_favorited = is_favorited?;
    let skip_config = skip_config?;
    let (block_ad_enabled, optimization_enabled) = player_config?;

    // 用户明确点了某个源(source+id), 不做静默换源兜底:
    // 详情失败/空选集时直接报错, 让前端提示"该源暂不可用", 而不是偷偷换成别的源
    let mut detail_response = match detail_result {
        Ok(detail) if !detail.detail.episodes.is_empty() => detail,
        Ok(_) => {
            return Err(format!(
                "该源暂无选集(站点可能被限流或资源下线): {}",
                source
            ));
        }
        Err(e) => {
            return Err(format!("获取该源详情失败: {}", e));
        }
    };

    detail_response.other_sources =
        reorder_results_with_source_intelligence(detail_response.other_sources, &source_manager);

    let probe_episode_index = play_record_meta
        .as_ref()
        .map(|record| {
            normalize_episode_index(record.episode_index, detail_response.detail.episodes.len())
        })
        .unwrap_or(0)
        .max(0) as usize;

    let probe_result = {
        let probe_url = detail_response
            .detail
            .episodes
            .get(probe_episode_index)
            .or_else(|| detail_response.detail.episodes.get(0))
            .cloned();

        if let Some(url) = probe_url {
            let client = get_video_client();
            Some(test_video_source(client, &url).await)
        } else {
            None
        }
    };

    if let Some(result) = probe_result {
        match result {
            Ok(result) => {
                let _ = source_manager.record_runtime_test_result_persisted(
                    &db,
                    detail_response.detail.source.clone(),
                    !result.has_error,
                    result.ping_time,
                    if result.has_error {
                        Some("playback probe failed".to_string())
                    } else {
                        None
                    },
                );
            }
            Err(_) => {
                // probe 失败仅记录源健康度, 不换源(用户点谁就播谁)
                let _ = source_manager.record_runtime_test_result_persisted(
                    &db,
                    detail_response.detail.source.clone(),
                    false,
                    0,
                    Some("playback probe failed".to_string()),
                );
            }
        }
    }

    let mut encountered_sources = Vec::with_capacity(1 + detail_response.other_sources.len());
    encountered_sources.push(detail_response.detail.clone());
    encountered_sources.extend(detail_response.other_sources.clone());
    persist_encountered_sources(&source_manager, &db, &encountered_sources);

    let play_record = play_record_meta.map(|record| PlayRecordInfo {
        episode_index: normalize_episode_index(
            record.episode_index,
            detail_response.detail.episodes.len(),
        ),
        play_time: record.play_time,
    });
    let (initial_episode_index, resume_time) = resolve_initial_playback_state(play_record.as_ref());

    Ok(PlayerInitialState {
        detail: detail_response.detail,
        other_sources: detail_response.other_sources,
        play_record,
        initial_episode_index,
        resume_time,
        is_favorited,
        skip_config,
        block_ad_enabled,
        optimization_enabled,
    })
}

/// 获取缓存统计信息
#[tauri::command]
pub fn get_cache_stats(
    video_cache: State<'_, VideoCacheManager>,
    search_cache: State<'_, SearchCacheManager>,
) -> Result<HashMap<String, CacheStats>, String> {
    let mut stats = HashMap::new();
    stats.insert("video".to_string(), video_cache.stats());
    stats.insert("search".to_string(), search_cache.stats());

    log::info!(
        "缓存统计 - 视频: {} 条目, 搜索: {} 条目",
        stats["video"].entry_count,
        stats["search"].entry_count
    );

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use rusqlite::Connection;

    fn make_result(
        title: &str,
        year: &str,
        episodes_len: usize,
        source: &str,
        id: &str,
    ) -> SearchResult {
        SearchResult {
            id: id.to_string(),
            title: title.to_string(),
            poster: String::new(),
            episodes: vec!["http://example.com/1.m3u8".to_string(); episodes_len],
            episodes_titles: Vec::new(),
            source: source.to_string(),
            source_name: "TestSource".to_string(),
            class: None,
            year: Some(year.to_string()),
            desc: None,
            type_name: None,
            douban_id: None,
            source_site_type: Some(1),
            login_hint: None,
            episodes_raw: Vec::new(),
            play_groups: Vec::new(),
        }
    }

    #[test]
    fn normalize_episode_index_clamps_and_converts() {
        assert_eq!(normalize_episode_index(1, 10), 0);
        assert_eq!(normalize_episode_index(0, 10), 0);
        assert_eq!(normalize_episode_index(-2, 10), 0);
        assert_eq!(normalize_episode_index(5, 3), 2);
        assert_eq!(normalize_episode_index(3, 3), 2);
        assert_eq!(normalize_episode_index(1, 0), 0);
    }

    #[test]
    fn resolve_initial_playback_state_prefers_record() {
        let record = PlayRecordInfo {
            episode_index: 4,
            play_time: 120,
        };
        let resolved = resolve_initial_playback_state(Some(&record));
        assert_eq!(resolved.0, 4);
        assert_eq!(resolved.1, Some(120));

        let resolved_none = resolve_initial_playback_state(None);
        assert_eq!(resolved_none.0, 0);
        assert_eq!(resolved_none.1, None);
    }

    #[test]
    fn filter_sources_for_fallback_matches_title_year_and_type() {
        let results = vec![
            make_result("Test Show", "2020", 12, "s1", "1"),
            make_result("Test Show", "2019", 12, "s2", "2"),
            make_result("Test Movie", "2020", 1, "s3", "3"),
            make_result("TestShow", "2020", 12, "s4", "4"),
        ];

        let filtered = filter_sources_for_fallback(
            &results,
            "Test Show",
            Some("2020"),
            Some(SearchTypeFilter::Tv),
        );

        assert_eq!(filtered.len(), 2);
        let sources: Vec<String> = filtered.iter().map(|item| item.source.clone()).collect();
        assert!(sources.contains(&"s1".to_string()));
        assert!(sources.contains(&"s4".to_string()));

        let filtered_no_year =
            filter_sources_for_fallback(&results, "Test Show", None, Some(SearchTypeFilter::Tv));
        assert_eq!(filtered_no_year.len(), 3);
    }

    #[test]
    fn parse_search_type_filter_maps_strings() {
        assert_eq!(
            parse_search_type_filter(Some("tv")),
            Some(SearchTypeFilter::Tv)
        );
        assert_eq!(
            parse_search_type_filter(Some("movie")),
            Some(SearchTypeFilter::Movie)
        );
        assert_eq!(parse_search_type_filter(Some("other")), None);
        assert_eq!(parse_search_type_filter(None), None);
    }

    #[test]
    fn reorder_results_with_best_places_best_first() {
        let best = make_result("Best", "2024", 1, "s1", "1");
        let other = make_result("Other", "2024", 1, "s2", "2");
        let results = vec![other.clone(), best.clone()];

        let ordered = reorder_results_with_best(&best, results);
        assert_eq!(ordered.len(), 2);
        assert_eq!(ordered[0].source, best.source);
        assert_eq!(ordered[0].id, best.id);
        assert_eq!(ordered[1].source, other.source);
    }

    #[test]
    fn parse_episode_groups_aligns_play_from_and_url() {
        // 百度网盘 组 + 夸克网盘 组, 各自独立选集
        let (default_eps, default_titles, groups) = parse_episode_groups(
            "第1集$http://a/1.m3u8#第2集$http://a/2.m3u8$$$第1集$http://b/1.m3u8#第2集$http://b/2.m3u8",
            Some("百度网盘$$$夸克网盘"),
            false,
        );
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].flag, "百度网盘");
        assert_eq!(groups[0].episodes.len(), 2);
        assert_eq!(groups[0].episodes_titles, vec!["第1集", "第2集"]);
        assert_eq!(groups[1].flag, "夸克网盘");
        assert_eq!(groups[1].episodes.len(), 2);
        // 默认选集取集数最多的一组(等长取第一组, 与旧行为一致)
        assert_eq!(default_eps, groups[0].episodes);
        assert_eq!(default_titles, groups[0].episodes_titles);
    }

    #[test]
    fn parse_episode_groups_picks_longest_when_counts_differ() {
        let (default_eps, _, groups) = parse_episode_groups(
            "第1集$http://a/1.m3u8$$$第1集$http://b/1.m3u8#第2集$http://b/2.m3u8#第3集$http://b/3.m3u8",
            None,
            false,
        );
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[1].episodes.len(), 3);
        assert_eq!(default_eps, groups[1].episodes);
        // 无 play_from 时生成占位名
        assert!(!groups[0].flag.is_empty());
    }

    #[test]
    fn parse_episode_groups_keeps_single_group_without_from() {
        let (default_eps, titles, groups) = parse_episode_groups(
            "http://a/1.m3u8#http://a/2.m3u8",
            None,
            false,
        );
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].episodes.len(), 2);
        assert_eq!(default_eps, groups[0].episodes);
        assert_eq!(titles, vec!["1", "2"]);
    }

    #[test]
    fn parse_episode_groups_spider_mode_keeps_raw_ids() {
        let (default_eps, _, groups) = parse_episode_groups(
            "第1集$netdisk://quark/1#第2集$netdisk://quark/2$$$第1集$netdisk://baidu/1#第2集$netdisk://baidu/2#第3集$netdisk://baidu/3",
            Some("夸克$$$百度"),
            true,
        );
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].flag, "夸克");
        assert_eq!(groups[0].episodes.len(), 2);
        assert_eq!(groups[0].episodes, vec!["netdisk://quark/1", "netdisk://quark/2"]);
        assert_eq!(groups[1].flag, "百度");
        assert_eq!(groups[1].episodes.len(), 3);
        // spider 原始 id 收录进 episodes_raw
        assert_eq!(groups[1].episodes_raw.len(), 3);
        assert_eq!(default_eps, groups[1].episodes);
    }

    fn setup_test_db() -> crate::db::db_client::Db {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        conn.execute_batch(
            r#"
            CREATE TABLE play_records (
                key TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                source_name TEXT NOT NULL,
                year TEXT,
                cover TEXT,
                episode_index INTEGER,
                total_episodes INTEGER,
                play_time INTEGER,
                total_time INTEGER,
                save_time INTEGER,
                search_title TEXT
            );
            CREATE TABLE skip_configs (
                key TEXT PRIMARY KEY,
                enable INTEGER DEFAULT 0,
                intro_time REAL DEFAULT 0,
                outro_time REAL DEFAULT 0
            );
            "#,
        )
        .expect("init schema");
        crate::db::db_client::Db::new(conn)
    }

    #[test]
    fn resolve_source_change_uses_current_play_time_when_no_resume() {
        let detail = make_result("Show", "2024", 3, "s1", "1");
        let resolved = resolve_source_change(&detail, 1, 12.5, None);
        assert_eq!(resolved.target_episode_index, 1);
        assert!((resolved.resume_time - 12.5).abs() < 0.01);
    }

    #[test]
    fn resolve_source_change_keeps_existing_resume_time() {
        let detail = make_result("Show", "2024", 3, "s1", "1");
        let resolved = resolve_source_change(&detail, 0, 20.0, Some(35.0));
        assert_eq!(resolved.target_episode_index, 0);
        assert!((resolved.resume_time - 35.0).abs() < 0.01);
    }

    #[test]
    fn resolve_source_change_clears_when_index_out_of_range() {
        let detail = make_result("Show", "2024", 2, "s1", "1");
        let resolved = resolve_source_change(&detail, 5, 30.0, Some(15.0));
        assert_eq!(resolved.target_episode_index, 0);
        assert_eq!(resolved.resume_time, 0.0);
    }

    #[test]
    fn migrate_play_source_state_moves_skip_config_and_clears_old_record() {
        let db = setup_test_db();
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO play_records (key, title, source_name, year, cover, episode_index, total_episodes, play_time, total_time, save_time, search_title)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    "old+1",
                    "Old Title",
                    "Old Source",
                    "2024",
                    "cover",
                    1,
                    10,
                    12,
                    100,
                    123,
                    "search",
                ],
            )?;
            conn.execute(
                "INSERT INTO skip_configs (key, enable, intro_time, outro_time) VALUES (?1, ?2, ?3, ?4)",
                params!["old+1", 1, 10.0, -20.0],
            )?;
            Ok(())
        })
        .unwrap();

        let skip = SkipConfigPayload {
            enable: true,
            intro_time: 10.0,
            outro_time: -20.0,
        };
        migrate_play_source_state(&db, Some("old+1"), "new+2", Some(&skip)).unwrap();

        let old_count: i32 = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM play_records WHERE key = ?1",
                    params!["old+1"],
                    |row| row.get(0),
                )
            })
            .unwrap();
        assert_eq!(old_count, 0);

        let old_skip_count: i32 = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM skip_configs WHERE key = ?1",
                    params!["old+1"],
                    |row| row.get(0),
                )
            })
            .unwrap();
        assert_eq!(old_skip_count, 0);

        let (enable, intro, outro): (i32, f64, f64) = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT enable, intro_time, outro_time FROM skip_configs WHERE key = ?1",
                    params!["new+2"],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
            })
            .unwrap();
        assert_eq!(enable, 1);
        assert!((intro - 10.0).abs() < 0.01);
        assert!((outro + 20.0).abs() < 0.01);
    }

    #[test]
    fn save_play_progress_inserts_one_based_episode_index() {
        let db = setup_test_db();
        let request = SavePlayProgressRequest {
            source: "s1".to_string(),
            id: "1".to_string(),
            title: "Title".to_string(),
            source_name: "Source".to_string(),
            year: "2024".to_string(),
            cover: "cover".to_string(),
            episode_index: 0,
            total_episodes: 10,
            play_time: 12.7,
            total_time: 120.2,
            search_title: Some("search".to_string()),
        };

        let saved = save_play_progress_inner(&db, request).unwrap();
        assert!(saved);

        let stored_index: i32 = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT episode_index FROM play_records WHERE key = ?1",
                    params!["s1+1"],
                    |row| row.get(0),
                )
            })
            .unwrap();
        assert_eq!(stored_index, 1);
    }

    #[test]
    fn save_play_progress_skips_when_too_short() {
        let db = setup_test_db();
        let request = SavePlayProgressRequest {
            source: "s1".to_string(),
            id: "1".to_string(),
            title: "Title".to_string(),
            source_name: "Source".to_string(),
            year: "2024".to_string(),
            cover: "cover".to_string(),
            episode_index: 0,
            total_episodes: 10,
            play_time: 0.5,
            total_time: 120.0,
            search_title: None,
        };

        let saved = save_play_progress_inner(&db, request).unwrap();
        assert!(!saved);

        let count: i32 = db
            .with_conn(|conn| {
                conn.query_row("SELECT COUNT(*) FROM play_records", [], |row| row.get(0))
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn resolve_enabled_source_returns_only_enabled_match() {
        let config = serde_json::json!({
            "SourceConfig": [
                { "key": "a", "api": "https://a.example.com", "name": "A", "disabled": true },
                { "key": "b", "api": "https://b.example.com", "name": "B", "disabled": false }
            ]
        });

        let source = resolve_enabled_source(&config, "b").expect("source b");
        assert_eq!(source.key, "b");
        assert_eq!(source.api, "https://b.example.com");
        assert!(resolve_enabled_source(&config, "a").is_none());
        assert!(resolve_enabled_source(&config, "missing").is_none());
    }

    #[test]
    fn validate_remote_url_rejects_non_http_schemes() {
        let result = validate_remote_url("file:///tmp/test.m3u8", false);
        assert!(result.is_err());
    }

    #[test]
    fn validate_remote_url_rejects_lan_hosts_when_disabled() {
        let result = validate_remote_url("http://192.168.1.20/video.m3u8", false);
        assert!(result.is_err());
    }

    #[test]
    fn validate_remote_url_allows_lan_hosts_when_enabled() {
        let result = validate_remote_url("http://192.168.1.20/video.m3u8", true);
        assert!(result.is_ok());
    }

    #[test]
    fn resolve_enabled_source_filters_lan_source_when_disallowed() {
        let config = serde_json::json!({
            "PlayerConfig": {
                "allow_lan_sources": false
            },
            "SourceConfig": [
                { "key": "lan", "api": "http://192.168.1.10/api.php/provide/vod", "name": "LAN", "disabled": false },
                { "key": "public", "api": "https://vod.example.com/api.php/provide/vod", "name": "Public", "disabled": false }
            ]
        });

        assert!(resolve_enabled_source(&config, "lan").is_none());
        assert!(resolve_enabled_source(&config, "public").is_some());
    }

    #[test]
    fn parse_source_categories_handles_number_and_string_type_id() {
        let body = r#"{
            "class": [
                { "type_id": 1, "type_name": "电影" },
                { "type_id": "2", "type_name": "电视剧", "type_pid": 0 }
            ]
        }"#;

        let categories = parse_source_categories(body).expect("parse categories");
        assert_eq!(categories.len(), 2);
        assert_eq!(categories[0].type_name, "电影");
        assert_eq!(categories[1].type_name, "电视剧");
    }

    #[test]
    fn decide_tick_timing_updates_timestamps_when_threshold_met() {
        let request = PlayerTickRequest {
            current_time: 10.0,
            total_duration: 100.0,
            now_ms: 20_000,
            last_save_at_ms: 14_000,
            save_interval_ms: 5_000,
            last_skip_check_at_ms: 18_000,
            skip_enabled: true,
            intro_time: 60.0,
            outro_time: 60.0,
            source: Some("s1".to_string()),
            id: Some("id1".to_string()),
            current_episode: Some(0),
            total_episodes: Some(10),
        };

        let decision = decide_tick_timing(&request);
        assert!(decision.should_save_progress);
        assert!(decision.should_check_skip);
        assert_eq!(decision.next_last_save_at_ms, 20_000);
        assert_eq!(decision.next_last_skip_check_at_ms, 20_000);
    }

    #[test]
    fn decide_tick_timing_keeps_timestamps_when_not_due() {
        let request = PlayerTickRequest {
            current_time: 10.0,
            total_duration: 100.0,
            now_ms: 20_000,
            last_save_at_ms: 19_000,
            save_interval_ms: 5_000,
            last_skip_check_at_ms: 19_200,
            skip_enabled: true,
            intro_time: 60.0,
            outro_time: 60.0,
            source: None,
            id: None,
            current_episode: None,
            total_episodes: None,
        };

        let decision = decide_tick_timing(&request);
        assert!(!decision.should_save_progress);
        assert!(!decision.should_check_skip);
        assert_eq!(decision.next_last_save_at_ms, 19_000);
        assert_eq!(decision.next_last_skip_check_at_ms, 19_200);
    }

    #[test]
    fn cache_stats_creation() {
        let stats = CacheStats {
            entry_count: 100,
            weighted_size: 50000,
        };
        assert_eq!(stats.entry_count, 100);
        assert_eq!(stats.weighted_size, 50000);
    }

    #[test]
    fn cache_stats_serialization() {
        let stats = CacheStats {
            entry_count: 50,
            weighted_size: 25000,
        };
        let json = serde_json::to_string(&stats).unwrap();
        assert!(json.contains("\"entry_count\":50"));
        assert!(json.contains("\"weighted_size\":25000"));

        let deserialized: CacheStats = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.entry_count, 50);
        assert_eq!(deserialized.weighted_size, 25000);
    }

    #[test]
    fn player_initial_state_serialization() {
        let detail = make_result("Test", "2024", 12, "source1", "id1");
        let state = PlayerInitialState {
            detail: detail.clone(),
            other_sources: vec![],
            play_record: None,
            initial_episode_index: 0,
            resume_time: None,
            is_favorited: false,
            skip_config: None,
            block_ad_enabled: true,
            optimization_enabled: true,
        };

        let json = serde_json::to_string(&state).unwrap();
        assert!(json.contains("\"initial_episode_index\":0"));
        assert!(json.contains("\"block_ad_enabled\":true"));
    }

    #[test]
    fn play_record_info_with_values() {
        let info = PlayRecordInfo {
            episode_index: 5,
            play_time: 123,
        };
        assert_eq!(info.episode_index, 5);
        assert_eq!(info.play_time, 123);
    }

    #[test]
    fn skip_config_info_serialization() {
        let config = SkipConfigInfo {
            enable: true,
            intro_time: 10,
            outro_time: -5,
        };

        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("\"enable\":true"));
        assert!(json.contains("\"intro_time\":10"));
        assert!(json.contains("\"outro_time\":-5"));
    }

    #[test]
    fn change_play_source_request_creation() {
        let request = ChangePlaySourceRequest {
            current_source: Some("s1".to_string()),
            current_id: Some("id1".to_string()),
            new_source: "s2".to_string(),
            new_id: "id2".to_string(),
            available_sources: vec![],
            current_episode_index: 3,
            current_play_time: 45.5,
            resume_time: Some(50.0),
            skip_config: None,
        };

        assert_eq!(request.current_episode_index, 3);
        assert!((request.current_play_time - 45.5).abs() < 0.01);
        assert_eq!(request.new_source, "s2");
    }

    #[test]
    fn normalize_episode_index_boundary_cases() {
        // Test with 0 maximum episodes
        assert_eq!(normalize_episode_index(1, 0), 0);
        assert_eq!(normalize_episode_index(0, 0), 0);
        assert_eq!(normalize_episode_index(-1, 0), 0);

        // Test with large values
        assert_eq!(normalize_episode_index(1000, 100), 99);
        assert_eq!(normalize_episode_index(99, 100), 98);

        // Test negative values
        assert_eq!(normalize_episode_index(-10, 50), 0);
        assert_eq!(normalize_episode_index(-1, 50), 0);
    }

    #[test]
    fn filter_sources_for_fallback_title_matching() {
        let results = vec![
            make_result("Exact Title", "2020", 12, "s1", "1"),
            make_result("exact title", "2020", 12, "s2", "2"),
            make_result("Exact", "2020", 12, "s3", "3"),
            make_result("Exact Title Extra", "2020", 12, "s4", "4"),
        ];

        let filtered = filter_sources_for_fallback(&results, "Exact Title", Some("2020"), None);

        assert!(filtered.len() > 0);
        let sources: Vec<String> = filtered.iter().map(|r| r.source.clone()).collect();
        assert!(sources.contains(&"s1".to_string()));
        assert!(sources.contains(&"s2".to_string()));
    }

    #[test]
    fn player_tick_request_creation_with_all_fields() {
        let request = PlayerTickRequest {
            current_time: 50.5,
            total_duration: 100.0,
            now_ms: 100_000,
            last_save_at_ms: 90_000,
            save_interval_ms: 5_000,
            last_skip_check_at_ms: 95_000,
            skip_enabled: true,
            intro_time: 10.0,
            outro_time: -5.0,
            source: Some("s1".to_string()),
            id: Some("id1".to_string()),
            current_episode: Some(2),
            total_episodes: Some(12),
        };

        assert_eq!(request.current_episode, Some(2));
        assert!((request.current_time - 50.5).abs() < 0.01);
        assert_eq!(request.save_interval_ms, 5_000);
    }

    #[test]
    fn resolve_source_change_with_clamping() {
        let detail = make_result("Show", "2024", 3, "s1", "1");

        let resolved = resolve_source_change(&detail, 100, 10.0, None);
        assert_eq!(resolved.target_episode_index, 0);
        assert_eq!(resolved.resume_time, 0.0);

        let resolved = resolve_source_change(&detail, -5, 10.0, None);
        assert_eq!(resolved.target_episode_index, 0);
    }

    #[test]
    fn parse_search_type_filter_valid_cases() {
        // Valid cases that work
        assert_eq!(
            parse_search_type_filter(Some("tv")),
            Some(SearchTypeFilter::Tv)
        );
        assert_eq!(
            parse_search_type_filter(Some("movie")),
            Some(SearchTypeFilter::Movie)
        );
        // None case
        assert_eq!(parse_search_type_filter(None), None);
    }
}

// ---------- 首页目录（Home Catalog） ----------

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HomeCatalogResponse {
    pub source_key: String,
    pub source_name: String,
    pub site_type: i32,
    pub categories: Vec<HomeCategoryRow>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HomeCategoryRow {
    pub type_id: String,
    pub type_name: String,
    pub list: Vec<HomeVideoCard>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HomeVideoCard {
    pub id: String,
    pub title: String,
    pub poster: String,
    pub year: Option<String>,
    pub episodes: Vec<String>,
    pub class: Option<String>,
    pub source: String,
    pub source_name: String,
}

const HOME_CATALOG_MAX_VIDEOS_PER_CATEGORY: usize = 8;
const HOME_CATALOG_TTL_SECS: u64 = 300;

struct CachedHomeCatalog {
    response: HomeCatalogResponse,
    fetched_at: std::time::Instant,
}

static HOME_CATALOG_CACHE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, CachedHomeCatalog>>,
> = std::sync::OnceLock::new();

fn home_catalog_cache()
-> &'static std::sync::Mutex<std::collections::HashMap<String, CachedHomeCatalog>> {
    HOME_CATALOG_CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn stringify_category_type_id(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn api_search_item_to_home_card(item: ApiSearchItem, source: &ApiSite) -> HomeVideoCard {
    let (episodes, _) = parse_episodes_for(item.vod_play_url.as_deref().unwrap_or(""), false);
    HomeVideoCard {
        id: match item.vod_id {
            serde_json::Value::String(s) => s,
            serde_json::Value::Number(n) => n.to_string(),
            _ => String::new(),
        },
        title: item.vod_name.trim().to_string(),
        poster: item.vod_pic,
        year: item.vod_year,
        episodes,
        class: item.vod_class,
        source: source.key.clone(),
        source_name: source.name.clone(),
    }
}

#[tauri::command]
pub async fn get_home_catalog(
    source_key: String,
    storage: State<'_, StorageManager>,
    db: State<'_, crate::db::db_client::Db>,
) -> Result<HomeCatalogResponse, String> {
    let config = get_config_with_db_sources(&storage, &db)?;
    let source = resolve_enabled_source(&config, &source_key)
        .ok_or_else(|| format!("Source not found or disabled: {}", source_key))?;

    let site_type = source.site_type.unwrap_or(1);

    // Spider 站点无目录能力，直接返回空
    if site_type == 3 {
        return Ok(HomeCatalogResponse {
            source_key: source.key,
            source_name: source.name,
            site_type: 3,
            categories: Vec::new(),
        });
    }

    // TTL 缓存命中
    {
        let map = home_catalog_cache().lock().unwrap();
        if let Some(cached) = map.get(&source_key) {
            if cached.fetched_at.elapsed().as_secs() < HOME_CATALOG_TTL_SECS {
                return Ok(cached.response.clone());
            }
        }
    }

    // 1) 拉取分类列表（单请求超时 6s，失败降级为空）
    let class_url = source_url(&source.api, "?ac=class");
    let class_body = match timeout(Duration::from_secs(6), async {
        let resp = get_video_client()
            .get(&class_url)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let resp = resp.error_for_status().map_err(|e| e.to_string())?;
        resp.text().await.map_err(|e| e.to_string())
    })
    .await
    {
        Ok(Ok(text)) => text,
        _ => String::new(),
    };

    let categories = parse_source_categories(&class_body).unwrap_or_default();

    // 2) 并行拉每个分类的视频（pg=1，截前 N 条）
    let client = get_video_client();
    let mut handles = Vec::with_capacity(categories.len());
    for (idx, cat) in categories.iter().enumerate() {
        let type_id = stringify_category_type_id(&cat.type_id);
        if type_id.is_empty() {
            continue;
        }
        let api = source.api.clone();
        let client = client.clone();
        let source_clone = source.clone();
        handles.push((idx, tokio::spawn(async move {
            let url = source_url(
                &api,
                &format!("?ac=videolist&t={}&pg=1", urlencoding::encode(&type_id)),
            );
            let resp = match timeout(Duration::from_secs(6), client.get(&url).send()).await {
                Ok(Ok(r)) => r,
                _ => return Vec::new(),
            };
            let resp = match resp.error_for_status() {
                Ok(r) => r,
                Err(_) => return Vec::new(),
            };
            let body = match timeout(Duration::from_secs(6), resp.text()).await {
                Ok(Ok(t)) => t,
                _ => return Vec::new(),
            };
            let items = parse_source_videos(&body).unwrap_or_default();
            let mut cards: Vec<HomeVideoCard> = items
                .into_iter()
                .take(HOME_CATALOG_MAX_VIDEOS_PER_CATEGORY)
                .map(|item| api_search_item_to_home_card(item, &source_clone))
                .collect();
            cards.retain(|c| !c.id.is_empty());
            cards
        })));
    }

    let mut indexed: Vec<(usize, Vec<HomeVideoCard>)> = Vec::new();
    for (idx, handle) in handles {
        match handle.await {
            Ok(cards) => indexed.push((idx, cards)),
            Err(_) => indexed.push((idx, Vec::new())),
        }
    }
    indexed.sort_by_key(|(i, _)| *i);

    let mut category_rows = Vec::with_capacity(categories.len());
    for (idx, cat) in categories.iter().enumerate() {
        let list = indexed
            .iter()
            .find(|(i, _)| *i == idx)
            .map(|(_, c)| c.clone())
            .unwrap_or_default();
        category_rows.push(HomeCategoryRow {
            type_id: stringify_category_type_id(&cat.type_id),
            type_name: cat.type_name.clone(),
            list,
        });
    }

    let response = HomeCatalogResponse {
        source_key: source.key,
        source_name: source.name,
        site_type,
        categories: category_rows,
    };

    // 写缓存
    {
        let mut map = home_catalog_cache().lock().unwrap();
        map.insert(
            source_key,
            CachedHomeCatalog {
                response: response.clone(),
                fetched_at: std::time::Instant::now(),
            },
        );
    }

    Ok(response)
}

#[cfg(test)]
mod home_catalog_tests {
    use super::*;

    #[test]
    fn stringify_category_type_id_handles_number_and_string() {
        assert_eq!(stringify_category_type_id(&serde_json::json!(1)), "1");
        assert_eq!(
            stringify_category_type_id(&serde_json::json!("2")),
            "2"
        );
        assert_eq!(stringify_category_type_id(&serde_json::json!(null)), "");
    }

    #[test]
    fn api_search_item_to_home_card_maps_fields() {
        let source = ApiSite {
            key: "src1".to_string(),
            api: "http://x".to_string(),
            name: "SourceOne".to_string(),
            detail: None,
            is_adult: None,
            site_type: Some(1),
            spider: None,
            searchable: Some(1),
        };
        let item = ApiSearchItem {
            vod_id: serde_json::json!(42),
            vod_name: "  Test Movie  ".to_string(),
            vod_pic: "http://pic".to_string(),
            vod_remarks: None,
            vod_play_url: Some("http://a.m3u8#http://b.m3u8".to_string()),
            vod_play_from: None,
            vod_class: Some("电影".to_string()),
            vod_year: Some("2020".to_string()),
            vod_content: None,
            vod_douban_id: None,
            type_name: None,
        };
        let card = api_search_item_to_home_card(item, &source);
        assert_eq!(card.id, "42");
        assert_eq!(card.title, "Test Movie");
        assert_eq!(card.source, "src1");
        assert_eq!(card.source_name, "SourceOne");
        assert_eq!(card.year.as_deref(), Some("2020"));
        assert_eq!(card.class.as_deref(), Some("电影"));
        assert_eq!(card.episodes.len(), 2);
    }

    // ---- Bridge/Spider 站点级搜索缓存 (方案 §8) ----

    fn ok_fetch(
        c: Arc<std::sync::atomic::AtomicU32>,
    ) -> impl FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<SearchResult>, String>> + Send>>
           + Send
           + 'static {
        move || {
            let c = c.clone();
            Box::pin(async move {
                c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok::<Vec<SearchResult>, String>(vec![])
            })
        }
    }

    fn err_fetch(
        c: Arc<std::sync::atomic::AtomicU32>,
    ) -> impl FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<SearchResult>, String>> + Send>>
           + Send
           + 'static {
        move || {
            let c = c.clone();
            Box::pin(async move {
                c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Err::<Vec<SearchResult>, String>("boom".into())
            })
        }
    }

    #[tokio::test]
    async fn bridge_search_cache_hit_avoids_refetch() {
        // 方案 §8/§38: 重复搜索必须命中缓存, 不再访问 Bridge
        let cache = BridgeSearchCache::new();
        let counter = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let r1 = cache
            .get_or_insert_with("k1:斗破".into(), ok_fetch(counter.clone()))
            .await;
        let r2 = cache
            .get_or_insert_with("k1:斗破".into(), ok_fetch(counter.clone()))
            .await;
        assert!(r1.is_ok() && r2.is_ok());
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 1);
        // 不同关键词各自独立缓存
        let _ = cache
            .get_or_insert_with("k1:斗破苍穹".into(), ok_fetch(counter.clone()))
            .await;
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn bridge_search_cache_error_not_cached() {
        // 失败不缓存: 下次可重试
        let cache = BridgeSearchCache::new();
        let counter = Arc::new(std::sync::atomic::AtomicU32::new(0));
        assert!(cache
            .get_or_insert_with("k2:x".into(), err_fetch(counter.clone()))
            .await
            .is_err());
        assert!(cache
            .get_or_insert_with("k2:x".into(), err_fetch(counter.clone()))
            .await
            .is_err());
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    // ---- 详情缓存 (方案 §9) ----

    #[tokio::test]
    async fn detail_cache_hit_avoids_refetch() {
        // 方案 §9: 详情 TTL 5~10 分钟; key = source_id + vod_id
        let counter = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let key = format!("detail:ut:{}:v1", std::process::id());
        let first = cached_detail_with(key.clone(), {
            let c = counter.clone();
            let k = key.clone();
            move || {
                let c = c.clone();
                let k = k.clone();
                async move {
                    c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok::<ApiSearchItem, String>(ApiSearchItem {
                        vod_id: Value::String(k),
                        vod_name: "测试片".into(),
                        vod_pic: String::new(),
                        vod_remarks: None,
                        vod_play_url: Some("第1集$http://x/1.mp4".into()),
                        vod_play_from: None,
                        vod_class: None,
                        vod_year: None,
                        vod_content: None,
                        vod_douban_id: None,
                        type_name: None,
                    })
                }
            }
        })
        .await
        .unwrap();
        let second = cached_detail_with(key, {
            let c = counter.clone();
            move || {
                let c = c.clone();
                async move {
                    c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok::<ApiSearchItem, String>(ApiSearchItem {
                        vod_id: Value::String("unreachable".into()),
                        vod_name: "不应出现".into(),
                        vod_pic: String::new(),
                        vod_remarks: None,
                        vod_play_url: None,
                        vod_play_from: None,
                        vod_class: None,
                        vod_year: None,
                        vod_content: None,
                        vod_douban_id: None,
                        type_name: None,
                    })
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(first.vod_name, "测试片");
        assert_eq!(second.vod_name, "测试片"); // 命中缓存, 未执行第二次 fetch
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
