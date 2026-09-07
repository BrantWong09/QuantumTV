pub mod tunnel;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

#[derive(Debug, Clone)]
pub struct BridgeConfig {
    pub enabled: bool,
    pub host_port: u16,
    pub bridge_url: String,
    pub tunnel_port: u16,
    /// env QUANTUMTV_BRIDGE_URL，Phase A 首候选
    pub url_override: Option<String>,
    /// env/UI 远程桥接地址，Phase A 次候选
    pub remote_url: Option<String>,
}

const ENV_KEYS: [&str; 5] = [
    "QUANTUMTV_BRIDGE_ENABLED",
    "QUANTUMTV_ADB_HOST_PORT",
    "QUANTUMTV_TUNNEL_PORT",
    "QUANTUMTV_BRIDGE_URL",
    "QUANTUMTV_BRIDGE_REMOTE_URL",
];

impl BridgeConfig {
    /// 纯函数入口，便于测试；from_env 只收集已知键后委托到这里
    pub fn from_map(m: &HashMap<String, String>) -> BridgeConfig {
        let enabled = m.get("QUANTUMTV_BRIDGE_ENABLED").map(|v| v != "0").unwrap_or(true);
        let host_port = m.get("QUANTUMTV_ADB_HOST_PORT").and_then(|v| v.parse().ok()).unwrap_or(18080);
        let tunnel_port = m
            .get("QUANTUMTV_TUNNEL_PORT")
            .and_then(|v| v.parse().ok())
            .unwrap_or(tunnel::DEFAULT_TUNNEL_PORT);
        let url_override = m
            .get("QUANTUMTV_BRIDGE_URL")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let remote_url = m
            .get("QUANTUMTV_BRIDGE_REMOTE_URL")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        BridgeConfig { enabled, host_port, bridge_url: format!("http://127.0.0.1:{host_port}"), tunnel_port, url_override, remote_url }
    }

    pub fn from_env() -> BridgeConfig {
        let mut m = HashMap::new();
        for k in ENV_KEYS {
            if let Ok(v) = std::env::var(k) {
                m.insert(k.to_string(), v);
            }
        }
        BridgeConfig::from_map(&m)
    }
}

/// Windows 默认 SDK 位置；无 LOCALAPPDATA 时返回 None
pub fn default_sdk_root() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|l| PathBuf::from(l).join("Android").join("Sdk"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeStatus {
    Idle,
    Starting,
    Ready,
    Failed,
}

impl BridgeStatus {
    pub fn as_u8(self) -> u8 {
        match self {
            BridgeStatus::Idle => 0,
            BridgeStatus::Starting => 1,
            BridgeStatus::Ready => 2,
            BridgeStatus::Failed => 3,
        }
    }
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => BridgeStatus::Starting,
            2 => BridgeStatus::Ready,
            3 => BridgeStatus::Failed,
            _ => BridgeStatus::Idle,
        }
    }
}

static STATUS: AtomicU8 = AtomicU8::new(0);
/// 解析成功的生效桥接地址与其来源（Phase A/B/C）
static EFFECTIVE_URL: Mutex<Option<String>> = Mutex::new(None);
static EFFECTIVE_KIND: AtomicU8 = AtomicU8::new(EFFECTIVE_NONE);
/// 网盘 cookie 等 ext 载荷的脏标记: 置位后下次 /init 前桥接会重建 spider 实例
static EXT_DIRTY: AtomicBool = AtomicBool::new(false);
/// 当前注入的 ext 内容 (由 Tauri 层写入, bridge_post 读取)
static EXT_PAYLOAD: Mutex<Option<String>> = Mutex::new(None);

pub const EFFECTIVE_NONE: u8 = 0;
pub const EFFECTIVE_REMOTE: u8 = 1;
pub const EFFECTIVE_EMULATOR: u8 = 2;
pub const EFFECTIVE_AVD: u8 = 3;
pub const EFFECTIVE_TUNNEL: u8 = 4;

/// 网盘 cookie 变更时由 Tauri 层调用: 置脏标记 + 更新载荷
pub fn set_ext_payload(ext: Option<String>) {
    if let Ok(mut g) = EXT_PAYLOAD.lock() {
        *g = ext;
    }
    EXT_DIRTY.store(true, Ordering::SeqCst);
}

pub fn mark_ext_dirty() {
    EXT_DIRTY.store(true, Ordering::SeqCst);
}

/// 取走当前 ext 载荷 (消费脏标记)
pub(crate) fn take_ext_if_dirty() -> Option<String> {
    if EXT_DIRTY.swap(false, Ordering::SeqCst) {
        EXT_PAYLOAD.lock().ok().and_then(|g| g.clone())
    } else {
        None
    }
}

