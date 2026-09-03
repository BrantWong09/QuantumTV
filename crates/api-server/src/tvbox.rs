use axum::extract::{Json, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use base64::Engine;
use quantumtv_core::is_adult_source;
use quantumtv_core::playback::filter_ads_from_m3_u8;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, SystemTime};

use crate::{AppState, SERVER_IP};
use quantumtv_api::config_file;
use quantumtv_api::config_url::{
    determine_site_type, fetch_subscription, Parse, Site, SubscriptionConfig,
};
static PARSES_URL: LazyLock<String> = LazyLock::new(|| {
    std::env::var("PARSES_URL").unwrap_or_else(|_| "http://127.0.0.1".to_string())
});

// ================== 数据结构 ==================

#[derive(Clone, Debug)]
pub struct SpiderInfo {
    #[allow(dead_code)]
    pub buffer: Option<Vec<u8>>,
    pub md5: String,
    pub source: String,
    pub success: bool,
    #[allow(dead_code)]
    pub cached: bool,
    pub timestamp: SystemTime,
    #[allow(dead_code)]
    pub size: usize,
    #[allow(dead_code)]
    pub tried: usize,
}

#[derive(Clone, Debug)]
pub struct FailedSources {
    pub sources: HashSet<String>,
    pub last_reset: SystemTime,
}

#[derive(Deserialize)]
pub struct ConfigParams {
    adult: Option<bool>,
    mode: Option<String>,
    spider: Option<String>,
    #[serde(rename = "forceSpiderRefresh")]
    force_spider_refresh: Option<String>,
    #[serde(rename = "subscriptionUrl")]
    subscription_url: Option<String>,
}

#[derive(Clone, Debug)]
pub struct CachedSubscription {
    pub config: SubscriptionConfig,
    pub cached_at: SystemTime,
}

/// Spider JAR 磁盘缓存元数据
#[derive(Serialize, Deserialize, Clone, Debug)]
struct SpiderMetadata {
    md5: String,
    source: String,
    success: bool,
    timestamp: u64, // Unix timestamp in seconds
    size: usize,
}

// ================== Spider Jar 候选源配置 ==================

const DOMESTIC_CANDIDATES: &[&str] = &[
    "https://agit.ai/Yoursmile7/TVBox/raw/branch/master/jar/custom_spider.jar",
    "https://ghproxy.net/https://raw.githubusercontent.com/FongMi/CatVodSpider/main/jar/custom_spider.jar",
    "https://mirror.ghproxy.com/https://raw.githubusercontent.com/FongMi/CatVodSpider/main/jar/custom_spider.jar",
];

const INTERNATIONAL_CANDIDATES: &[&str] = &[
    "https://raw.githubusercontent.com/FongMi/CatVodSpider/main/jar/custom_spider.jar",
    "https://raw.gitmirror.com/FongMi/CatVodSpider/main/jar/custom_spider.jar",
    "https://ghproxy.cc/https://raw.githubusercontent.com/FongMi/CatVodSpider/main/jar/custom_spider.jar",
];

const PROXY_CANDIDATES: &[&str] = &[
    "https://gh-proxy.com/https://raw.githubusercontent.com/FongMi/CatVodSpider/main/jar/custom_spider.jar",
    "https://ghps.cc/https://raw.githubusercontent.com/FongMi/CatVodSpider/main/jar/custom_spider.jar",
    "https://gh.api.99988866.xyz/https://raw.githubusercontent.com/FongMi/CatVodSpider/main/jar/custom_spider.jar",
];

// Fallback JAR (base64 encoded minimal working spider.jar)
const FALLBACK_JAR_BASE64: &str = "UEsDBBQACAgIACVFfFcAAAAAAAAAAAAAAAAJAAAATUVUQS1JTkYvUEsHCAAAAAACAAAAAAAAACVFfFcAAAAAAAAAAAAAAAANAAAATUVUQS1JTkYvTUFOSUZFU1QuTUZNYW5pZmVzdC1WZXJzaW9uOiAxLjAKQ3JlYXRlZC1CeTogMS44LjBfNDIxIChPcmFjbGUgQ29ycG9yYXRpb24pCgpQSwcIj79DCUoAAABLAAAAUEsDBBQACAgIACVFfFcAAAAAAAAAAAAAAAAMAAAATWVkaWFVdGlscy5jbGFzczWRSwrCQBBER3trbdPxm4BuBHfiBxHFH4hCwJX4ATfFCrAxnWnYgZCTuPIIHkCPYE+lM5NoILPpoqvrVVd1JslCaLB3MpILJ5xRz5gbMeMS+oyeBOc4xSWucYsZN3CHe7zgiQue8YJXvOEdH/jEFz7whW984weZ+Ecm/pGJf2TiH5n4Ryb+kYl/ZOIfmfhHJv6RiX9k4h+Z+Ecm/pGJf2TiH5n4Ryb+kYl/ZOIfGQaaaXzgE1/4xje+8Y1vfOMb3/jGN77xjW98q9c0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdM0TdOI06nO7p48NRQjICAgICAgICAgICAgICAoKCgoKCgoKCgoKCgoKChoqKioqKioqKio;";

