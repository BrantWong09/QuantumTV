pub mod tunnel;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

#[derive(Debug, Clone)]
pub struct BridgeConfig {
    pub enabled: bool,
    pub avd: String,
    pub sdk_root: PathBuf,
    pub host_port: u16,
    pub bridge_url: String,
    pub apk_path: PathBuf,
    /// env QUANTUMTV_BRIDGE_URL，Phase A 首候选
    pub url_override: Option<String>,
    /// env QUANTUMTV_BRIDGE_REMOTE_URL，远程桥接地址
    pub remote_url: Option<String>,
    /// env QUANTUMTV_BRIDGE_ADB_ADDRESSES，逗号/换行分隔
    pub adb_addresses: Vec<String>,
    /// env QUANTUMTV_BRIDGE_AUTO_SCAN，"0"=false，默认 true
    pub auto_scan: bool,
}

const ENV_KEYS: [&str; 9] = [
    "QUANTUMTV_BRIDGE_ENABLED",
    "QUANTUMTV_BRIDGE_AVD",
    "QUANTUMTV_BRIDGE_SDK",
    "QUANTUMTV_ADB_HOST_PORT",
    "QUANTUMTV_BRIDGE_URL",
    "QUANTUMTV_BRIDGE_APK",
    "QUANTUMTV_BRIDGE_REMOTE_URL",
    "QUANTUMTV_BRIDGE_ADB_ADDRESSES",
    "QUANTUMTV_BRIDGE_AUTO_SCAN",
];

/// 逗号/换行分隔的地址串 → 去空白、去空的地址数组
fn normalize_addr_list(s: &str) -> Vec<String> {
    s.split([',', '\n'])
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty())
        .collect()
}

impl BridgeConfig {
    /// 纯函数入口，便于测试；from_env 只收集已知键后委托到这里
    pub fn from_map(m: &HashMap<String, String>) -> BridgeConfig {
        let enabled = m.get("QUANTUMTV_BRIDGE_ENABLED").map(|v| v != "0").unwrap_or(true);
        let avd = m.get("QUANTUMTV_BRIDGE_AVD").cloned().unwrap_or_else(|| "wexbridge".to_string());
        let sdk_root = m
            .get("QUANTUMTV_BRIDGE_SDK")
            .map(PathBuf::from)
            .or_else(default_sdk_root)
            .unwrap_or_else(|| PathBuf::from("Android/Sdk"));
        let host_port = m.get("QUANTUMTV_ADB_HOST_PORT").and_then(|v| v.parse().ok()).unwrap_or(18080);
        let url_override = m
            .get("QUANTUMTV_BRIDGE_URL")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        // bridge_url 恒为本地 forward 地址，不再被 env 覆盖
        let bridge_url = format!("http://127.0.0.1:{}", host_port);
        let remote_url = m
            .get("QUANTUMTV_BRIDGE_REMOTE_URL")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let adb_addresses = m
            .get("QUANTUMTV_BRIDGE_ADB_ADDRESSES")
            .map(|s| normalize_addr_list(s))
            .unwrap_or_default();
        let auto_scan = m.get("QUANTUMTV_BRIDGE_AUTO_SCAN").map(|v| v != "0").unwrap_or(true);
        let apk_path = m
            .get("QUANTUMTV_BRIDGE_APK")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("android/spider-bridge/out/bridge.apk"));
        BridgeConfig { enabled, avd, sdk_root, host_port, bridge_url, apk_path, url_override, remote_url, adb_addresses, auto_scan }
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
static WE_STARTED: AtomicBool = AtomicBool::new(false);
/// 本进程拉起的模拟器句柄；shutdown 时使用
pub(crate) static EMU_CHILD: Mutex<Option<tokio::process::Child>> = Mutex::new(None);
/// 本进程拉起的模拟器 serial；shutdown 时用它精准关闭对应设备
pub(crate) static STARTED_SERIAL: Mutex<Option<String>> = Mutex::new(None);
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

pub fn we_started() -> bool {
    WE_STARTED.load(Ordering::SeqCst)
}

pub(crate) fn set_we_started(v: bool) {
    WE_STARTED.store(v, Ordering::SeqCst);
}

pub fn adb_path(sdk_root: &Path) -> PathBuf {
    sdk_root.join("platform-tools").join("adb.exe")
}

pub fn emulator_path(sdk_root: &Path) -> PathBuf {
    sdk_root.join("emulator").join("emulator.exe")
}

/// 模拟器 DNS: 默认公共 DNS (国内 CDN 多节点, ISP DNS 可能轮询到死 IP 导致
/// wex spider 运行时配置 api.txt 拉取超时 → detail/category 连锁 NPE)。
/// env QUANTUMTV_EMU_DNS 覆盖; 设为 "off"/"0" 跳过该参数。
/// -writable-system: 允许 adb remount 后钉 /etc/hosts (kstore.vip 解析污染兜底)。
pub fn emulator_args(avd: &str) -> Vec<String> {
    let mut args = vec![
        "-avd".to_string(),
        avd.to_string(),
        "-no-snapshot-save".to_string(),
        "-no-boot-anim".to_string(),
        "-gpu".to_string(),
        "auto".to_string(),
        "-writable-system".to_string(),
    ];
    let dns = std::env::var("QUANTUMTV_EMU_DNS").unwrap_or_default();
    let dns = dns.trim();
    if dns.is_empty() {
        args.extend(["-dns-server".to_string(), "223.5.5.5,119.29.29.29".to_string()]);
    } else if dns != "off" && dns != "0" {
        args.extend(["-dns-server".to_string(), dns.to_string()]);
    }
    args
}

pub fn forward_args(host_port: u16, device_port: u16) -> Vec<String> {
    vec![format!("tcp:{}", host_port), format!("tcp:{}", device_port)]
}

/// 从 `adb devices` 输出提取处于 device 状态的模拟器 serial
pub fn emulator_serial(devices_out: &str) -> Option<String> {
    emulator_serials(devices_out).into_iter().next()
}

/// `adb devices` 输出中所有处于 device 状态的模拟器 serial
fn emulator_serials(devices_out: &str) -> Vec<String> {
    devices_out
        .lines()
        .skip(1) // 跳过 "List of devices attached"
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let serial = it.next()?;
            let state = it.next()?;
            (serial.starts_with("emulator-") && state == "device").then(|| serial.to_string())
        })
        .collect()
}

