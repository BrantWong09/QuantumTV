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
}

const ENV_KEYS: [&str; 6] = [
    "QUANTUMTV_BRIDGE_ENABLED",
    "QUANTUMTV_BRIDGE_AVD",
    "QUANTUMTV_BRIDGE_SDK",
    "QUANTUMTV_ADB_HOST_PORT",
    "QUANTUMTV_BRIDGE_URL",
    "QUANTUMTV_BRIDGE_APK",
];

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
        let bridge_url = m
            .get("QUANTUMTV_BRIDGE_URL")
            .cloned()
            .unwrap_or_else(|| format!("http://127.0.0.1:{}", host_port));
        let apk_path = m
            .get("QUANTUMTV_BRIDGE_APK")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("android/spider-bridge/out/bridge.apk"));
        BridgeConfig { enabled, avd, sdk_root, host_port, bridge_url, apk_path }
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

pub fn emulator_args(avd: &str) -> Vec<String> {
    vec![
        "-avd".to_string(),
        avd.to_string(),
        "-no-snapshot-save".to_string(),
        "-no-boot-anim".to_string(),
        "-gpu".to_string(),
        "auto".to_string(),
    ]
}

pub fn forward_args(host_port: u16, device_port: u16) -> Vec<String> {
    vec![format!("tcp:{}", host_port), format!("tcp:{}", device_port)]
}

/// 从 `adb devices` 输出提取处于 device 状态的模拟器 serial
pub fn emulator_serial(devices_out: &str) -> Option<String> {
    devices_out
        .lines()
        .skip(1) // 跳过 "List of devices attached"
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let serial = it.next()?;
            let state = it.next()?;
            (serial.starts_with("emulator-") && state == "device").then(|| serial.to_string())
        })
        .next()
}

pub fn has_emulator_device(devices_out: &str) -> bool {
    emulator_serial(devices_out).is_some()
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
        assert_eq!(cfg.bridge_url, "http://127.0.0.1:1234"); // 显式 URL 不被端口拼接覆盖
        assert_eq!(cfg.apk_path, PathBuf::from("C:/bridge.apk"));
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
        assert_eq!(
            emulator_args("wexbridge"),
            vec![
                "-avd".to_string(),
                "wexbridge".to_string(),
                "-no-snapshot-save".to_string(),
                "-no-boot-anim".to_string(),
                "-gpu".to_string(),
                "auto".to_string(),
            ]
        );
        assert_eq!(forward_args(18080, 8080), vec!["tcp:18080".to_string(), "tcp:8080".to_string()]);
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
}