const SUCCESS_TTL: u64 = 24 * 60 * 60 * 5; // 5 天
const FAILURE_TTL: u64 = 10 * 60; // 10 分钟
const FAILURE_RESET_INTERVAL: u64 = 2 * 60 * 60; // 2 小时

// 磁盘缓存路径
const CACHE_DIR: &str = ".cache";
const SPIDER_JAR_FILE: &str = "spider.jar";
const SPIDER_META_FILE: &str = "spider.json";

// ================== 辅助函数 ==================

/// 从磁盘加载 Spider JAR
fn load_spider_from_disk() -> Option<SpiderInfo> {
    let cache_dir = PathBuf::from(CACHE_DIR);
    let jar_path = cache_dir.join(SPIDER_JAR_FILE);
    let meta_path = cache_dir.join(SPIDER_META_FILE);

    // 检查文件是否存在
    if !jar_path.exists() || !meta_path.exists() {
        return None;
    }

    // 读取元数据
    let meta_content = match std::fs::read_to_string(&meta_path) {
        Ok(content) => content,
        Err(e) => {
            tracing::warn!("Failed to read spider metadata: {}", e);
            return None;
        }
    };

    let metadata: SpiderMetadata = match serde_json::from_str(&meta_content) {
        Ok(meta) => meta,
        Err(e) => {
            tracing::warn!("Failed to parse spider metadata: {}", e);
            return None;
        }
    };

    // 检查缓存是否过期（5天）
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    if now - metadata.timestamp > SUCCESS_TTL {
        tracing::info!("Disk cache expired (age: {}s)", now - metadata.timestamp);
        return None;
    }

    // 读取 JAR 文件
    let buffer = match std::fs::read(&jar_path) {
        Ok(data) => data,
        Err(e) => {
            tracing::warn!("Failed to read spider JAR: {}", e);
            return None;
        }
    };

    // 验证 MD5
    let actual_md5 = calculate_md5(&buffer);
    if actual_md5 != metadata.md5 {
        tracing::warn!("Spider JAR MD5 mismatch, cache corrupted");
        return None;
    }

    tracing::info!(
        "磁盘加载了 spider jar: {} bytes, md5: {}, age: {}s",
        buffer.len(),
        metadata.md5,
        now - metadata.timestamp
    );

    Some(SpiderInfo {
        buffer: Some(buffer),
        md5: metadata.md5,
        source: metadata.source,
        success: metadata.success,
        cached: true,
        timestamp: SystemTime::UNIX_EPOCH + Duration::from_secs(metadata.timestamp),
        size: metadata.size,
        tried: 0,
    })
}

/// 保存 Spider JAR 到磁盘
fn save_spider_to_disk(info: &SpiderInfo) -> Result<(), String> {
    let cache_dir = PathBuf::from(CACHE_DIR);

    // 创建缓存目录
    if let Err(e) = std::fs::create_dir_all(&cache_dir) {
        return Err(format!("Failed to create cache directory: {}", e));
    }

    let jar_path = cache_dir.join(SPIDER_JAR_FILE);
    let meta_path = cache_dir.join(SPIDER_META_FILE);

    // 保存 JAR 文件
    if let Some(buffer) = &info.buffer {
        if let Err(e) = std::fs::write(&jar_path, buffer) {
            return Err(format!("Failed to write spider JAR: {}", e));
        }
    } else {
        return Err("No buffer to save".to_string());
    }

    // 保存元数据
    let timestamp = info
        .timestamp
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let metadata = SpiderMetadata {
        md5: info.md5.clone(),
        source: info.source.clone(),
        success: info.success,
        timestamp,
        size: info.size,
    };

    let meta_content = serde_json::to_string_pretty(&metadata)
        .map_err(|e| format!("Failed to serialize metadata: {}", e))?;

    std::fs::write(&meta_path, meta_content)
        .map_err(|e| format!("Failed to write metadata: {}", e))?;

    tracing::info!(
        "Saved spider JAR to disk: {} bytes, md5: {}",
        info.size,
        info.md5
    );

    Ok(())
}

fn is_private_host(url: &str) -> bool {
    if url.starts_with("http") {
        let lower = url.to_lowercase();
        lower.contains("localhost") || lower.contains("127.") || lower.contains("10.")
    } else {
        true
    }
}

fn header_value(headers: &HeaderMap, key: &str) -> Option<String> {
    headers
        .get(key)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.split(',').next().unwrap_or(value).trim().to_string())
}

fn resolve_base_url(headers: &HeaderMap) -> String {
    let proto = header_value(headers, "x-forwarded-proto").unwrap_or_else(|| "http".to_string());
    let host = header_value(headers, "x-forwarded-host")
        .or_else(|| header_value(headers, "host"))
        .unwrap_or_else(|| SERVER_IP.as_str().to_string());

    if host.starts_with("http://") || host.starts_with("https://") {
        host
    } else {
        format!("{}://{}", proto, host)
    }
}

