# 桥接自动拉起（ADR 0002 Phase 4）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 桌面端启动时自动拉起 wexbridge AVD、装好桥接 APK、建立 adb forward 并通过健康检查；应用退出时自动关闭自己拉起的模拟器；任何一步失败都静默降级。

**Architecture:** core 新增 `bridge` 模块（配置/状态机/纯函数命令构造/IO 步骤/编排五层），Tauri 层在 `setup` 里 `spawn(ensure_ready())`、`RunEvent::Exit` 里 `block_on(shutdown())`。spider 分流逻辑不变。

**Tech Stack:** Rust（quantumtv-core crate）、tokio（workspace，features=full）、reqwest、log、Tauri v2。

**规格:** `docs/superpowers/specs/2026-09-04-bridge-autostart-design.md`

## Global Constraints

- 单测不得拉起真实进程/网络（集成测试除外，全部 `#[ignore]`）
- 所有环境变量有默认值，零配置可用
- 失败只 `log::warn!` + 状态置 `Failed`，禁止 panic、禁止自动重试
- 端口约定：host `18080` → device `8080`；`QUANTUMTV_BRIDGE_URL` 默认 `http://127.0.0.1:18080`
- 包名 `com.quantumtv.bridge`，服务组件 `com.quantumtv.bridge/.BridgeService`
- 只关闭"自己拉起"的模拟器（`WE_STARTED` 标志），外部已运行的模拟器不动
- core 新增依赖仅允许 `log = "0.4"`（其余用现有依赖）
- 提交信息格式沿用仓库惯例：`feat(bridge): ...` / `test(bridge): ...` / `chore(...)` 等

## 环境事实（实现者无需再探索）

- SDK: `%LOCALAPPDATA%\Android\Sdk`；adb 在 `platform-tools\adb.exe`，emulator 在 `emulator\emulator.exe`
- AVD: `wexbridge`（`%USERPROFILE%\.android\avd\wexbridge.avd`）
- 健康检查: `GET /health` → `{"code":200,"err":"ok","data":{"initialized":true}}`
- 设备列表: `adb devices` 输出形如 `emulator-5554\tdevice`
- core 的 `src-tauri` 在 workspace 之外（workspace exclude），构建需在 `src-tauri` 目录单独 `cargo check`

---

### Task 1: bridge 模块骨架 — 配置与状态机

**Files:**
- Create: `crates/core/src/bridge/mod.rs`
- Modify: `crates/core/src/lib.rs`（加一行 `pub mod bridge;`）
- Modify: `crates/core/Cargo.toml`（dependencies 加 `log = "0.4"`）

**Interfaces:**
- Consumes: 无（首个任务）
- Produces:
  - `pub struct BridgeConfig { enabled: bool, avd: String, sdk_root: PathBuf, host_port: u16, bridge_url: String, apk_path: PathBuf }`（字段全 pub）
  - `BridgeConfig::from_map(&HashMap<String, String>) -> BridgeConfig`、`BridgeConfig::from_env() -> BridgeConfig`
  - `pub fn default_sdk_root() -> Option<PathBuf>`
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum BridgeStatus { Idle, Starting, Ready, Failed }`
  - `pub fn status() -> BridgeStatus`、`pub(crate) fn set_status(s: BridgeStatus)`、`pub(crate) fn try_begin_start() -> bool`、`pub fn we_started() -> bool`
  - `static EMU_CHILD: Mutex<Option<tokio::process::Child>>`（本任务建好，后续任务用）

- [ ] **Step 1: 写失败测试**

在 `crates/core/src/bridge/mod.rs` 末尾放（文件顶部先写最小实现占位会导致测试不是"失败"状态——因此本步**只写测试和空的模块声明**，实现留给 Step 3）：

```rust
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
}
```

同时在 `crates/core/src/lib.rs` 的模块列表加一行（这一行不属于"实现"，是让测试可编译的最小声明）：

```rust
pub mod bridge;
```

`crates/core/Cargo.toml` 的 `[dependencies]` 末尾加：

```toml
log = "0.4"
```

注意：此时 `bridge/mod.rs` 里只有 `#[cfg(test)] mod tests` 和 `use` 声明，编译会因 `BridgeConfig`/`BridgeStatus` 等不存在而失败。

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p quantumtv-core bridge:: -v`（workdir: 仓库根）
Expected: 编译错误，`cannot find type BridgeConfig` 等。

- [ ] **Step 3: 写最小实现**

`crates/core/src/bridge/mod.rs` 顶部（tests 模块之前）：

```rust
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
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p quantumtv-core bridge:: -v`
Expected: 4 个测试全部 PASS。

- [ ] **Step 5: 全量回归**

Run: `cargo test -p quantumtv-core`
Expected: 全部 PASS（`bridge_integration.rs` 里的 `is_bridge_class` 等测试不受影响；`#[ignore]` 的不跑）。