/// 桥接就绪后的实际可用地址；None = 未就绪（调用方应快速失败）
pub fn effective_url() -> Option<String> {
    EFFECTIVE_URL.lock().ok().and_then(|g| g.clone())
}

pub fn effective_kind() -> u8 {
    EFFECTIVE_KIND.load(Ordering::SeqCst)
}

pub(crate) fn set_effective(url: &str, kind: u8) {
    if let Ok(mut g) = EFFECTIVE_URL.lock() {
        *g = Some(url.to_string());
    }
    EFFECTIVE_KIND.store(kind, Ordering::SeqCst);
}

/// 清空生效 URL/kind；仅当处于 Ready 时状态归 Idle（供手动重试前复位）
pub fn reset_effective() {
    if let Ok(mut g) = EFFECTIVE_URL.lock() {
        *g = None;
    }
    EFFECTIVE_KIND.store(EFFECTIVE_NONE, Ordering::SeqCst);
    // 仅 Ready→Idle：try_begin_start 已接受 Idle/Failed，从这两个状态重试无需解锁；
    // Starting 时复位会误伤进行中的启动流程并造成测试间状态竞争，故不处理
    if status() == BridgeStatus::Ready {
        set_status(BridgeStatus::Idle);
    }
}

pub fn status() -> BridgeStatus {
    BridgeStatus::from_u8(STATUS.load(Ordering::SeqCst))
}

pub(crate) fn set_status(s: BridgeStatus) {
    STATUS.store(s.as_u8(), Ordering::SeqCst);
}

/// Idle 或 Failed 时允许进入 Starting（失败后允许手动再试一次），其余拒绝
pub(crate) fn try_begin_start() -> bool {
    STATUS
        .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
        || STATUS
            .compare_exchange(3, 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
}

pub fn adb_path(sdk_root: &Path) -> PathBuf {
    sdk_root.join("platform-tools").join("adb.exe")
}

pub fn all_device_serials(devices_out: &str) -> Vec<String> {
    devices_out
        .lines()
        .skip(1)
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let serial = it.next()?;
            let state = it.next()?;
            (state == "device").then(|| serial.to_string())
        })
        .collect()
}

pub fn parse_health_body(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("code").and_then(|c| c.as_i64()))
        .map(|c| c == 200)
        .unwrap_or(false)
}