fn get_candidates() -> Vec<String> {
    // 简化版：返回所有候选源（国内优先，然后国际，最后代理）
    let mut candidates = Vec::new();
    candidates.extend(DOMESTIC_CANDIDATES.iter().map(|s| s.to_string()));
    candidates.extend(INTERNATIONAL_CANDIDATES.iter().map(|s| s.to_string()));
    candidates.extend(PROXY_CANDIDATES.iter().map(|s| s.to_string()));
    candidates
}

fn calculate_md5(data: &[u8]) -> String {
    format!("{:x}", md5::compute(data))
}

async fn fetch_remote(url: &str, timeout_ms: u64, retry_count: usize) -> Option<Vec<u8>> {
    for attempt in 0..=retry_count {
        match fetch_remote_once(url, timeout_ms).await {
            Ok(data) => {
                // 验证 JAR 文件格式（检查 ZIP 头）
                if data.len() < 1000 {
                    tracing::warn!("File too small: {} bytes from {}", data.len(), url);
                    continue;
                }

                if data[0] != 0x50 || data[1] != 0x4B {
                    tracing::warn!("Invalid JAR file format from {}", url);
                    continue;
                }

                return Some(data);
            }
            Err(e) => {
                tracing::warn!(
                    "尝试 {}/{} 失败，源: {}: {}",
                    attempt + 1,
                    retry_count + 1,
                    url,
                    e
                );

                // 网络错误等待后重试
                if attempt < retry_count {
                    tokio::time::sleep(Duration::from_secs((attempt + 1) as u64)).await;
                }
            }
        }
    }

    None
}

async fn fetch_remote_once(url: &str, timeout_ms: u64) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|e| format!("Failed to create client: {}", e))?;

    // 根据源类型优化请求头
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("Accept", "*/*".parse().unwrap());
    headers.insert("Accept-Encoding", "identity".parse().unwrap());
    headers.insert("Cache-Control", "no-cache".parse().unwrap());
    headers.insert("Connection", "close".parse().unwrap());

    let user_agent = if url.contains("github") || url.contains("raw.githubusercontent") {
        "curl/7.68.0"
    } else if url.contains("gitee") || url.contains("gitcode") {
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36"
    } else if url.contains("jsdelivr") || url.contains("fastly") {
        "DecoTV/1.0"
    } else {
        "Mozilla/5.0 (Linux; Android 11; SM-G973F) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Mobile Safari/537.36"
    };

    headers.insert("User-Agent", user_agent.parse().unwrap());

    let response = client
        .get(url)
        .headers(headers)
        .send()
        .await
        .map_err(|e| format!("Request failed: {}", e))?;

    if !response.status().is_success() {
        return Err(format!(
            "HTTP {}: {}",
            response.status(),
            response.status().canonical_reason().unwrap_or("Unknown")
        ));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("Failed to read response: {}", e))?;

    Ok(bytes.to_vec())
}

async fn get_spider_jar(state: &AppState, force_refresh: bool) -> SpiderInfo {
    let now = SystemTime::now();

    // 重置失败记录（定期清理）
    {
        let mut failed = state.failed_sources.lock().await;
        if let Ok(elapsed) = now.duration_since(failed.last_reset) {
            if elapsed.as_secs() > FAILURE_RESET_INTERVAL {
                failed.sources.clear();
                failed.last_reset = now;
                tracing::info!("重置失败的源列表");
            }
        }
    }

    // 1. 检查磁盘缓存（优先级最高）
    if !force_refresh {
        if let Some(disk_info) = load_spider_from_disk() {
            // 更新内存缓存
            let mut cache = state.spider_info.lock().await;
            *cache = disk_info.clone();
            tracing::info!("使用磁盘缓存的 spider jar");
            return disk_info;
        }
    }

    // 2. 检查内存缓存
    if !force_refresh {
        let cache = state.spider_info.lock().await;
        // 只有当缓存中有实际的 JAR 数据时才使用缓存
        if cache.buffer.is_some() {
            if let Ok(elapsed) = now.duration_since(cache.timestamp) {
                let ttl = if cache.success {
                    SUCCESS_TTL
                } else {
                    FAILURE_TTL
                };
                if elapsed.as_secs() < ttl {
                    tracing::info!(
                        "使用缓存的 spider jar (age: {}s, success: {})",
                        elapsed.as_secs(),
                        cache.success
                    );
                    return SpiderInfo {
                        cached: true,
                        ..cache.clone()
                    };
                }
            }
        }
    }

    let mut tried = 0;
    let candidates = get_candidates();
    let total_candidates = candidates.len(); // Store length before move

    // 过滤掉近期失败的源
    let failed_sources = state.failed_sources.lock().await;
    let active_candidates: Vec<String> = candidates
        .iter()
        .filter(|url| !failed_sources.sources.contains(*url))
        .cloned()
        .collect();
    drop(failed_sources);

    let candidates_to_try = if active_candidates.is_empty() {
        candidates
    } else {
        active_candidates
    };

    tracing::info!("尝试 {} spider jar 源", candidates_to_try.len());

    for url in candidates_to_try {
        tried += 1;
        tracing::info!("尝试 spider jar 源 {}/{}: {}", tried, total_candidates, url);

        if let Some(buffer) = fetch_remote(&url, 3000, 1).await {
            // 成功时从失败列表移除
            let mut failed = state.failed_sources.lock().await;
            failed.sources.remove(&url);
            drop(failed);

            let md5_hash = calculate_md5(&buffer);
            let size = buffer.len();

            let info = SpiderInfo {
                buffer: Some(buffer),
                md5: md5_hash,
                source: url.clone(),
                success: true,
                cached: false,
                timestamp: now,
                size,
                tried,
            };

            // 更新缓存
            let mut cache = state.spider_info.lock().await;
            *cache = info.clone();
            drop(cache);

            // 保存到磁盘
            if let Err(e) = save_spider_to_disk(&info) {
                tracing::warn!("保存 spider jar 到磁盘失败: {}", e);
            }

            tracing::info!(
                "成功从 {} 获取 spider jar (size: {} bytes, md5: {})",
                url,
                size,
                info.md5
            );
            return info;
        } else {
            // 失败时添加到失败列表
            let mut failed = state.failed_sources.lock().await;
            failed.sources.insert(url.clone());
        }
    }

    tracing::warn!("所有 spider jar 源失败，使用备用");

    let fallback_data = base64::engine::general_purpose::STANDARD
        .decode(FALLBACK_JAR_BASE64)
        .unwrap_or_default();

    let md5_hash = calculate_md5(&fallback_data);
    let size = fallback_data.len();

    let info = SpiderInfo {
        buffer: Some(fallback_data),
        md5: md5_hash,
        source: "fallback".to_string(),
        success: false,
        cached: false,
        timestamp: now,
        size,
        tried,
    };

    // 更新缓存
    let mut cache = state.spider_info.lock().await;
    *cache = info.clone();

    info
}