/// 第三方模拟器/WSA 常见 TCP adb 端口（LDPlayer/Nox/MuMu/WSA）
pub const SCAN_PORTS: &[u16] = &[5555, 5557, 62001, 62025, 62026, 62027, 16384, 16416, 16448, 58526];

/// `adb devices` 输出中所有处于 device 状态的 serial (含 emulator- 与 host:port)
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

/// 从 `adb devices` 输出解析 TCP 型 serial（`host:port`，端口可解析为 u16），仅 device 状态
pub fn connectable_serials(devices_out: &str) -> Vec<String> {
    devices_out
        .lines()
        .skip(1)
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let serial = it.next()?;
            let state = it.next()?;
            let port_ok = serial.rsplit(':').next()?.parse::<u16>().is_ok();
            (port_ok && serial.contains(':') && state == "device").then(|| serial.to_string())
        })
        .collect()
}

pub fn has_emulator_device(devices_out: &str) -> bool {
    !emulator_serials(devices_out).is_empty()
}

/// 从 `adb -s <serial> emu avd name` 输出解析 AVD 名。
/// 兼容两种格式：`OK: <name>`（旧版 platform-tools 单行）与 `<name>\nOK`（新版两行）。
pub fn parse_avd_name(out: &str) -> Option<String> {
    let lines: Vec<&str> = out.lines().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
    // 旧版: "OK: wexbridge"
    for l in &lines {
        if let Some(name) = l.strip_prefix("OK:") {
            let name = name.trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    // 新版: 首个非噪音行为 AVD 名，且必须伴随独立一行 "OK"
    let name = lines.iter().copied().find(|l| {
        !l.starts_with("OK:")
            && *l != "OK"
            && !l.starts_with("error:")
            && !l.starts_with("Android Console")
    })?;
    lines.iter().any(|l| *l == "OK").then(|| name.to_string())
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

/// Phase B 连接序列: 手动地址在前，auto_scan 时追加扫描端口（127.0.0.1:p）；按字符串去重
pub(crate) fn phase_b_addresses(manual: &[String], auto_scan: bool) -> Vec<String> {
    let mut out: Vec<String> = manual.to_vec();
    if auto_scan {
        for p in SCAN_PORTS {
            let addr = format!("127.0.0.1:{}", p);
            if !out.contains(&addr) {
                out.push(addr);
            }
        }
    }
    out
}

pub(crate) async fn spawn_emulator(cfg: &BridgeConfig) -> Result<(), String> {
    let emu = emulator_path(&cfg.sdk_root);
    if !emu.exists() {
        return Err(format!("emulator 不存在: {}", emu.display()));
    }
    let child = tokio::process::Command::new(&emu)
        .args(&emulator_args(&cfg.avd))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("拉起模拟器失败: {}", e))?;
    *EMU_CHILD.lock().unwrap() = Some(child);
    set_we_started(true);
    log::info!("[桥接] 模拟器 {} 拉起中", cfg.avd);
    Ok(())
}

pub(crate) async fn wait_boot(adb: &Path, serial: &str) -> Result<(), String> {
    let wait_args = vec!["-s".to_string(), serial.to_string(), "wait-for-device".to_string()];
    // wait-for-device 在模拟器未出现时会无限阻塞，必须限时（wait_boot 两段各限时 120s，最坏约 240s）
    tokio::time::timeout(std::time::Duration::from_secs(120), run_adb(adb, &wait_args))
        .await
        .map_err(|_| format!("等待设备 {} 上线超时 (120s)", serial))??;
    let prop_args = vec![
        "-s".to_string(),
        serial.to_string(),
        "shell".to_string(),
        "getprop".to_string(),
        "sys.boot_completed".to_string(),
    ];
    // 指数退避 1s→5s，总上限 120s
    let mut delay = std::time::Duration::from_secs(1);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err("等待模拟器启动超时 (120s)".to_string());
        }
        if let Ok(out) = run_adb(adb, &prop_args).await {
            if out.trim() == "1" {
                log::info!("[桥接] 模拟器 {} 启动完成", serial);
                return Ok(());
            }
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(std::time::Duration::from_secs(5));
    }
}

pub(crate) async fn ensure_apk_installed(
    adb: &Path,
    serial: &str,
    apk_path: &Path,
) -> Result<(), String> {
    let pkg = "com.quantumtv.bridge";
    let check_args = vec![
        "-s".to_string(),
        serial.to_string(),
        "shell".to_string(),
        "pm".to_string(),
        "path".to_string(),
        pkg.to_string(),
    ];
    if let Ok(out) = run_adb(adb, &check_args).await {
        if out.trim_start().starts_with("package:") {
            log::info!("[桥接] APK 已安装，跳过");
            return Ok(());
        }
    }
    if !apk_path.exists() {
        return Err(format!(
            "桥接 APK 未安装且本地不存在: {}（可先运行 android/spider-bridge/build.ps1，或设置 QUANTUMTV_BRIDGE_APK）",
            apk_path.display()
        ));
    }
    let install_args = vec![
        "-s".to_string(),
        serial.to_string(),
        "install".to_string(),
        "-r".to_string(),
        apk_path.display().to_string(),
    ];
    run_adb(adb, &install_args).await?;
    log::info!("[桥接] APK 安装完成");
    Ok(())
}

pub(crate) async fn start_bridge_service(adb: &Path, serial: &str) -> Result<(), String> {
    let args = vec![
        "-s".to_string(),
        serial.to_string(),
        "shell".to_string(),
        "am".to_string(),
        "start-foreground-service".to_string(),
        "-n".to_string(),
        "com.quantumtv.bridge/.BridgeService".to_string(),
    ];
    run_adb(adb, &args).await?;
    log::info!("[桥接] BridgeService 启动指令已发送");
    Ok(())
}

/// adb forward 完整参数（纯函数，含 -s serial 定向）
pub(crate) fn forward_cmd_args(serial: &str, host_port: u16) -> Vec<String> {
    vec!["-s".to_string(), serial.to_string(), "forward".to_string()]
        .into_iter()
        .chain(forward_args(host_port, 8080))
        .collect()
}

pub(crate) async fn forward_port(adb: &Path, serial: &str, host_port: u16) -> Result<(), String> {
    run_adb(adb, &forward_cmd_args(serial, host_port)).await.map(|_| ())
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
    // Phase A: 远程真机直连（env 覆盖值优先，探活失败继续瀑布）
    for cand in remote_candidates(cfg.url_override.as_deref(), cfg.remote_url.as_deref()) {
        if probe_health(&cand).await {
            set_effective(&cand, EFFECTIVE_REMOTE);
            log::info!("[桥接] 远程桥接就绪: {}", cand);
            return Ok(());
        }
    }

    // B/C 需要 adb
    let adb = adb_path(&cfg.sdk_root);
    if !adb.exists() {
        return Err(format!("adb 不存在: {}，且远程候选均未就绪", adb.display()));
    }

    // Phase B: 第三方模拟器（手动地址优先，auto_scan 追加扫描端口；总上限 120s）
    let b_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);
    for addr in phase_b_addresses(&cfg.adb_addresses, cfg.auto_scan) {
        if tokio::time::Instant::now() >= b_deadline {
            log::warn!("[桥接] Phase B 总超时，转入官方 AVD");
            break;
        }
        // connect 幂等；失败不中断（该地址可能不是 adb 端口）；单地址 5s 上限防个别地址卡死
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_adb(&adb, &["connect".to_string(), addr.clone()]),
        )
        .await;
        if !wait_serial_device(&adb, &addr, 5).await {
            continue;
        }
        // 手动地址 = 用户显式指定即授权安装；扫描发现的地址只探活复用，绝不装 APK（防误连无关 Android 实例）
        let manual = cfg.adb_addresses.contains(&addr);
        if try_adb_device(&adb, &addr, cfg, b_deadline, manual).await.is_ok() {
            set_effective(&cfg.bridge_url, EFFECTIVE_EMULATOR);
            log::info!("[桥接] 第三方模拟器桥接就绪: {}", addr);
            return Ok(());
        }
    }

    // Phase C: 官方 AVD（复用运行中的或拉起新的）
    ensure_avd_bridge(cfg, &adb).await?;
    set_effective(&cfg.bridge_url, EFFECTIVE_AVD);
    Ok(())
}