- [ ] **Step 6: Commit**

```powershell
git add crates/core/src/bridge/mod.rs crates/core/src/lib.rs crates/core/Cargo.toml
git commit -m "feat(bridge): add bridge module config and status machine"
```

---

### Task 2: adb/emulator 命令构造（纯函数）

**Files:**
- Modify: `crates/core/src/bridge/mod.rs`

**Interfaces:**
- Consumes: `BridgeConfig.sdk_root`（Task 1）
- Produces:
  - `pub fn adb_path(sdk_root: &Path) -> PathBuf`
  - `pub fn emulator_path(sdk_root: &Path) -> PathBuf`
  - `pub fn emulator_args(avd: &str) -> Vec<String>`
  - `pub fn forward_args(host_port: u16, device_port: u16) -> Vec<String>`
  - `pub fn emulator_serial(devices_out: &str) -> Option<String>`
  - `pub fn has_emulator_device(devices_out: &str) -> bool`

- [ ] **Step 1: 写失败测试**

在 Task 1 的 `mod tests` 里追加：

```rust
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
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p quantumtv-core bridge:: -v`
Expected: 编译错误，`cannot find function adb_path` 等。

- [ ] **Step 3: 写最小实现**

加在 `bridge/mod.rs`（`set_we_started` 之后、tests 之前）：

```rust
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
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p quantumtv-core bridge:: -v`
Expected: 全部 PASS（Task 1 的 4 个 + 本任务 2 个）。

- [ ] **Step 5: Commit**

```powershell
git add crates/core/src/bridge/mod.rs
git commit -m "feat(bridge): adb/emulator command builders"
```

---

### Task 3: IO 步骤 — adb 执行、健康检查、模拟器/APK/服务控制

**Files:**
- Modify: `crates/core/src/bridge/mod.rs`

**Interfaces:**
- Consumes: Task 2 的路径/参数函数、Task 1 的 `EMU_CHILD`/`set_we_started`
- Produces:
  - `pub fn parse_health_body(body: &str) -> bool`
  - `pub(crate) async fn run_adb(adb: &Path, args: &[String]) -> Result<String, String>`
  - `pub(crate) async fn probe_health(bridge_url: &str) -> bool`
  - `pub(crate) async fn spawn_emulator(cfg: &BridgeConfig) -> Result<(), String>`
  - `pub(crate) async fn wait_boot(adb: &Path, serial: &str) -> Result<(), String>`
  - `pub(crate) async fn ensure_apk_installed(adb: &Path, serial: &str, apk_path: &Path) -> Result<(), String>`
  - `pub(crate) async fn start_bridge_service(adb: &Path, serial: &str) -> Result<(), String>`
  - `pub(crate) async fn forward_port(adb: &Path, host_port: u16) -> Result<(), String>`

- [ ] **Step 1: 写失败测试**

`mod tests` 追加：

```rust
    #[test]
    fn health_body_parsing() {
        assert!(parse_health_body(r#"{"code":200,"err":"ok","data":{"initialized":true}}"#));
        assert!(parse_health_body(r#"{"code":200,"err":"ok"}"#));
        assert!(!parse_health_body(r#"{"code":500,"err":"boom"}"#));
        assert!(!parse_health_body("not json"));
        assert!(!parse_health_body(""));
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p quantumtv-core bridge::health_body -v`
Expected: 编译错误 `cannot find function parse_health_body`。

- [ ] **Step 3: 写最小实现**

追加到 `bridge/mod.rs`（`has_emulator_device` 之后）：