/// 获取缓存的订阅配置
async fn get_cached_subscription(
    state: &AppState,
    url: &str,
    force_refresh: bool,
    adult: bool,
) -> Result<SubscriptionConfig, String> {
    let mut cache = state.subscription_cache.lock().await;

    // 检查缓存是否有效（10分钟）
    let cache_valid = if let Some(cached) = cache.as_ref() {
        SystemTime::now()
            .duration_since(cached.cached_at)
            .map(|d| d.as_secs() < 600)
            .unwrap_or(false)
    } else {
        false
    };

    if force_refresh || !cache_valid {
        // 优先检查 PARSES_FILE 是否存在
        let parses_file_path = PathBuf::from(&*config_file::PARSES_FILE);
        let config = if parses_file_path.exists() {
            load_subscription_from_file(adult).await?
        } else {
            fetch_subscription(url, adult).await?
        };

        *cache = Some(CachedSubscription {
            config: config.clone(),
            cached_at: SystemTime::now(),
        });
        Ok(config)
    } else {
        Ok(cache.as_ref().unwrap().config.clone())
    }
}

/// 从文件加载配置
async fn load_subscription_from_file(adult: bool) -> Result<SubscriptionConfig, String> {
    // 加载 source configs
    let source_configs = if adult {
        config_file::load_source_configs_from_file()
            .await
            .map_err(|e| format!("加载源文件失败: {}", e))?
    } else {
        config_file::filter_adult_source_configs()
            .await
            .map_err(|e| format!("过滤成人源失败: {}", e))?
    };

    // 将 SourceConfig 转换为 Site
    let sites: Vec<Site> = source_configs
        .into_iter()
        .map(|sc| {
            // 使用统一的 site_type 判断逻辑
            let site_type = determine_site_type(&sc.api);

            Site {
                key: sc.key,
                name: sc.name,
                site_type,
                api: sc.api,
                jar: None,
                is_adult: Some(sc.is_adult),
                searchable: Some(1),
                quick_search: Some(1),
                filterable: Some(1),
                changeable: None,
            }
        })
        .collect();

    Ok(SubscriptionConfig {
        spider: None,
        sites: Some(sites),
        parses: Some(vec![
            Parse {
                name: "默认解析".to_string(),
                parse_type: 0,
                url: "https://jx.xmflv.com/?url=".to_string(),
            },
            Parse {
                name: "并发解析".to_string(),
                parse_type: 2,
                url: "Parallel".to_string(),
            },
        ]),
        lives: None,
    })
}

/// 默认配置
fn get_default_config() -> SubscriptionConfig {
    SubscriptionConfig {
        spider: Some(
            "https://cdn.jsdelivr.net/gh/FongMi/CatVodSpider@main/jar/spider.jar".to_string(),
        ),
        sites: Some(vec![Site {
            key: "demo".to_string(),
            name: "演示站点".to_string(),
            site_type: 3, // Spider 站点
            api: "https://example.com/api".to_string(),
            jar: None,
            is_adult: Some(false),
            searchable: Some(1),
            quick_search: Some(1),
            filterable: Some(1),
            changeable: None,
        }]),
        parses: Some(vec![
            Parse {
                name: "默认解析".to_string(),
                parse_type: 0,
                url: "https://jx.xmflv.com/?url=".to_string(),
            },
            Parse {
                name: "并发解析".to_string(),
                parse_type: 2,
                url: "Parallel".to_string(),
            },
        ]),
        lives: None,
    }
}