/// serial 是否为配置的 AVD
async fn serial_matches_avd(adb: &Path, serial: &str, avd: &str) -> bool {
    let args = vec![
        "-s".to_string(),
        serial.to_string(),
        "emu".to_string(),
        "avd".to_string(),
        "name".to_string(),
    ];
    match run_adb(adb, &args).await {
        Ok(out) => parse_avd_name(&out).as_deref() == Some(avd),
        Err(_) => false,
    }
}

/// 轮询 `adb devices` 直到指定 TCP serial 出现且为 device 状态
async fn wait_serial_device(adb: &Path, serial: &str, secs: u64) -> bool {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        if let Ok(out) = run_adb(adb, &["devices".to_string()]).await {
            if connectable_serials(&out).iter().any(|s| s == serial) {
                return true;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

/// 单个第三方设备处理链路，按地址来源分支:
/// - allow_install=true（手动配置/官方 AVD 授权）: 等就绪 → 装 APK → forward → 启动服务 → 健康轮询
/// - allow_install=false（扫描发现）: 等就绪 → forward → 单次探活；健康说明设备已有桥接在跑，直接复用；
///   不健康视为非桥接设备（如无关 Android 实例），跳过且绝不装 APK/启服务
/// 注意: wait_boot 内部各自有 120s 上限，可能超过 Phase B 总 deadline，
/// 故整段用 tokio::time::timeout 包裹，到点强制转入 Phase C。
async fn try_adb_device(
    adb: &Path,
    serial: &str,
    cfg: &BridgeConfig,
    deadline: tokio::time::Instant,
    allow_install: bool,
) -> Result<(), String> {
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    tokio::time::timeout(
        remaining,
        async {
            wait_boot(adb, serial).await?;
            if !allow_install {
                // 扫描路径: 探活必须发生在装 APK 之前
                forward_port(adb, serial, cfg.host_port).await?;
                if probe_health(&cfg.bridge_url).await {
                    return Ok(());
                }
                return Err(format!("非桥接设备，跳过: {} 未运行桥接服务", serial));
            }
            ensure_apk_installed(adb, serial, &cfg.apk_path).await?;
            forward_port(adb, serial, cfg.host_port).await?;
            start_bridge_service(adb, serial).await?;
            loop {
                if probe_health(&cfg.bridge_url).await {
                    return Ok(());
                }
                // 健康轮询单次 1s；外层 timeout 负责总时长约束
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        },
    )
    .await
    .map_err(|_| "Phase B 设备处理超时".to_string())?
}

/// Phase C: 官方 AVD 桥接（原 startup_steps 主体逻辑，仅 forward_port 增加 serial）
async fn ensure_avd_bridge(cfg: &BridgeConfig, adb: &Path) -> Result<(), String> {
    // 模拟器在跑吗？只认配置 AVD 的 serial，绝不劫持外部其他模拟器
    let devices = run_adb(adb, &["devices".to_string()]).await?;
    let mut serial: Option<String> = None;
    for s in emulator_serials(&devices) {
        if serial_matches_avd(adb, &s, &cfg.avd).await {
            log::info!("[桥接] 复用运行中的 AVD {} ({})", cfg.avd, s);
            serial = Some(s);
            break;
        }
    }
    let serial = match serial {
        Some(s) => s,
        None => {
            spawn_emulator(cfg).await?;
            // 等配置 AVD 的 serial 出现（offline→device 由 wait_boot 内的 wait-for-device 兜底）
            wait_for_avd_serial(adb, cfg).await?
        }
    };
    if we_started() {
        *STARTED_SERIAL.lock().unwrap() = Some(serial.clone());
    }

    // 等系统启动完成
    wait_boot(adb, &serial).await?;

    // APK
    ensure_apk_installed(adb, &serial, &cfg.apk_path).await?;

    // forward + 启动服务
    forward_port(adb, &serial, cfg.host_port).await?;
    start_bridge_service(adb, &serial).await?;

    // 健康轮询（30s 上限，1s 间隔）
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if probe_health(&cfg.bridge_url).await {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("桥接健康检查超时 (30s)".to_string());
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

/// 轮询等待配置 AVD 的模拟器 serial 出现并处于 device 状态
async fn wait_for_avd_serial(adb: &Path, cfg: &BridgeConfig) -> Result<String, String> {
    // 模拟器注册到 adb 可能超过 15s（冷启动/AVD 锁释放），给足 60s
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let out = run_adb(adb, &["devices".to_string()]).await?;
        for s in emulator_serials(&out) {
            if serial_matches_avd(adb, &s, &cfg.avd).await {
                return Ok(s);
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("模拟器 {} 未出现在设备列表 (60s)", cfg.avd));
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

/// 应用退出钩子调用：只关闭自己拉起的模拟器
pub async fn shutdown() {
    if !we_started() {
        return;
    }
    let cfg = BridgeConfig::from_env();
    let adb = adb_path(&cfg.sdk_root);
    // 1. 优雅关闭：优先用记住的 serial 精准关闭，拿不到时退回无 serial 广播
    let serial = STARTED_SERIAL.lock().unwrap().take();
    let _ = match serial {
        Some(s) => {
            run_adb(&adb, &["-s".to_string(), s, "emu".to_string(), "kill".to_string()]).await
        }
        None => run_adb(&adb, &["emu".to_string(), "kill".to_string()]).await,
    };
    // 2. 最多等 3s
    let child = EMU_CHILD.lock().unwrap().take();
    if let Some(mut child) = child {
        for _ in 0..6 {
            if matches!(child.try_wait(), Ok(Some(_))) {
                log::info!("[桥接] 模拟器已退出");
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        let _ = child.kill().await;
    }
    set_we_started(false);
    set_status(BridgeStatus::Idle);
    log::info!("[桥接] 已关闭");
}

/// 在桥接 APK 所在设备上启动一个 activity (如网盘登录页)。
/// 通过 adb 找到第一个处于 device 状态的设备执行 am start。
pub async fn launch_bridge_activity(
    cfg: &BridgeConfig,
    extra_key: &str,
    extra_value: &str,
) -> Result<(), String> {
    let adb = adb_path(&cfg.sdk_root);
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
        assert_eq!(cfg.avd, "wexbridge");
        assert_eq!(cfg.host_port, 18080);
        assert_eq!(cfg.bridge_url, "http://127.0.0.1:18080");
        assert_eq!(cfg.apk_path, PathBuf::from("android/spider-bridge/out/bridge.apk"));
    }

    #[test]
    fn config_env_overrides() {
        let cfg = BridgeConfig::from_map(&map(&[
            ("QUANTUMTV_BRIDGE_ENABLED", "0"),
            ("QUANTUMTV_BRIDGE_AVD", "myavd"),
            ("QUANTUMTV_BRIDGE_SDK", "D:/Android/Sdk"),
            ("QUANTUMTV_ADB_HOST_PORT", "19090"),
            ("QUANTUMTV_BRIDGE_URL", "http://127.0.0.1:1234"),
            ("QUANTUMTV_BRIDGE_APK", "C:/bridge.apk"),
        ]));
        assert!(!cfg.enabled);
        assert_eq!(cfg.avd, "myavd");
        assert_eq!(cfg.sdk_root, PathBuf::from("D:/Android/Sdk"));
        assert_eq!(cfg.host_port, 19090);
        // QUANTUMTV_BRIDGE_URL 现在是 Phase A 首候选（url_override），bridge_url 恒为本地地址
        assert_eq!(cfg.url_override.as_deref(), Some("http://127.0.0.1:1234"));
        assert_eq!(cfg.bridge_url, "http://127.0.0.1:19090");
        assert_eq!(cfg.apk_path, PathBuf::from("C:/bridge.apk"));
    }

    #[test]
    fn config_remote_keys() {
        let cfg = BridgeConfig::from_map(&map(&[
            ("QUANTUMTV_BRIDGE_REMOTE_URL", " http://192.168.1.20:8080 "),
            ("QUANTUMTV_BRIDGE_ADB_ADDRESSES", " 127.0.0.1:5555 ,\n127.0.0.1:16384 , ,"),
            ("QUANTUMTV_BRIDGE_AUTO_SCAN", "0"),
        ]));
        assert_eq!(cfg.remote_url.as_deref(), Some("http://192.168.1.20:8080"));
        assert_eq!(cfg.adb_addresses, vec!["127.0.0.1:5555", "127.0.0.1:16384"]);
        assert!(!cfg.auto_scan);
    }

    #[test]
    fn config_remote_keys_default_and_empty() {
        let cfg = BridgeConfig::from_map(&map(&[
            ("QUANTUMTV_BRIDGE_REMOTE_URL", "   "),
            ("QUANTUMTV_BRIDGE_ADB_ADDRESSES", " , ,"),
        ]));
        assert_eq!(cfg.remote_url, None);
        assert!(cfg.adb_addresses.is_empty());
        assert!(cfg.auto_scan, "未配置默认开启扫描");
    }

    #[test]
    fn status_transitions() {
        set_status(BridgeStatus::Idle);
        assert_eq!(status(), BridgeStatus::Idle);
        assert!(try_begin_start());
        assert_eq!(status(), BridgeStatus::Starting);
        assert!(!try_begin_start(), "Starting 状态不允许再次进入");
        set_status(BridgeStatus::Failed);
        assert!(try_begin_start(), "Failed 允许重新拉起");
        set_status(BridgeStatus::Idle);
    }

    #[test]
    fn from_u8_roundtrip() {
        for s in [BridgeStatus::Idle, BridgeStatus::Starting, BridgeStatus::Ready, BridgeStatus::Failed] {
            assert_eq!(BridgeStatus::from_u8(s.as_u8()), s);
        }
    }

    #[test]
    fn paths_and_args() {
        let sdk = Path::new("D:/Android/Sdk");
        assert_eq!(adb_path(sdk), PathBuf::from("D:/Android/Sdk/platform-tools/adb.exe"));
        assert_eq!(emulator_path(sdk), PathBuf::from("D:/Android/Sdk/emulator/emulator.exe"));
        // 默认注入公共 DNS (ISP DNS 轮询到死 CDN IP 的规避)
        let args = emulator_args("wexbridge");
        assert!(args.starts_with(&[
            "-avd".to_string(),
            "wexbridge".to_string(),
            "-no-snapshot-save".to_string(),
            "-no-boot-anim".to_string(),
            "-gpu".to_string(),
            "auto".to_string(),
        ]));
        let di = args.iter().position(|a| a == "-dns-server").expect("dns-server missing");
        assert_eq!(args[di + 1], "223.5.5.5,119.29.29.29");
        assert_eq!(forward_args(18080, 8080), vec!["tcp:18080".to_string(), "tcp:8080".to_string()]);
    }

    #[test]
    fn emulator_args_dns_env_override() {
        // SAFETY: 单测串行场景下临时改 env; 改回避免影响其他用例
        std::env::set_var("QUANTUMTV_EMU_DNS", "8.8.8.8");
        let args = emulator_args("wexbridge");
        let di = args.iter().position(|a| a == "-dns-server").expect("dns missing");
        assert_eq!(args[di + 1], "8.8.8.8");

        std::env::set_var("QUANTUMTV_EMU_DNS", "off");
        let args = emulator_args("wexbridge");
        assert!(!args.contains(&"-dns-server".to_string()));

        std::env::remove_var("QUANTUMTV_EMU_DNS");
        let args = emulator_args("wexbridge");
        assert!(args.contains(&"-dns-server".to_string()));
    }

    #[test]
    fn forward_cmd_args_includes_serial() {
        let args = forward_cmd_args("127.0.0.1:5555", 18080);
        assert_eq!(
            args,
            vec!["-s", "127.0.0.1:5555", "forward", "tcp:18080", "tcp:8080"]
        );
    }

    #[test]
    fn emulator_serial_parsing() {
        let out = "List of devices attached\nemulator-5554\tdevice\n\n";
        assert_eq!(emulator_serial(out).as_deref(), Some("emulator-5554"));
        assert!(has_emulator_device(out));
        assert!(!has_emulator_device("List of devices attached\n"));
        assert!(!has_emulator_device("List of devices attached\nemulator-5554\toffline\n"));
        assert!(!has_emulator_device("List of devices attached\nABC123\tdevice\n"), "非 emulator- 前缀不算");
    }

    #[test]
    fn emulator_serials_collects_all() {
        let out = "List of devices attached\nemulator-5554\tdevice\nemulator-5556\tdevice\nemulator-5558\toffline\nABC123\tdevice\n";
        assert_eq!(
            emulator_serials(out),
            vec!["emulator-5554".to_string(), "emulator-5556".to_string()]
        );
        assert_eq!(emulator_serial(out).as_deref(), Some("emulator-5554"));
    }

    #[test]
    fn parse_avd_name_parsing() {
        assert_eq!(parse_avd_name("OK: wexbridge\r\n").as_deref(), Some("wexbridge"));
        assert_eq!(parse_avd_name("error: unknown host service").as_deref(), None);
        assert_eq!(parse_avd_name("").as_deref(), None);
    }

    #[test]
    fn parse_avd_name_two_line_ok_format() {
        // 新版 platform-tools: 第一行为 AVD 名，随后独立一行 OK（实测输出）
        assert_eq!(parse_avd_name("wexbridge\r\r\nOK\r\r\n").as_deref(), Some("wexbridge"));
        assert_eq!(parse_avd_name("wexbridge\nOK\n").as_deref(), Some("wexbridge"));
        assert_eq!(parse_avd_name("OK\n").as_deref(), None);
        assert_eq!(parse_avd_name("error: device offline\nOK\n").as_deref(), None);
    }

    #[test]
    fn health_body_parsing() {
        assert!(parse_health_body(r#"{"code":200,"err":"ok","data":{"initialized":true}}"#));
        assert!(parse_health_body(r#"{"code":200,"err":"ok"}"#));
        assert!(!parse_health_body(r#"{"code":500,"err":"boom"}"#));
        assert!(!parse_health_body("not json"));
        assert!(!parse_health_body(""));
    }

    #[test]
    fn scan_ports_cover_known_emulators() {
        assert!(SCAN_PORTS.contains(&5555), "LDPlayer");
        assert!(SCAN_PORTS.contains(&62001), "Nox");
        assert!(SCAN_PORTS.contains(&16384), "MuMu");
        assert!(SCAN_PORTS.contains(&58526), "WSA");
        assert!(SCAN_PORTS.iter().all(|p| *p >= 1024));
    }

    #[test]
    fn connectable_serials_parsing() {
        let out = "List of devices attached\n127.0.0.1:5555\tdevice\nemulator-5554\tdevice\n192.168.1.20:5555\toffline\nnot-a-port\tdevice\n";
        assert_eq!(connectable_serials(out), vec!["127.0.0.1:5555".to_string()]);
        assert!(connectable_serials("List of devices attached\n").is_empty());
    }

    #[test]
    fn all_device_serials_parsing() {
        let out = "List of devices attached\n127.0.0.1:5555\tdevice\nemulator-5554\tdevice\n192.168.1.20:5555\toffline\n";
        assert_eq!(
            all_device_serials(out),
            vec!["127.0.0.1:5555".to_string(), "emulator-5554".to_string()]
        );
        assert!(all_device_serials("List of devices attached\n").is_empty());
    }

    #[test]
    fn ensure_ready_with_disabled_config_is_noop() {
        set_status(BridgeStatus::Idle);
        let cfg = BridgeConfig::from_map(&map(&[("QUANTUMTV_BRIDGE_ENABLED", "0")]));
        // 用同步 runtime 包一层，仅验证短路行为，不触发任何进程
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let r = rt.block_on(ensure_ready_with(cfg));
        assert!(r.is_ok());
        assert_eq!(status(), BridgeStatus::Idle, "禁用时不改变状态");
    }

    // 合并为单个测试：EFFECTIVE_*/STATUS 为进程级全局，cargo test 并行执行时
    // 两个测试互相写入会造成间歇性断言失败，串在一个函数内即可消除竞争
    #[test]
    fn effective_url_roundtrip_and_reset_status() {
        set_effective("http://192.168.1.20:8080", EFFECTIVE_REMOTE);
        assert_eq!(effective_url().as_deref(), Some("http://192.168.1.20:8080"));
        assert_eq!(effective_kind(), EFFECTIVE_REMOTE);
        set_effective("http://127.0.0.1:18080", EFFECTIVE_EMULATOR);
        assert_eq!(effective_kind(), EFFECTIVE_EMULATOR);
        reset_effective();
        assert_eq!(effective_url(), None);
        assert_eq!(effective_kind(), EFFECTIVE_NONE);

        // Starting 时不复位：try_begin_start 已接受 Idle/Failed，从这两个状态重试无需解锁
        set_status(BridgeStatus::Starting);
        set_effective("http://127.0.0.1:18080", EFFECTIVE_EMULATOR);
        reset_effective();
        assert_eq!(effective_url(), None);
        assert_eq!(status(), BridgeStatus::Starting);
        set_status(BridgeStatus::Idle);

        // Ready 时复位归 Idle：仅从 Ready 重试才需要解锁
        set_status(BridgeStatus::Ready);
        set_effective("http://127.0.0.1:18080", EFFECTIVE_EMULATOR);
        reset_effective();
        assert_eq!(effective_url(), None);
        assert_eq!(status(), BridgeStatus::Idle);
    }

    #[test]
    fn remote_candidates_order_and_dedup() {
        assert_eq!(remote_candidates(None, None), Vec::<String>::new());
        assert_eq!(
            remote_candidates(Some(" http://a:1 "), Some("http://b:2")),
            vec!["http://a:1".to_string(), "http://b:2".to_string()]
        );
        assert_eq!(
            remote_candidates(Some("http://a:1"), Some("http://a:1")),
            vec!["http://a:1".to_string()],
            "重复候选去重"
        );
        assert_eq!(remote_candidates(Some("  "), Some("http://b:2")), vec!["http://b:2".to_string()]);
    }

    #[test]
    fn phase_b_addresses_manual_first_scan_dedup() {
        let manual = vec!["127.0.0.1:5555".to_string()];
        let addrs = phase_b_addresses(&manual, true);
        assert_eq!(&addrs[0], "127.0.0.1:5555", "手动地址在前");
        assert!(addrs.iter().any(|a| a == "127.0.0.1:16384"));
        assert_eq!(addrs.iter().filter(|a| **a == "127.0.0.1:5555").count(), 1, "与扫描端口去重");
        assert!(phase_b_addresses(&manual, false).len() == 1, "关闭扫描只剩手动");
        assert!(phase_b_addresses(&[], true).len() == SCAN_PORTS.len());
    }

    // allow_install=false（扫描发现路径）行为验证：
    // apk_path 指向不存在的文件——若实现把 ensure_apk_installed 挪到探活/等待之前，
    // 错误会变成特征文案"桥接 APK 未安装且本地不存在"，本测试即失败
    #[tokio::test]
    async fn try_adb_device_scan_path_fails_without_install() {
        let cfg = BridgeConfig::from_map(&map(&[
            ("QUANTUMTV_BRIDGE_APK", "Z:/no-such-dir/bridge.apk"),
        ]));
        let adb = Path::new("Z:/definitely-not-adb/platform-tools/adb.exe");
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        let r = try_adb_device(adb, "127.0.0.1:5555", &cfg, deadline, false).await;
        let msg = r.expect_err("无 adb/设备时扫描路径必须返回 Err");
        assert!(
            !msg.contains("桥接 APK 未安装") && !msg.contains("install"),
            "扫描路径不应触及安装阶段: {}",
            msg
        );
        assert!(msg.contains("执行 adb 失败"), "应失败于 wait_boot 的 adb 调用: {}", msg);
    }
}