pub(crate) async fn run_adb(adb: &Path, args: &[String]) -> Result<String, String> {
    let output = tokio::process::Command::new(adb)
        .args(args)
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("执行 adb 失败: {}", e))?;
    if !output.status.success() {
        return Err(format!(
            "adb {:?} 失败: {}",
            args,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub(crate) async fn probe_health(bridge_url: &str) -> bool {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .no_proxy()
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    match client.get(format!("{}/health", bridge_url)).send().await {
        Ok(resp) => match resp.text().await {
            Ok(body) => parse_health_body(&body),
            Err(_) => false,
        },
        Err(_) => false,
    }
}

/// Phase A 候选（优先级序）: env QUANTUMTV_BRIDGE_URL 覆盖值在前，remote_url 在后；去空白去重
pub(crate) fn remote_candidates(url_override: Option<&str>, remote_url: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for cand in [url_override, remote_url].into_iter().flatten() {
        let cand = cand.trim();
        if !cand.is_empty() && !out.iter().any(|x| x == cand) {
            out.push(cand.to_string());
        }
    }
    out
}

/// 应用启动钩子调用：读取环境变量并执行完整拉起流程（幂等，可并发调用）
pub async fn ensure_ready() -> Result<(), String> {
    ensure_ready_with(BridgeConfig::from_env()).await
}

pub async fn ensure_ready_with(cfg: BridgeConfig) -> Result<(), String> {
    if !cfg.enabled {
        return Ok(());
    }
    if !try_begin_start() {
        return Ok(()); // 已在 Starting/Ready，直接复用
    }
    tunnel::ensure_started(cfg.tunnel_port, cfg.host_port, cfg.bridge_url.clone()).await?;
    let result = startup_steps(&cfg).await;
    match result {
        Ok(()) => {
            set_status(BridgeStatus::Ready);
            log::info!("[桥接] 就绪: {}", cfg.bridge_url);
            Ok(())
        }
        Err(e) => {
            set_status(BridgeStatus::Failed);
            log::warn!("[桥接] 静默降级: {}", e);
            Err(e)
        }
    }
}

async fn startup_steps(cfg: &BridgeConfig) -> Result<(), String> {
    // Phase 0: 隧道注册 (APK 主动拨入; 等一个拨号周期)
    if tunnel::wait_registration(std::time::Duration::from_secs(6)).await {
        set_effective(&cfg.bridge_url, EFFECTIVE_TUNNEL);
        log::info!("[桥接] 隧道就绪: {}", cfg.bridge_url);
        return Ok(());
    }

    // Phase A: 远程真机直连（env 覆盖值优先，探活失败继续瀑布）
    for cand in remote_candidates(cfg.url_override.as_deref(), cfg.remote_url.as_deref()) {
        if probe_health(&cand).await {
            set_effective(&cand, EFFECTIVE_REMOTE);
            log::info!("[桥接] 远程桥接就绪: {}", cand);
            return Ok(());
        }
    }

    if tunnel::is_active() {
        // 等待期间隧道拨入: 以隧道为准
        set_effective(&cfg.bridge_url, EFFECTIVE_TUNNEL);
        return Ok(());
    }
    Err("未发现桥接设备：请将 bridge.apk 拖入模拟器安装并保持其运行，或在局域网真机上填写远程桥接地址".to_string())
}

/// 应用退出钩子: 关闭隧道服务与虚拟桥接监听, 复位状态
pub async fn shutdown() {
    tunnel::shutdown_all().await;
    reset_effective();
    set_status(BridgeStatus::Idle);
    log::info!("[桥接] 已关闭");
}

/// 在桥接 APK 所在设备上启动一个 activity (如网盘登录页)。
/// 通过 adb 找到第一个处于 device 状态的设备执行 am start。
#[allow(unused_variables)]
pub async fn launch_bridge_activity(
    cfg: &BridgeConfig,
    extra_key: &str,
    extra_value: &str,
) -> Result<(), String> {
    let adb = default_sdk_root().map(|sdk| adb_path(&sdk)).unwrap_or_else(|| PathBuf::from("adb"));
    if !adb.exists() {
        return Err(format!("adb 不存在: {}", adb.display()));
    }
    let devices_out = run_adb(&adb, &["devices".to_string()]).await?;
    let serial = all_device_serials(&devices_out)
        .into_iter()
        .next()
        .ok_or_else(|| "未发现已连接的设备 (adb devices 为空)".to_string())?;
    let args = vec![
        "-s".to_string(),
        serial.clone(),
        "shell".to_string(),
        "am".to_string(),
        "start".to_string(),
        "-n".to_string(),
        "com.quantumtv.bridge/.CloudLoginActivity".to_string(),
        "--es".to_string(),
        extra_key.to_string(),
        extra_value.to_string(),
    ];
    run_adb(&adb, &args).await?;
    log::info!("[桥接] 已在 {} 拉起 CloudLoginActivity ({}={})", serial, extra_key, extra_value);
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn config_defaults() {
        let cfg = BridgeConfig::from_map(&map(&[]));
        assert!(cfg.enabled);
        assert_eq!(cfg.host_port, 18080);
        assert_eq!(cfg.bridge_url, "http://127.0.0.1:18080");
        assert_eq!(cfg.tunnel_port, 18099);
        assert_eq!(cfg.remote_url, None);
    }

    #[test]
    fn config_env_overrides() {
        let cfg = BridgeConfig::from_map(&map(&[
            ("QUANTUMTV_BRIDGE_ENABLED", "0"),
            ("QUANTUMTV_ADB_HOST_PORT", "19090"),
            ("QUANTUMTV_TUNNEL_PORT", "18199"),
            ("QUANTUMTV_BRIDGE_URL", "http://192.168.1.20:8080"),
            ("QUANTUMTV_BRIDGE_REMOTE_URL", " http://192.168.1.20:8080 "),
        ]));
        assert!(!cfg.enabled);
        assert_eq!(cfg.host_port, 19090);
        assert_eq!(cfg.bridge_url, "http://127.0.0.1:19090");
        assert_eq!(cfg.tunnel_port, 18199);
        assert_eq!(cfg.url_override.as_deref(), Some("http://192.168.1.20:8080"));
        assert_eq!(cfg.remote_url.as_deref(), Some("http://192.168.1.20:8080"));
    }

    #[test]
    fn remote_candidates_dedup_and_order() {
        let out = remote_candidates(Some(" http://a:1 "), Some("http://b:2"));
        assert_eq!(out, vec!["http://a:1".to_string(), "http://b:2".to_string()]);
        assert!(remote_candidates(None, Some("   ")).is_empty());
    }

    #[test]
    fn status_roundtrip_and_effective() {
        set_effective("http://x", EFFECTIVE_TUNNEL);
        assert_eq!(effective_url().as_deref(), Some("http://x"));
        assert_eq!(effective_kind(), EFFECTIVE_TUNNEL);
        reset_effective();
        assert_eq!(effective_url(), None);
        assert_eq!(effective_kind(), EFFECTIVE_NONE);
    }

    #[test]
    fn parse_health_body_variants() {
        assert!(parse_health_body("{\"code\":200,\"err\":\"ok\"}"));
        assert!(!parse_health_body("{\"code\":500}"));
        assert!(!parse_health_body("garbage"));
    }
}