// ================== API 处理器 ==================

pub async fn get_config_handler(
    Query(params): Query<ConfigParams>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // 0. 构建 API 服务器地址（用于 M3U8 代理）

    let api_base_url = resolve_base_url(&headers);
    let m3u8_proxy_url = format!("{}/api/proxy/m3u8?url=", api_base_url);

    // 1. 获取订阅配置
    let subscription_url = params
        .subscription_url
        .as_deref()
        .unwrap_or(PARSES_URL.as_str());

    let force_refresh = params.force_spider_refresh.as_deref() == Some("1");
    // adult 参数，默认 false 过滤成人资源
    let adult = params.adult.unwrap_or(false);
    let mut config =
        match get_cached_subscription(&state, subscription_url, force_refresh, adult).await {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::error!("Failed to fetch subscription, using default: {}", e);
                get_default_config()
            }
        };

    // 2. 处理 Spider 逻辑（使用 10 秒超时）
    let spider_info = match tokio::time::timeout(
        Duration::from_secs(10),
        get_spider_jar(&state, force_refresh),
    )
    .await
    {
        Ok(info) => info,
        Err(_) => {
            tracing::warn!("Spider JAR 获取超时，使用备用");
            // 超时时使用 fallback
            let fallback_data = base64::engine::general_purpose::STANDARD
                .decode(FALLBACK_JAR_BASE64)
                .unwrap_or_default();
            let md5_hash = calculate_md5(&fallback_data);
            SpiderInfo {
                buffer: Some(fallback_data.clone()),
                md5: md5_hash.clone(),
                source: "fallback".to_string(),
                success: false,
                cached: false,
                timestamp: SystemTime::now(),
                size: fallback_data.len(),
                tried: 0,
            }
        }
    };

    // 构建 spider 字符串（使用代理 URL）
    let spider_proxy_url = format!("{}/api/proxy/spider.jar", api_base_url);
    let global_spider_jar = format!("{};md5;{}", spider_proxy_url, spider_info.md5);

    // 允许 URL 参数覆盖 Spider（仅当是公网地址时）
    let final_spider = if let Some(spider_url) = &params.spider {
        if spider_url.starts_with("http") && !is_private_host(spider_url) {
            spider_url.clone()
        } else {
            global_spider_jar
        }
    } else {
        // 优先使用代理 URL，忽略订阅配置中的 spider
        global_spider_jar
    };

    // 3. 过滤逻辑
    if !adult {
        if let Some(sites) = config.sites.as_mut() {
            sites.retain(|s| !is_adult_source(&s.api) && !is_adult_source(&s.name));
        }
    }

    let mode = params
        .mode
        .clone()
        .unwrap_or_else(|| "standard".to_string());

    // 5. 添加广告过滤解析器
    let mut parses = config.parses.unwrap_or_default();

    // 在开头插入广告过滤解析器（优先使用）
    parses.insert(
        0,
        Parse {
            name: "🚫 广告过滤".to_string(),
            parse_type: 0,
            url: m3u8_proxy_url,
        },
    );

    // 6. 组装响应（仅返回 TVBox 标准字段）
    let response = serde_json::json!({
        "spider": final_spider,
        "sites": config.sites.unwrap_or_default(),
        "parses": parses,
        "lives": config.lives.unwrap_or_default(),
    });

    tracing::info!(
        "Config generated: spider_success={}, mode={}, adult={}, subscription_url={}",
        spider_info.success,
        mode,
        adult,
        subscription_url
    );

    (StatusCode::OK, Json(response))
}

/// M3U8 代理处理器（带广告过滤）
pub async fn proxy_m3u8_handler(
    Query(params): Query<std::collections::HashMap<String, String>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let url = match params.get("url") {
        Some(u) => u.clone(), // 克隆以避免生命周期问题
        None => return (StatusCode::BAD_REQUEST, "Missing url parameter".to_string()),
    };

    tracing::info!("Proxying M3U8: {}", url);

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36")
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create client: {}", e),
            )
        }
    };

    let response = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                format!("Failed to fetch M3U8: {}", e),
            )
        }
    };

    if !response.status().is_success() {
        return (
            StatusCode::BAD_GATEWAY,
            format!("Upstream error: {}", response.status()),
        );
    }

    let content = match response.text().await {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to read response: {}", e),
            )
        }
    };

    // 使用 core crate 中的广告过滤函数
    let filtered = filter_ads_from_m3_u8(&content);

    // 重写 M3U8 中的 TS URL 为代理 URL
    let api_base_url = resolve_base_url(&headers);
    let proxy_base = format!("{}/api/proxy/ts?url=", api_base_url);
    let rewritten = rewrite_m3u8_urls(&filtered, &url, &proxy_base);

    // 后台并发预加载前几个 TS 片段（异步，不阻塞响应）
    let rewritten_clone = rewritten.clone();
    let url_clone = url.clone();
    tokio::spawn(async move {
        preload_ts_segments(&rewritten_clone, &url_clone).await;
    });

    (StatusCode::OK, rewritten)
}