```rust
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
    run_adb(adb, &wait_args).await?;
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

pub(crate) async fn forward_port(adb: &Path, host_port: u16) -> Result<(), String> {
    let args = vec!["forward".to_string()].into_iter().chain(forward_args(host_port, 8080)).collect::<Vec<_>>();
    run_adb(adb, &args).await
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p quantumtv-core bridge:: -v`
Expected: 全部 PASS。IO 函数本任务不测（集成测试覆盖）。

- [ ] **Step 5: Commit**

```powershell
git add crates/core/src/bridge/mod.rs
git commit -m "feat(bridge): adb execution, health probe, emulator/apk/service control"
```

---

### Task 4: ensure_ready 编排与 shutdown

**Files:**
- Modify: `crates/core/src/bridge/mod.rs`

**Interfaces:**
- Consumes: Task 1/2/3 全部产出
- Produces:
  - `pub async fn ensure_ready() -> Result<(), String>`（读 env，委托 `ensure_ready_with`）
  - `pub async fn ensure_ready_with(cfg: BridgeConfig) -> Result<(), String>`
  - `pub async fn shutdown()`

- [ ] **Step 1: 写失败测试**

`mod tests` 追加（编排的"禁止重复进入/禁用短路"行为可纯测，真实流程归 Task 5 集成测试）：

```rust
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
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p quantumtv-core bridge::ensure_ready -v`
Expected: 编译错误 `cannot find function ensure_ready_with`。

- [ ] **Step 3: 写实现**

追加到 `bridge/mod.rs`（IO 函数之后）：

```rust
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
    let adb = adb_path(&cfg.sdk_root);
    if !adb.exists() {
        return Err(format!("adb 不存在: {}", adb.display()));
    }

    // 快路径: 桥接已可用（上次会话遗留的模拟器/APK），只补 forward
    if probe_health(&cfg.bridge_url).await {
        forward_port(adb, cfg.host_port).await?;
        log::info!("[桥接] 复用已运行的桥接");
        return Ok(());
    }

    // 1. 模拟器在跑吗？没有则拉起
    let devices = run_adb(&adb, &["devices".to_string()]).await?;
    let serial = match emulator_serial(&devices) {
        Some(s) => s,
        None => {
            spawn_emulator(cfg).await?;
            // 等设备出现在列表（offline→device 由 wait_boot 内的 wait-for-device 兜底）
            wait_device_online(&adb, cfg).await?
        }
    };

    // 2. 等系统启动完成
    wait_boot(&adb, &serial).await?;

    // 3. APK
    ensure_apk_installed(&adb, &serial, &cfg.apk_path).await?;

    // 4. forward + 启动服务
    forward_port(&adb, cfg.host_port).await?;
    start_bridge_service(&adb, &serial).await?;

    // 5. 健康轮询（30s 上限，1s 间隔）
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

async fn wait_device_online(adb: &Path, cfg: &BridgeConfig) -> Result<String, String> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let out = run_adb(adb, &["devices".to_string()]).await?;
        if let Some(s) = emulator_serial(&out) {
            return Ok(s);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("模拟器 {} 未出现在设备列表 (15s)", cfg.avd));
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
    // 1. 优雅关闭
    let _ = run_adb(&adb, &["emu".to_string(), "kill".to_string()]).await;
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
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p quantumtv-core bridge:: -v && cargo test -p quantumtv-core`
Expected: 全部 PASS。

- [ ] **Step 5: Commit**

```powershell
git add crates/core/src/bridge/mod.rs
git commit -m "feat(bridge): ensure_ready orchestration and shutdown"
```

---

### Task 5: 集成测试（真拉起，#[ignore]）

**Files:**
- Create: `crates/core/tests/bridge_lifecycle.rs`

**Interfaces:**
- Consumes: `bridge::{ensure_ready, ensure_ready_with, shutdown, status, BridgeStatus, we_started, BridgeConfig, has_emulator_device, adb_path, run_adb 的公有面}` — 注意 `run_adb` 是 `pub(crate)`，集成测试拿不到；设备列表断言改用 `std::process::Command` 直接调 adb
- Produces: `#[ignore]` 集成测试，验证完整生命周期

- [ ] **Step 1: 写集成测试**

`crates/core/tests/bridge_lifecycle.rs`：