/// 重写 M3U8 中的 TS URL 为代理 URL
fn rewrite_m3u8_urls(m3u8_content: &str, base_url: &str, proxy_base: &str) -> String {
    let mut result = String::new();

    for line in m3u8_content.lines() {
        let trimmed = line.trim();

        // 跳过注释行和空行
        if trimmed.starts_with('#') || trimmed.is_empty() {
            result.push_str(line);
            result.push('\n');
            continue;
        }

        // 处理 TS URL
        if trimmed.ends_with(".ts") || trimmed.ends_with(".m3u8") {
            // 解析为绝对 URL
            let absolute_url = if trimmed.starts_with("http://") || trimmed.starts_with("https://")
            {
                trimmed.to_string()
            } else {
                // 相对 URL，需要基于 base_url 解析
                resolve_relative_url(base_url, trimmed)
            };

            // 重写为代理 URL
            let encoded_url = urlencoding::encode(&absolute_url);
            result.push_str(&format!("{}{}", proxy_base, encoded_url));
            result.push('\n');
        } else {
            result.push_str(line);
            result.push('\n');
        }
    }

    result
}

/// 解析相对 URL
fn resolve_relative_url(base: &str, relative: &str) -> String {
    if let Ok(base_url) = url::Url::parse(base) {
        if let Ok(resolved) = base_url.join(relative) {
            return resolved.to_string();
        }
    }
    relative.to_string()
}

/// 并发预加载 TS 片段（前5个）
async fn preload_ts_segments(m3u8_content: &str, _base_url: &str) {
    let mut ts_urls = Vec::new();

    for line in m3u8_content.lines() {
        let trimmed = line.trim();
        if trimmed.ends_with(".ts") {
            // 提取原始 URL（从代理 URL 中解码）
            if let Some(url_param) = trimmed
                .strip_prefix("http://")
                .and_then(|s| s.split("url=").nth(1))
            {
                if let Ok(decoded) = urlencoding::decode(url_param) {
                    ts_urls.push(decoded.to_string());
                    if ts_urls.len() >= 5 {
                        break;
                    }
                }
            }
        }
    }

    // 并发下载前5个片段
    let tasks: Vec<_> = ts_urls
        .into_iter()
        .map(|url| {
            tokio::spawn(async move {
                if let Ok(client) = reqwest::Client::builder()
                    .timeout(Duration::from_secs(10))
                    .build()
                {
                    let _ = client.get(&url).send().await;
                }
            })
        })
        .collect();

    // 等待所有任务完成
    for task in tasks {
        let _ = task.await;
    }
}

/// TS 视频片段代理处理器（带缓存加速）
pub async fn proxy_ts_handler(
    Query(params): Query<std::collections::HashMap<String, String>>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let url = match params.get("url") {
        Some(u) => u,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                axum::http::HeaderMap::new(),
                "Missing url parameter".as_bytes().to_vec(),
            )
        }
    };

    // 检查缓存
    if let Some(cached_data) = state.ts_cache.get(url).await {
        tracing::debug!("TS 缓存: {}", url);
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            "video/mp2t".parse().unwrap(),
        );
        headers.insert(
            axum::http::header::CACHE_CONTROL,
            "public, max-age=432000".parse().unwrap(),
        );
        return (StatusCode::OK, headers, cached_data);
    }

    tracing::debug!("TS cache miss, downloading: {}", url);

    // 下载 TS 片段
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36")
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::http::HeaderMap::new(),
                format!("Failed to create client: {}", e).into_bytes(),
            )
        }
    };

    let response = match client.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                axum::http::HeaderMap::new(),
                format!("Failed to fetch TS: {}", e).into_bytes(),
            )
        }
    };

    if !response.status().is_success() {
        return (
            StatusCode::BAD_GATEWAY,
            axum::http::HeaderMap::new(),
            format!("Upstream error: {}", response.status()).into_bytes(),
        );
    }

    let data = match response.bytes().await {
        Ok(d) => d.to_vec(),
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::http::HeaderMap::new(),
                format!("Failed to read response: {}", e).into_bytes(),
            )
        }
    };

    // 保存到缓存
    state.ts_cache.insert(url.clone(), data.clone()).await;
    tracing::debug!("TS cached: {} ({} bytes)", url, data.len());

    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        "video/mp2t".parse().unwrap(),
    );
    headers.insert(
        axum::http::header::CACHE_CONTROL,
        "public, max-age=3600".parse().unwrap(),
    );

    (StatusCode::OK, headers, data)
}

/// Spider JAR 代理处理器
pub async fn proxy_spider_jar_handler(State(state): State<AppState>) -> impl IntoResponse {
    tracing::info!("Proxying Spider JAR");

    // 获取缓存的 Spider JAR（不强制刷新）
    let spider_info = get_spider_jar(&state, false).await;

    if let Some(buffer) = spider_info.buffer {
        tracing::info!(
            "使用磁盘缓存的 spider jar: {} bytes, md5: {}, source: {}",
            buffer.len(),
            spider_info.md5,
            spider_info.source
        );

        // 设置响应头
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            "application/java-archive".parse().unwrap(),
        );
        headers.insert(
            axum::http::header::CONTENT_DISPOSITION,
            "attachment; filename=\"spider.jar\"".parse().unwrap(),
        );
        headers.insert(
            axum::http::header::CACHE_CONTROL,
            "public, max-age=14400".parse().unwrap(),
        );

        (StatusCode::OK, headers, buffer)
    } else {
        tracing::error!("Failed to get Spider JAR");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::http::HeaderMap::new(),
            Vec::new(),
        )
    }
}

// ================== 搜索端点 ==================

const SEARCH_CACHE_DIR: &str = ".cache/sites";
const RUNNER_CACHE_DIR: &str = ".cache/runner";
const SPIDER_RUNNER_SOURCE: &str = include_str!("../resources/SpiderRunner.java");
const SEARCH_TIMEOUT_SECS: u64 = 20;

#[derive(Deserialize)]
pub struct SearchParams {
    site_key: String,
    query: String,
    #[serde(default)]
    spider: Option<String>,
    #[serde(default)]
    class_name: Option<String>,
}