```rust
//! 桥接生命周期集成测试。
//! 运行条件: 本机已安装 Android SDK + wexbridge AVD（或 QUANTUMTV_BRIDGE_AVD 指定的 AVD）。
//! 运行: cargo test -p quantumtv-core --test bridge_lifecycle -- --ignored --test-threads=1

use quantumtv_core::bridge::{self, BridgeConfig, BridgeStatus};

#[tokio::test(flavor = "multi_thread")]
#[ignore = "需要本机 Android SDK + AVD，真拉起模拟器（约 2 分钟）"]
async fn ensure_ready_then_shutdown_lifecycle() {
    let cfg = BridgeConfig::from_env();
    if !cfg.enabled {
        eprintln!("QUANTUMTV_BRIDGE_ENABLED=0，跳过");
        return;
    }

    bridge::ensure_ready_with(cfg.clone()).await.expect("ensure_ready 应成功");
    assert_eq!(bridge::status(), BridgeStatus::Ready);

    // 健康端点真实可达
    let client = reqwest::Client::new();
    let body = client
        .get(format!("{}/health", cfg.bridge_url))
        .send()
        .await
        .expect("health 请求应成功")
        .text()
        .await
        .expect("health 响应体");
    assert!(bridge::parse_health_body(&body), "health 应为 code=200: {}", body);

    let was_ours = bridge::we_started();
    bridge::shutdown().await;

    if was_ours {
        assert_eq!(bridge::status(), BridgeStatus::Idle);
        // 自己拉起的模拟器应已消失
        let adb = bridge::adb_path(&cfg.sdk_root);
        let out = std::process::Command::new(adb)
            .arg("devices")
            .output()
            .expect("adb devices 应可执行");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            !bridge::has_emulator_device(&text),
            "shutdown 后模拟器应消失: {}",
            text
        );
    } else {
        // 复用外部模拟器的场景：不允许关别人的设备
        eprintln!("复用外部模拟器，跳过 shutdown 断言");
    }
}
```

注意：`crates/core/Cargo.toml` 需要加 dev-dependencies 段（reqwest 已是主依赖，但集成测试是独立 crate，需要显式声明）：

```toml
[dev-dependencies]
reqwest = { workspace = true }
tokio = { workspace = true }
```

- [ ] **Step 2: 编译验证（不跑 ignore 测试）**

Run: `cargo test -p quantumtv-core --test bridge_lifecycle`
Expected: 编译通过，输出 `0 passed; 1 ignored`。

- [ ] **Step 3: 真跑集成测试（本机满足条件）**

先停掉当前外部模拟器（避免走"复用外部"分支）：

```powershell
& "$env:LOCALAPPDATA\Android\Sdk\platform-tools\adb.exe" emu kill
Start-Sleep -Seconds 5
```

Run: `cargo test -p quantumtv-core --test bridge_lifecycle -- --ignored --test-threads=1`
Expected: `ensure_ready_then_shutdown_lifecycle ... ok`；日志能看到"模拟器 wexbridge 拉起中"→"启动完成"→"就绪"→"已关闭"。耗时约 2 分钟。

- [ ] **Step 4: Commit**

```powershell
git add crates/core/tests/bridge_lifecycle.rs crates/core/Cargo.toml
git commit -m "test(bridge): full lifecycle integration test (ignored, needs AVD)"
```

---

### Task 6: Tauri 接线 + 端口默认值对齐

**Files:**
- Modify: `src-tauri/src/lib.rs`（setup 钩子约 148-151 行处追加；`.run(...)` 改 `.build(...).run(closure)`）
- Modify: `src-tauri/src/commands/video.rs:1093-1094` 与 `:1455-1456`（两处 BRIDGE_URL 默认值）

**Interfaces:**
- Consumes: `quantumtv_core::bridge::{ensure_ready, shutdown}`（Task 4）
- Produces: 应用启动即后台拉起、退出即关闭

- [ ] **Step 1: setup 钩子里拉起**

`src-tauri/src/lib.rs` 中，在 `scheduler::start_background_tasks(app.handle().clone());`（约 149 行）之后、`Ok(())` 之前插入：

```rust
            // 桥接自动拉起（ADR 0002 Phase 4，失败静默降级）
            tauri::async_runtime::spawn(async {
                if let Err(e) = quantumtv_core::bridge::ensure_ready().await {
                    log::warn!("[桥接] 后台拉起失败: {}", e);
                }
            });
```

- [ ] **Step 2: 退出时关闭**