/// 对齐 src-tauri 端 ApiSearchItem 的字段(serde 按 key 匹配)
#[derive(Serialize, Deserialize)]
pub struct SearchResultItem {
    pub vod_id: serde_json::Value,
    pub vod_name: String,
    pub vod_pic: String,
    #[serde(default)]
    pub vod_remarks: Option<String>,
    #[serde(default)]
    pub vod_play_url: Option<String>,
    #[serde(default)]
    pub vod_class: Option<String>,
    #[serde(default)]
    pub vod_year: Option<String>,
    #[serde(default)]
    pub vod_content: Option<String>,
    #[serde(default)]
    pub vod_douban_id: Option<serde_json::Value>,
    #[serde(default)]
    pub type_name: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct SearchResponse {
    pub list: Vec<SearchResultItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pagecount: Option<i32>,
}

fn check_java_available() -> bool {
    std::process::Command::new("java")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 解析 spider 配置: "URL;md5;<hash>" -> (url, Option<md5>)
fn parse_spider_spec(spec: &str) -> (String, Option<String>) {
    let mut parts = spec.splitn(3, ';');
    let url = parts.next().unwrap_or("").trim().to_string();
    let md5 = if parts.next() == Some("md5") {
        parts.next().map(|s| s.trim().to_string())
    } else {
        None
    };
    (url, md5)
}

/// 下载(或从磁盘缓存读取)站点 spider JAR,返回 jar 文件路径
async fn get_site_spider_jar(site_key: &str, spec: &str) -> Result<PathBuf, String> {
    let (url, expected_md5) = parse_spider_spec(spec);
    if url.is_empty() {
        return Err(format!("站点 {} 的 spider 配置为空", site_key));
    }

    // 缓存目录按 md5(无则按 url 哈希)命名,多个站点共享同一 JAR 时只下载一次
    let cache_name = expected_md5
        .clone()
        .unwrap_or_else(|| format!("{:x}", md5::compute(url.as_bytes())));
    let dir = PathBuf::from(CACHE_DIR).join(SEARCH_CACHE_DIR).join(&cache_name);
    let jar_path = dir.join(SPIDER_JAR_FILE);

    // 磁盘缓存命中
    if jar_path.exists() {
        if let Ok(data) = std::fs::read(&jar_path) {
            if let Some(exp) = &expected_md5 {
                if calculate_md5(&data) == *exp {
                    tracing::info!("站点 {} 使用磁盘缓存的 spider jar ({})", site_key, cache_name);
                    return Ok(jar_path);
                }
                tracing::warn!("站点 {} 缓存 jar MD5 不匹配,重新下载", site_key);
            } else {
                tracing::info!("站点 {} 使用磁盘缓存的 spider jar ({})", site_key, cache_name);
                return Ok(jar_path);
            }
        }
    }

    // 下载
    let data = fetch_remote(&url, 8000, 1)
        .await
        .ok_or_else(|| format!("下载 spider jar 失败: {}", url))?;

    if let Some(exp) = &expected_md5 {
        let actual = calculate_md5(&data);
        if &actual != exp {
            tracing::warn!("站点 {} jar MD5 不匹配: 期望 {} 实际 {}", site_key, exp, actual);
        }
    }

    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Err(format!("创建缓存目录失败: {}", e));
    }
    std::fs::write(&jar_path, &data).map_err(|e| format!("写入 jar 失败: {}", e))?;
    tracing::info!("站点 {} 下载 spider jar: {} bytes", site_key, data.len());
    Ok(jar_path)
}

/// 编译 SpiderRunner.java(首次),返回 runner 类目录
async fn ensure_runner_compiled() -> Result<PathBuf, String> {
    let runner_dir = PathBuf::from(CACHE_DIR).join(RUNNER_CACHE_DIR);
    let class_file = runner_dir.join("SpiderRunner.class");
    if class_file.exists() {
        return Ok(runner_dir);
    }

    if let Err(e) = tokio::fs::create_dir_all(&runner_dir).await {
        return Err(format!("创建 runner 目录失败: {}", e));
    }

    let src_path = runner_dir.join("SpiderRunner.java");
    if let Err(e) = tokio::fs::write(&src_path, SPIDER_RUNNER_SOURCE).await {
        return Err(format!("写入 runner 源码失败: {}", e));
    }

    let output = tokio::process::Command::new("javac")
        .arg("-encoding")
        .arg("UTF-8")
        .arg("-d")
        .arg(&runner_dir)
        .arg(&src_path)
        .output()
        .await
        .map_err(|e| format!("执行 javac 失败: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("编译 SpiderRunner 失败: {}", stderr.trim()));
    }

    tracing::info!("SpiderRunner 编译完成");
    Ok(runner_dir)
}

/// 执行 java 子进程调用 spider 类搜索,返回解析后的结果列表
async fn run_java_search(
    jar_path: &Path,
    runner_dir: &Path,
    class_name: &str,
    query: &str,
) -> Result<Vec<SearchResultItem>, String> {
    if !check_java_available() {
        return Err("Java 运行时不可用".to_string());
    }

    let sep = if cfg!(windows) { ";" } else { ":" };
    let classpath = format!("{}{}{}", jar_path.display(), sep, runner_dir.display());

    let output = tokio::process::Command::new("java")
        .arg("-Dfile.encoding=UTF-8")
        .arg("-cp")
        .arg(&classpath)
        .arg("SpiderRunner")
        .arg(jar_path)
        .arg("search")
        .arg(class_name)
        .arg(query)
        .output()
        .await
        .map_err(|e| format!("执行 java 失败: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("java 执行失败: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: SearchResponse = serde_json::from_str(&stdout)
        .map_err(|e| format!("解析 spider 搜索结果失败: {}, body: {}", e, &stdout[..stdout.len().min(300)]))?;
    Ok(parsed.list)
}

/// 从内存订阅缓存按 site_key 解析站点(参数缺失时的兜底)
async fn resolve_site_from_config(state: &AppState, site_key: &str) -> Option<(String, String)> {
    let config = state.subscription_cache.lock().await.as_ref()?.config.clone();
    let site = config.sites?.into_iter().find(|s| s.key == site_key)?;
    let class_name = site
        .api
        .strip_prefix("csp_")
        .unwrap_or(&site.api)
        .to_string();
    let spider_spec = site.jar.or(config.spider).unwrap_or_default();
    Some((class_name, spider_spec))
}

pub async fn search_handler(
    Query(params): Query<SearchParams>,
    State(state): State<AppState>,
) -> Json<SearchResponse> {
    let empty = SearchResponse {
        list: vec![],
        pagecount: None,
    };

    // 1. 解析类名与 spider 配置(优先用调用方传入的参数)
    let mut class_name = params.class_name.clone().unwrap_or_default();
    let mut spider_spec = params.spider.clone().unwrap_or_default();

    if class_name.is_empty() || spider_spec.is_empty() {
        if let Some((cn, ss)) = resolve_site_from_config(&state, &params.site_key).await {
            if class_name.is_empty() {
                class_name = cn;
            }
            if spider_spec.is_empty() {
                spider_spec = ss;
            }
        }
    }

    // 防御: 去掉残留的 csp_ 前缀
    let class_name = class_name.strip_prefix("csp_").unwrap_or(&class_name);
    if class_name.is_empty() {
        tracing::warn!("search: 站点 {} 无类名", params.site_key);
        return Json(empty);
    }

    // 2. 获取站点 spider JAR
    let jar_path = match get_site_spider_jar(&params.site_key, &spider_spec).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("search: 站点 {} 获取 jar 失败: {}", params.site_key, e);
            return Json(empty);
        }
    };

    // 3. 编译 runner
    let runner_dir = match ensure_runner_compiled().await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("search: 编译 runner 失败: {}", e);
            return Json(empty);
        }
    };

    // 4. 执行 java 搜索(超时控制)
    let fut = run_java_search(&jar_path, &runner_dir, class_name, &params.query);
    let result = match tokio::time::timeout(Duration::from_secs(SEARCH_TIMEOUT_SECS), fut).await {
        Ok(Ok(list)) => list,
        Ok(Err(e)) => {
            tracing::warn!("search: 站点 {} 搜索失败: {}", params.site_key, e);
            vec![]
        }
        Err(_) => {
            tracing::warn!("search: 站点 {} 搜索超时({}s)", params.site_key, SEARCH_TIMEOUT_SECS);
            vec![]
        }
    };

    tracing::info!(
        "search: 站点 {} 类 {} 查询 '{}' -> {} 条结果",
        params.site_key,
        class_name,
        params.query,
        result.len()
    );
    Json(SearchResponse {
        list: result,
        pagecount: None,
    })
}