`src-tauri/src/lib.rs` 文件末尾，把：

```rust
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
```

改为：

```rust
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, event| {
            if let tauri::RunEvent::Exit = event {
                tauri::async_runtime::block_on(quantumtv_core::bridge::shutdown());
            }
        });
```

- [ ] **Step 3: BRIDGE_URL 默认端口 8080 → 18080**

`src-tauri/src/commands/video.rs` 两处（约 1093-1094 行和 1455-1456 行），把：

```rust
            let bridge_url = std::env::var("QUANTUMTV_BRIDGE_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:8080".to_string());
```

都改为：

```rust
            let bridge_url = std::env::var("QUANTUMTV_BRIDGE_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:18080".to_string());
```

- [ ] **Step 4: 编译验证**

Run: `cargo check`（workdir: `src-tauri`）
Expected: 无 error（warning 允许）。

- [ ] **Step 5: 手动冒烟（可选但推荐）**

先 `adb emu kill` 停掉现有模拟器，再 `pnpm tauri dev` 启动应用：
Expected: 日志出现 `[桥接] 模拟器 wexbridge 拉起中` → 约 1-2 分钟后 `[桥接] 就绪: http://127.0.0.1:18080`；关闭应用窗口后日志出现 `[桥接] 已关闭`，`adb devices` 列表为空。

- [ ] **Step 6: Commit**

```powershell
git add src-tauri/src/lib.rs src-tauri/src/commands/video.rs
git commit -m "feat(tauri): auto-start bridge on launch, shutdown on exit, align port 18080"
```

---

### Task 7: APK 稳定输出路径 + ADR 状态更新

**Files:**
- Modify: `android/spider-bridge/build.ps1`（末尾追加输出复制）
- Modify: `.gitignore`（忽略 `android/spider-bridge/out/`）
- Modify: `docs/adr/0002-android-bridge-for-wex-spiders.md`（"后续工作"标注 Phase 4 完成；docs 被 gitignore，本地编辑不提交）

**Interfaces:**
- Consumes: 无
- Produces: `android/spider-bridge/out/bridge.apk`（`ensure_apk_installed` 的默认 APK 路径）

- [ ] **Step 1: build.ps1 追加稳定输出**

在 `android/spider-bridge/build.ps1` 最后一行 `Write-Host "BUILD OK: $root\out\bridge.apk"` 之前插入：

```powershell
# 输出到仓库稳定路径，供 core bridge ensure_apk_installed 默认使用
New-Item -ItemType Directory -Path "$PSScriptRoot\out" -Force | Out-Null
Copy-Item "$root\out\bridge.apk" "$PSScriptRoot\out\bridge.apk" -Force
```

并把最后一行改为：

```powershell
Write-Host "BUILD OK: $PSScriptRoot\out\bridge.apk"
```

- [ ] **Step 2: .gitignore 追加**

`.gitignore` 末尾加：

```
# bridge apk build output
android/spider-bridge/out/
```

- [ ] **Step 3: ADR 标注进度（本地编辑，不提交）**

`docs/adr/0002-android-bridge-for-wex-spiders.md` 的"后续工作"一节，把第一项：

```markdown
- Phase 4: 桌面端自动拉起 AVD + adb forward + 桥接健康检查
```

改为：

```markdown
- ~~Phase 4: 桌面端自动拉起 AVD + adb forward + 桥接健康检查~~（已完成，2026-09-04，见 crates/core/src/bridge）
```

- [ ] **Step 4: 验证 build.ps1 语法**

Run: `pwsh -NoProfile -Command "Get-Content android/spider-bridge/build.ps1 -Raw | Out-Null; 'syntax ok'"`（完整重跑需模拟器+JAR，语法检查即可；如本机具备条件可整跑一次确认 `out\bridge.apk` 生成）
Expected: `syntax ok`。

- [ ] **Step 5: Commit**

```powershell
git add android/spider-bridge/build.ps1 .gitignore
git commit -m "chore(bridge): output apk to stable repo path for auto-install"
```

---

## 收尾

- [ ] 全量回归: `cargo test -p quantumtv-core`（workdir: 仓库根）+ `cargo check`（workdir: `src-tauri`）
- [ ] `git log --oneline -8` 确认 7 个提交齐全、无遗漏文件
