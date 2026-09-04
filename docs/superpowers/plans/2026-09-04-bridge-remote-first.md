# 桥接远程优先 + 第三方模拟器 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 桥接启动改为"远程真机直连 → 第三方模拟器 adb connect → 官方 AVD"三阶段瀑布，生效 URL 进 core 全局状态，桥接配置迁入 admin UI（env 兜底）。

**Architecture:** core（`crates/core/src/bridge/mod.rs`）承载瀑布编排与决策纯函数；src-tauri 新增 `commands/bridge.rs` 四命令负责配置持久化（`data.json` 的 `BridgeConfig` 键）与状态查询；`video.rs` 两处桥接调用改读 `effective_url()`；前端在 admin 页新增"桥接设置"折叠区。

**Tech Stack:** Rust (tokio, reqwest, serde) / Tauri v2 / Next.js + TypeScript + Tailwind

**Spec:** `docs/superpowers/specs/2026-09-04-bridge-remote-first-design.md`

## Global Constraints

- 核心库包名 `quantumtv-core`（根 workspace）；桌面端包名 `quantumtv`（独立 workspace，目录 `src-tauri/`，自带 Cargo.lock）
- 测试命令：core 用 `cargo test -p quantumtv-core`（workdir=仓库根）；桌面端用 `cargo check`（workdir=src-tauri）；前端用 `npm run lint`、`npm run typecheck`
- 提交信息沿用仓库惯例：`feat(bridge): ...` / `feat(tauri): ...` / `feat(ui): ...`，中文正文不强制
- 注释风格：跟随现有代码，关键逻辑用简短中文注释
- `/docs` 在 .gitignore 中，新增文档需 `git add -f`
- 现有 31 个 core 测试不得回归；`#[ignore]` 的集成测试（bridge_lifecycle/bridge_integration）需通过编译
- 不改桥接 APK；不加鉴权

---

### Task 1: core — BridgeConfig 扩展（remote_url / adb_addresses / auto_scan / url_override 语义拆分）

**Files:**
- Modify: `crates/core/src/bridge/mod.rs`（BridgeConfig 结构体 ~第 7-14 行、from_map ~第 26-56 行、tests 模块）

**Interfaces:**
- Consumes: 现有 `BridgeConfig::from_map(&HashMap<String, String>)`
- Produces:
  - `BridgeConfig` 新字段：`url_override: Option<String>`（env `QUANTUMTV_BRIDGE_URL`，Phase A 首候选）、`remote_url: Option<String>`（`QUANTUMTV_BRIDGE_REMOTE_URL`）、`adb_addresses: Vec<String>`（`QUANTUMTV_BRIDGE_ADB_ADDRESSES`，逗号/换行分隔）、`auto_scan: bool`（`QUANTUMTV_BRIDGE_AUTO_SCAN`，"0"=false，默认 true）
  - `bridge_url` 语义收窄：恒为本地 forward URL `http://127.0.0.1:{host_port}`，不再被 env 覆盖
  - `fn normalize_addr_list(s: &str) -> Vec<String>`（私有，Task 6 的 UI 归一逻辑复用同一解析语义）

- [ ] **Step 1: 写失败测试**

在 `crates/core/src/bridge/mod.rs` 的 `mod tests` 中，修改 `config_env_overrides` 并新增两个测试：

```rust
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
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p quantumtv-core bridge::tests::config`
Expected: FAIL（`url_override`/`remote_url` 等字段不存在，编译错误）

- [ ] **Step 3: 最小实现**

BridgeConfig 结构体（约第 7-14 行）追加字段：

```rust
#[derive(Debug, Clone)]
pub struct BridgeConfig {
    pub enabled: bool,
    pub avd: String,
    pub sdk_root: PathBuf,
    pub host_port: u16,
    pub bridge_url: String,
    pub apk_path: PathBuf,
    pub url_override: Option<String>,
    pub remote_url: Option<String>,
    pub adb_addresses: Vec<String>,
    pub auto_scan: bool,
}
```

`normalize_addr_list` 纯函数（放在 `from_map` 上方）：

```rust
/// 逗号/换行分隔的地址串 → 去空白、去空的地址数组
fn normalize_addr_list(s: &str) -> Vec<String> {
    s.split([',', '\n'])
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty())
        .collect()
}
```

`from_map` 改造（保留现有 enabled/avd/sdk_root/host_port/apk_path 逻辑；`ENV_KEYS` 数组追加三个新键）：

```rust
        let url_override = m
            .get("QUANTUMTV_BRIDGE_URL")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
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
        BridgeConfig { enabled, avd, sdk_root, host_port, bridge_url, apk_path, url_override, remote_url, adb_addresses, auto_scan }
```

`const ENV_KEYS: [&str; 6]` 改为 `const ENV_KEYS: [&str; 9]`，追加 `"QUANTUMTV_BRIDGE_REMOTE_URL"`, `"QUANTUMTV_BRIDGE_ADB_ADDRESSES"`, `"QUANTUMTV_BRIDGE_AUTO_SCAN"`。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p quantumtv-core bridge`
Expected: PASS（全部 bridge 模块测试）

- [ ] **Step 5: 提交**

```bash
git add crates/core/src/bridge/mod.rs
git commit -m "feat(bridge): extend BridgeConfig with remote/adb/autoscan keys"
```

---

### Task 2: core — TCP serial 解析 + 扫描端口表

**Files:**
- Modify: `crates/core/src/bridge/mod.rs`（emulator_serials 附近 + tests 模块）

**Interfaces:**
- Produces:
  - `pub const SCAN_PORTS: &[u16]`（LDPlayer/Nox/MuMu/WSA 常见端口）
  - `pub fn connectable_serials(devices_out: &str) -> Vec<String>`（从 `adb devices` 输出解析 `host:port` 形式且 state=device 的 serial；Task 5 的 `wait_serial_device` 依赖）

- [ ] **Step 1: 写失败测试**

```rust
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
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p quantumtv-core bridge::tests::connectable`
Expected: FAIL（`connectable_serials`/`SCAN_PORTS` 未定义）

- [ ] **Step 3: 最小实现**

放在 `emulator_serials` 函数后：

```rust
/// 第三方模拟器/WSA 常见 TCP adb 端口（LDPlayer/Nox/MuMu/WSA）
pub const SCAN_PORTS: &[u16] = &[5555, 5557, 62001, 62025, 62026, 62027, 16384, 16416, 16448, 58526];

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
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p quantumtv-core bridge`
Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add crates/core/src/bridge/mod.rs
git commit -m "feat(bridge): tcp serial parsing and emulator scan ports"
```

---

### Task 3: core — 生效 URL 状态（effective URL / kind）

**Files:**
- Modify: `crates/core/src/bridge/mod.rs`（STATUS 静态区 ~第 90-95 行 + tests 模块）

**Interfaces:**
- Produces（video.rs 与 Tauri 状态查询依赖）:
  - `pub fn effective_url() -> Option<String>`
  - `pub fn effective_kind() -> u8`（0=none 1=remote 2=emulator 3=avd）
  - `pub(crate) fn set_effective(url: &str, kind: u8)`（Task 5 瀑布内调用）
  - `pub fn reset_effective()`（Task 6 retry 流程调用：清 URL/kind，且非 Starting 时状态归 Idle）
  - 常量 `pub const EFFECTIVE_NONE/REMOTE/EMULATOR/AVD: u8`

- [ ] **Step 1: 写失败测试**

```rust
    #[test]
    fn effective_url_roundtrip_and_reset() {
        set_effective("http://192.168.1.20:8080", EFFECTIVE_REMOTE);
        assert_eq!(effective_url().as_deref(), Some("http://192.168.1.20:8080"));
        assert_eq!(effective_kind(), EFFECTIVE_REMOTE);
        set_effective("http://127.0.0.1:18080", EFFECTIVE_EMULATOR);
        assert_eq!(effective_kind(), EFFECTIVE_EMULATOR);
        reset_effective();
        assert_eq!(effective_url(), None);
        assert_eq!(effective_kind(), EFFECTIVE_NONE);
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p quantumtv-core bridge::tests::effective`
Expected: FAIL（函数未定义）

- [ ] **Step 3: 最小实现**

在 `pub(crate) static STARTED_SERIAL` 之后添加：

```rust
/// 解析成功的生效桥接地址与其来源（Phase A/B/C）
static EFFECTIVE_URL: Mutex<Option<String>> = Mutex::new(None);
static EFFECTIVE_KIND: AtomicU8 = AtomicU8::new(EFFECTIVE_NONE);

pub const EFFECTIVE_NONE: u8 = 0;
pub const EFFECTIVE_REMOTE: u8 = 1;
pub const EFFECTIVE_EMULATOR: u8 = 2;
pub const EFFECTIVE_AVD: u8 = 3;

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

/// 清空生效 URL/kind；非 Starting 时状态归 Idle（供手动重试前复位）
pub fn reset_effective() {
    if let Ok(mut g) = EFFECTIVE_URL.lock() {
        *g = None;
    }
    EFFECTIVE_KIND.store(EFFECTIVE_NONE, Ordering::SeqCst);
    if status() != BridgeStatus::Starting {
        set_status(BridgeStatus::Idle);
    }
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p quantumtv-core bridge`
Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add crates/core/src/bridge/mod.rs
git commit -m "feat(bridge): effective url state with kind tracking"
```

---

### Task 4: core — 决策纯函数（remote_candidates / phase_b_addresses）

**Files:**
- Modify: `crates/core/src/bridge/mod.rs`（probe_health 附近 + tests 模块）

**Interfaces:**
- Consumes: Task 2 的 `SCAN_PORTS`
- Produces（Task 5 瀑布依赖）:
  - `pub(crate) fn remote_candidates(url_override: Option<&str>, remote_url: Option<&str>) -> Vec<String>`（env 覆盖优先、去空去重）
  - `pub(crate) fn phase_b_addresses(manual: &[String], auto_scan: bool) -> Vec<String>`（手动在前、扫描追加、去重）

- [ ] **Step 1: 写失败测试**

```rust
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
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p quantumtv-core bridge::tests::remote_candidates bridge::tests::phase_b`
Expected: FAIL（函数未定义）

- [ ] **Step 3: 最小实现**

```rust
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
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p quantumtv-core bridge`
Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add crates/core/src/bridge/mod.rs
git commit -m "feat(bridge): phase decision helpers for remote and adb candidates"
```

---

### Task 5: core — startup_steps 三阶段瀑布重写

**Files:**
- Modify: `crates/core/src/bridge/mod.rs`（`startup_steps` ~第 368-424 行整体重写；`forward_port` ~第 336-339 行加 serial 参数；新增 `wait_serial_device`/`try_adb_device`/`ensure_avd_bridge`；tests 模块更新）
- Modify: `crates/core/tests/bridge_lifecycle.rs`（`cfg.bridge_url` → `bridge::effective_url()`）

**Interfaces:**
- Consumes: Task 1-4 全部产物；现有 `probe_health`/`wait_boot`/`ensure_apk_installed`/`start_bridge_service`/`spawn_emulator`/`wait_for_avd_serial`/`serial_matches_avd`/`emulator_serials`
- Produces:
  - `pub(crate) fn forward_cmd_args(serial: &str, host_port: u16) -> Vec<String>`（纯函数，可测）
  - `forward_port(adb: &Path, serial: &str, host_port: u16) -> Result<(), String>`（**签名变更**：加 `-s serial`，多设备共存时不再报错）
  - `async fn wait_serial_device(adb: &Path, serial: &str, secs: u64) -> bool`（轮询 `adb devices` 等待 TCP serial 上线）
  - `async fn try_adb_device(adb: &Path, serial: &str, cfg: &BridgeConfig, deadline: Instant) -> Result<(), String>`
  - `async fn ensure_avd_bridge(cfg: &BridgeConfig, adb: &Path) -> Result<(), String>`（原 Phase C 逻辑原样搬移）

- [ ] **Step 1: 写失败测试（forward 命令参数纯函数断言）**

修改现有 `paths_and_args` 测试（forward_args 纯函数断言保留），新增：

```rust
    #[test]
    fn forward_cmd_args_includes_serial() {
        let args = forward_cmd_args("127.0.0.1:5555", 18080);
        assert_eq!(
            args,
            vec!["-s", "127.0.0.1:5555", "forward", "tcp:18080", "tcp:8080"]
        );
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p quantumtv-core bridge::tests::forward_cmd`
Expected: FAIL（`forward_cmd_args` 未定义，编译错误）

- [ ] **Step 3: 实现 forward_port + 三个辅助函数 + 瀑布重写**

新增纯函数与 `forward_port`（替换原第 336-339 行）：

```rust
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
```

新增三个辅助函数（放在 `serial_matches_avd` 与 `wait_for_avd_serial` 之间）：

```rust
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

/// 单个第三方设备完整链路: 等就绪 → 装 APK → forward → 启动服务 → 健康轮询（受总 deadline 约束）
/// 注意: wait_boot 内部各自有 120s 上限，可能超过 Phase B 总 deadline，
/// 故整段用 tokio::time::timeout 包裹，到点强制转入 Phase C。
async fn try_adb_device(
    adb: &Path,
    serial: &str,
    cfg: &BridgeConfig,
    deadline: tokio::time::Instant,
) -> Result<(), String> {
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    tokio::time::timeout(
        remaining,
        async {
            wait_boot(adb, serial).await?;
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
```

`startup_steps` 整体替换（原"快路径复用"逻辑由 Phase C 的运行中 AVD 复用分支覆盖，删除）：

```rust
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
        // connect 幂等；失败不中断（该地址可能不是 adb 端口）
        let _ = run_adb(&adb, &["connect".to_string(), addr.clone()]).await;
        if !wait_serial_device(&adb, &addr, 5).await {
            continue;
        }
        if try_adb_device(&adb, &addr, cfg, b_deadline).await.is_ok() {
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
            wait_for_avd_serial(adb, cfg).await?
        }
    };
    if we_started() {
        *STARTED_SERIAL.lock().unwrap() = Some(serial.clone());
    }

    wait_boot(adb, &serial).await?;
    ensure_apk_installed(adb, &serial, &cfg.apk_path).await?;
    forward_port(adb, &serial, cfg.host_port).await?;
    start_bridge_service(adb, &serial).await?;

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
```

- [ ] **Step 4: 修正受签名变更影响的既有测试与集成测试**

- `crates/core/tests/bridge_lifecycle.rs` 第 22 行：`format!("{}/health", cfg.bridge_url)` 改为基于 `bridge::effective_url()`：

```rust
    let url = bridge::effective_url().expect("桥接就绪后应有生效 URL");
    let body = client
        .get(format!("{}/health", url))
```

- 全仓搜索 `forward_port(` 确认无其他调用点（当前仅 startup_steps 使用）。

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test -p quantumtv-core`
Expected: PASS（全部，含忽略测试编译通过）；另跑 `cargo test -p quantumtv-core --test bridge_lifecycle` 确认仅因 `#[ignore]` 跳过。

- [ ] **Step 6: 提交**

```bash
git add crates/core/src/bridge/mod.rs crates/core/tests/bridge_lifecycle.rs
git commit -m "feat(bridge): three-phase waterfall startup (remote/emulator/avd)"
```

---

### Task 6: src-tauri — 桥接命令模块 + 注册 + 启动钩子

**Files:**
- Create: `src-tauri/src/commands/bridge.rs`
- Modify: `src-tauri/src/commands/mod.rs`（追加 `pub mod bridge;`）
- Modify: `src-tauri/src/lib.rs`（第 151-156 行启动钩子替换；invoke_handler 数组 ~第 298 行前注册 4 命令）

**Interfaces:**
- Consumes: core 的 `BridgeConfig::from_map/from_env`、`ensure_ready_with`、`status()`、`we_started()`、`shutdown()`、`reset_effective()`、`effective_url()`、`effective_kind()`、`EFFECTIVE_*` 常量；现有 `StorageManager::get_data/update_config`
- Produces（前端依赖的 Tauri 命令）:
  - `get_bridge_config() -> BridgeSettingsDto`（`{ remote_url: String, adb_addresses: String, auto_scan: bool }`，字段蛇形命名）
  - `save_bridge_config(settings: BridgeSettingsDto) -> Result<(), String>`（校验 → 落盘 `config.BridgeConfig` → 后台重试）
  - `get_bridge_status() -> BridgeStatusDto`（`{ status, effective_url, mode }`，mode ∈ `none|remote|emulator|avd`）
  - `retry_bridge() -> Result<(), String>`（Starting 拒绝；否则关旧 AVD、复位、后台重跑）
  - `pub async fn startup_ensure(handle: tauri::AppHandle)`（lib.rs 启动钩子调用）

- [ ] **Step 1: 创建 `src-tauri/src/commands/bridge.rs`**

```rust
use serde::{Deserialize, Serialize};
use tauri::{Manager, State};

use crate::storage::StorageManager;

/// 桥接设置（持久化在 data.json 的 config.BridgeConfig）
/// serde(default) 兼容旧数据缺字段；auto_scan 缺省 true 经字段级 default 处理
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct BridgeSettingsDto {
    pub remote_url: String,
    /// 逗号/换行分隔的 adb connect 地址
    pub adb_addresses: String,
    pub auto_scan: bool,
}

impl Default for BridgeSettingsDto {
    fn default() -> Self {
        Self { remote_url: String::new(), adb_addresses: String::new(), auto_scan: true }
    }
}

#[derive(Debug, Serialize)]
pub struct BridgeStatusDto {
    pub status: String,
    pub effective_url: Option<String>,
    pub mode: String,
}

fn bridge_settings_from_config(config: &serde_json::Value) -> Option<BridgeSettingsDto> {
    config
        .get("BridgeConfig")
        .and_then(|v| serde_json::from_value::<BridgeSettingsDto>(v.clone()).ok())
}

fn validate_settings(s: &BridgeSettingsDto) -> Result<(), String> {
    let remote = s.remote_url.trim();
    if !remote.is_empty() {
        let parsed = url::Url::parse(remote).map_err(|e| format!("远程桥接地址无效: {}", e))?;
        if parsed.scheme() != "http" && parsed.scheme() != "https" {
            return Err("远程桥接地址仅支持 http/https".to_string());
        }
    }
    for addr in s.adb_addresses.split([',', '\n']).map(str::trim).filter(|a| !a.is_empty()) {
        let Some((_host, port)) = addr.rsplit_once(':') else {
            return Err(format!("adb 地址格式无效（应为 host:port）: {}", addr));
        };
        port.parse::<u16>().map_err(|_| format!("adb 端口无效: {}", addr))?;
    }
    Ok(())
}

/// UI 优先、env 兜底合并为 core 的配置 map（UI 三个键始终写入，空值表示显式清除）
fn build_bridge_map(s: &BridgeSettingsDto) -> std::collections::HashMap<String, String> {
    let mut m = std::collections::HashMap::new();
    for k in [
        "QUANTUMTV_BRIDGE_ENABLED",
        "QUANTUMTV_BRIDGE_AVD",
        "QUANTUMTV_BRIDGE_SDK",
        "QUANTUMTV_ADB_HOST_PORT",
        "QUANTUMTV_BRIDGE_URL",
        "QUANTUMTV_BRIDGE_APK",
        "QUANTUMTV_BRIDGE_REMOTE_URL",
        "QUANTUMTV_BRIDGE_ADB_ADDRESSES",
        "QUANTUMTV_BRIDGE_AUTO_SCAN",
    ] {
        if let Ok(v) = std::env::var(k) {
            if !v.is_empty() {
                m.insert(k.to_string(), v);
            }
        }
    }
    let addrs = s
        .adb_addresses
        .split([',', '\n'])
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .collect::<Vec<_>>()
        .join(",");
    m.insert("QUANTUMTV_BRIDGE_REMOTE_URL".to_string(), s.remote_url.trim().to_string());
    m.insert("QUANTUMTV_BRIDGE_ADB_ADDRESSES".to_string(), addrs);
    m.insert("QUANTUMTV_BRIDGE_AUTO_SCAN".to_string(), if s.auto_scan { "1" } else { "0" }.to_string());
    m
}

/// Starting 防重入检查 + 关闭自拉起旧模拟器 + 后台重跑瀑布
fn spawn_retry(settings: BridgeSettingsDto) -> Result<(), String> {
    if quantumtv_core::bridge::status() == quantumtv_core::bridge::BridgeStatus::Starting {
        return Err("桥接正在启动中，请稍后再试".to_string());
    }
    let cfg = quantumtv_core::bridge::BridgeConfig::from_map(&build_bridge_map(&settings));
    tauri::async_runtime::spawn(async move {
        if quantumtv_core::bridge::we_started() {
            quantumtv_core::bridge::shutdown().await;
        }
        quantumtv_core::bridge::reset_effective();
        if let Err(e) = quantumtv_core::bridge::ensure_ready_with(cfg).await {
            log::warn!("[桥接] 桥接重试失败: {}", e);
        }
    });
    Ok(())
}

fn persist_settings(settings: &BridgeSettingsDto, state: &State<'_, StorageManager>) -> Result<(), String> {
    let mut data = state.get_data()?;
    if !data.config.is_object() {
        data.config = serde_json::json!({});
    }
    data.config
        .as_object_mut()
        .unwrap()
        .insert("BridgeConfig".to_string(), serde_json::to_value(settings).map_err(|e| e.to_string())?);
    state.update_config(data.config)
}

/// 获取桥接设置
#[tauri::command]
pub async fn get_bridge_config(state: State<'_, StorageManager>) -> Result<BridgeSettingsDto, String> {
    let data = state.get_data()?;
    Ok(bridge_settings_from_config(&data.config).unwrap_or_default())
}

/// 保存桥接设置并触发后台重连
#[tauri::command]
pub async fn save_bridge_config(
    settings: BridgeSettingsDto,
    state: State<'_, StorageManager>,
) -> Result<(), String> {
    validate_settings(&settings)?;
    persist_settings(&settings, &state)?;
    spawn_retry(settings)
}

/// 桥接状态（供前端轮询）
#[tauri::command]
pub async fn get_bridge_status() -> BridgeStatusDto {
    let status = match quantumtv_core::bridge::status() {
        quantumtv_core::bridge::BridgeStatus::Idle => "idle",
        quantumtv_core::bridge::BridgeStatus::Starting => "starting",
        quantumtv_core::bridge::BridgeStatus::Ready => "ready",
        quantumtv_core::bridge::BridgeStatus::Failed => "failed",
    };
    let mode = match quantumtv_core::bridge::effective_kind() {
        quantumtv_core::bridge::EFFECTIVE_REMOTE => "remote",
        quantumtv_core::bridge::EFFECTIVE_EMULATOR => "emulator",
        quantumtv_core::bridge::EFFECTIVE_AVD => "avd",
        _ => "none",
    };
    BridgeStatusDto {
        status: status.to_string(),
        effective_url: quantumtv_core::bridge::effective_url(),
        mode: mode.to_string(),
    }
}

/// 手动重试（不改动已保存配置）
#[tauri::command]
pub async fn retry_bridge(state: State<'_, StorageManager>) -> Result<(), String> {
    let settings = {
        let data = state.get_data()?;
        bridge_settings_from_config(&data.config).unwrap_or_default()
    };
    spawn_retry(settings)
}

/// 应用启动钩子: UI 配置存在则 UI 优先合并 env，否则纯 env（向后兼容）
pub async fn startup_ensure(handle: tauri::AppHandle) {
    let settings = {
        let storage = handle.state::<StorageManager>();
        storage.get_data().ok().and_then(|d| bridge_settings_from_config(&d.config))
    };
    let cfg = match settings {
        Some(s) => quantumtv_core::bridge::BridgeConfig::from_map(&build_bridge_map(&s)),
        None => quantumtv_core::bridge::BridgeConfig::from_env(),
    };
    if let Err(e) = quantumtv_core::bridge::ensure_ready_with(cfg).await {
        log::warn!("[桥接] 后台拉起失败: {}", e);
    }
}
```

- [ ] **Step 2: 注册模块与命令**

`src-tauri/src/commands/mod.rs` 追加一行：

```rust
pub mod bridge;
```

`src-tauri/src/lib.rs` 第 151-156 行替换为：

```rust
            // 桥接自动拉起（远程优先，失败降级模拟器；失败静默降级）
            let bridge_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                commands::bridge::startup_ensure(bridge_handle).await;
            });
```

invoke_handler 数组（`commands::search::get_search_cache_stats,` 之后）追加：

```rust
            commands::bridge::get_bridge_config,
            commands::bridge::save_bridge_config,
            commands::bridge::get_bridge_status,
            commands::bridge::retry_bridge,
```

- [ ] **Step 3: 编译验证**

Run: `cargo check`（workdir=`src-tauri`）
Expected: 无错误（warning 可接受）

- [ ] **Step 4: 回归 core 测试**

Run: `cargo test -p quantumtv-core`（workdir=仓库根）
Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/commands/bridge.rs src-tauri/src/commands/mod.rs src-tauri/src/lib.rs
git commit -m "feat(tauri): bridge settings commands, status query and startup hook"
```

---

### Task 7: src-tauri — video.rs 消费 effective_url

**Files:**
- Modify: `src-tauri/src/commands/video.rs`（`search_site_results` ~第 1094-1098 行、`fetch_detail_item` ~第 1460-1464 行）

**Interfaces:**
- Consumes: core `quantumtv_core::bridge::effective_url() -> Option<String>`

- [ ] **Step 1: 修改 search_site_results 桥接分支（约 1094 行）**

```rust
        let items = if quantumtv_core::spider::is_bridge_class(class_name) {
            // wex Guard 类: 走 Android 桥接 (含 OLLVM/DexNative 保护, JVM 无法加载)
            let Some(bridge_url) = quantumtv_core::bridge::effective_url() else {
                return Err("桥接未就绪".to_string());
            };
            quantumtv_core::spider::spider_bridge_search(class_name, query, &bridge_url).await?
        } else {
```

- [ ] **Step 2: 修改 fetch_detail_item 桥接分支（约 1460 行）**

```rust
        let item = if quantumtv_core::spider::is_bridge_class(class_name) {
            // wex Guard 类: 走 Android 桥接
            let Some(bridge_url) = quantumtv_core::bridge::effective_url() else {
                return Err("桥接未就绪".to_string());
            };
            quantumtv_core::spider::spider_bridge_detail(class_name, id, &bridge_url).await?
        } else {
```

- [ ] **Step 3: 确认无残留环境变量读取**

Run: `grep -n "QUANTUMTV_BRIDGE_URL" src-tauri/src/commands/video.rs`
Expected: 无输出（该 env 读取已全部移除；lib.rs/commands/bridge.rs 中允许存在）

- [ ] **Step 4: 编译与回归**

Run: `cargo check`（workdir=`src-tauri`）；`cargo test -p quantumtv-core`（workdir=仓库根）
Expected: 均通过

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/commands/video.rs
git commit -m "feat(tauri): consume bridge effective_url for wex sites"
```

---

### Task 8: 前端 — admin 页"桥接设置"折叠区

**Files:**
- Create: `src/components/BridgeSettings.tsx`
- Modify: `src/app/admin/page.tsx`（import 区、expandedTabs ~第 1436 行、折叠区列表 ~第 1549 行后插入）

**Interfaces:**
- Consumes: Task 6 的 4 个命令（参数/返回为蛇形命名 JSON）
- Produces: `<BridgeSettings showAlert={showAlert} />` 组件

- [ ] **Step 1: 创建 `src/components/BridgeSettings.tsx`**

```tsx
'use client';

import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';

interface BridgeSettingsDto {
  remote_url: string;
  adb_addresses: string;
  auto_scan: boolean;
}

interface BridgeStatusDto {
  status: 'idle' | 'starting' | 'ready' | 'failed';
  effective_url: string | null;
  mode: 'none' | 'remote' | 'emulator' | 'avd';
}

const STATUS_LABEL: Record<BridgeStatusDto['status'], string> = {
  idle: '未启动',
  starting: '启动中',
  ready: '就绪',
  failed: '失败',
};

const STATUS_COLOR: Record<BridgeStatusDto['status'], string> = {
  idle: 'bg-gray-400',
  starting: 'bg-yellow-500',
  ready: 'bg-green-500',
  failed: 'bg-red-500',
};

const MODE_LABEL: Record<BridgeStatusDto['mode'], string> = {
  none: '—',
  remote: '远程真机',
  emulator: '第三方模拟器',
  avd: '官方模拟器',
};

export default function BridgeSettings({
  showAlert,
}: {
  showAlert: (
    type: 'success' | 'error' | 'warning',
    title: string,
    message?: string,
  ) => void;
}) {
  const [settings, setSettings] = useState<BridgeSettingsDto>({
    remote_url: '',
    adb_addresses: '',
    auto_scan: true,
  });
  const [status, setStatus] = useState<BridgeStatusDto | null>(null);
  const [saving, setSaving] = useState(false);
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const refreshStatus = useCallback(async () => {
    try {
      setStatus(await invoke<BridgeStatusDto>('get_bridge_status'));
    } catch {
      /* 状态查询失败不打断 */
    }
  }, []);

  const startPolling = useCallback(() => {
    if (pollRef.current) clearInterval(pollRef.current);
    const startedAt = Date.now();
    pollRef.current = setInterval(async () => {
      try {
        const s = await invoke<BridgeStatusDto>('get_bridge_status');
        setStatus(s);
        if (
          s.status === 'ready' ||
          s.status === 'failed' ||
          Date.now() - startedAt > 120000
        ) {
          if (pollRef.current) clearInterval(pollRef.current);
          pollRef.current = null;
        }
      } catch {
        /* 忽略单次轮询失败 */
      }
    }, 1000);
  }, []);

  useEffect(() => {
    (async () => {
      try {
        setSettings(await invoke<BridgeSettingsDto>('get_bridge_config'));
      } catch {
        /* 读取失败用默认值 */
      }
      await refreshStatus();
    })();
    return () => {
      if (pollRef.current) clearInterval(pollRef.current);
    };
  }, [refreshStatus]);

  const handleSave = async () => {
    setSaving(true);
    try {
      await invoke('save_bridge_config', { settings });
      showAlert('success', '保存成功', '桥接正在重新连接');
      startPolling();
    } catch (error) {
      showAlert(
        'error',
        '保存失败',
        error instanceof Error ? error.message : String(error),
      );
    } finally {
      setSaving(false);
    }
  };

  const handleRetry = async () => {
    try {
      await invoke('retry_bridge');
      startPolling();
    } catch (error) {
      showAlert(
        'error',
        '重试失败',
        error instanceof Error ? error.message : String(error),
      );
    }
  };

  return (
    <div className='space-y-4'>
      {/* 远程桥接地址 */}
      <div>
        <label className='mb-1 block text-sm font-medium text-gray-700 dark:text-gray-300'>
          远程桥接地址
        </label>
        <input
          type='text'
          value={settings.remote_url}
          onChange={(e) =>
            setSettings({ ...settings, remote_url: e.target.value })
          }
          placeholder='http://192.168.1.20:8080'
          className='w-full rounded-lg border border-gray-300 px-3 py-2 text-sm dark:border-gray-600 dark:bg-gray-700 dark:text-gray-100'
        />
        <p className='mt-1 text-xs text-gray-500 dark:text-gray-400'>
          在同一局域网设备（电视盒子/旧手机）安装 QuantumTV Bridge
          APK 后填入其地址；优先于本地模拟器。未鉴权，仅限可信局域网使用。
        </p>
      </div>

      {/* adb 地址列表 */}
      <div>
        <label className='mb-1 block text-sm font-medium text-gray-700 dark:text-gray-300'>
          adb 连接地址（逗号或换行分隔）
        </label>
        <textarea
          value={settings.adb_addresses}
          onChange={(e) =>
            setSettings({ ...settings, adb_addresses: e.target.value })
          }
          rows={2}
          placeholder='127.0.0.1:5555, 127.0.0.1:16384'
          className='w-full rounded-lg border border-gray-300 px-3 py-2 text-sm dark:border-gray-600 dark:bg-gray-700 dark:text-gray-100'
        />
        <p className='mt-1 text-xs text-gray-500 dark:text-gray-400'>
          LDPlayer 默认 127.0.0.1:5555，MuMu 默认 127.0.0.1:16384。留空且开启自动扫描时自动探测常见端口。
        </p>
      </div>

      {/* 自动扫描 */}
      <label className='flex items-center gap-2 text-sm text-gray-700 dark:text-gray-300'>
        <input
          type='checkbox'
          checked={settings.auto_scan}
          onChange={(e) =>
            setSettings({ ...settings, auto_scan: e.target.checked })
          }
        />
        自动扫描常见模拟器端口
      </label>

      {/* 状态卡片 */}
      <div className='rounded-lg border border-gray-200 p-3 text-sm dark:border-gray-700'>
        {status ? (
          <div className='flex items-center gap-2'>
            <span
              className={`inline-block h-2.5 w-2.5 rounded-full ${STATUS_COLOR[status.status]}`}
            />
            <span className='font-medium'>
              {STATUS_LABEL[status.status]}
            </span>
            <span className='text-gray-500 dark:text-gray-400'>
              模式: {MODE_LABEL[status.mode]}
            </span>
            {status.effective_url && (
              <span className='text-gray-500 dark:text-gray-400'>
                {status.effective_url}
              </span>
            )}
          </div>
        ) : (
          <span className='text-gray-500 dark:text-gray-400'>加载中…</span>
        )}
      </div>

      {/* 操作 */}
      <div className='flex gap-2'>
        <button
          onClick={handleSave}
          disabled={saving}
          className='rounded-lg bg-blue-600 px-4 py-2 text-sm text-white transition-colors hover:bg-blue-700 disabled:opacity-50'
        >
          {saving ? '保存中…' : '保存并重连'}
        </button>
        <button
          onClick={handleRetry}
          className='rounded-lg border border-gray-300 px-4 py-2 text-sm text-gray-700 transition-colors hover:bg-gray-50 dark:border-gray-600 dark:text-gray-300 dark:hover:bg-gray-700'
        >
          重试
        </button>
      </div>
    </div>
  );
}
```

- [ ] **Step 2: 挂载到 admin 页**

`src/app/admin/page.tsx`：

1. import 区（`import AnalyticsDashboard ...` 附近）追加：

```tsx
import BridgeSettings from '@/components/BridgeSettings';
```

2. lucide-react import 中追加 `Radio`（按字母序插入）。

3. `expandedTabs` state（~第 1436 行）追加键：

```tsx
    bridge: false,
```

4. 在"视频源配置"CollapsibleTab 之后（"自定义分类"之前，~第 1553 行）插入：

```tsx
        {/* 桥接设置 */}
        <CollapsibleTab
          title='桥接设置'
          icon={<Radio className='w-5 h-5 text-orange-500' />}
          isExpanded={expandedTabs.bridge}
          onToggle={() => toggleTab('bridge')}
        >
          <BridgeSettings showAlert={showAlert} />
        </CollapsibleTab>
```

- [ ] **Step 3: lint + 类型检查**

Run: `npm run lint`、`npm run typecheck`
Expected: 均无错误

- [ ] **Step 4: 提交**

```bash
git add src/components/BridgeSettings.tsx src/app/admin/page.tsx
git commit -m "feat(ui): bridge settings tab in admin page"
```

---

### Task 9: 全量验证 + 手动集成清单

**Files:** 无代码改动（验证任务）

- [ ] **Step 1: 全量自动验证**

```bash
cargo test -p quantumtv-core        # workdir=仓库根
cargo check                          # workdir=src-tauri
npm run lint && npm run typecheck    # workdir=仓库根
```
Expected: 全部通过

- [ ] **Step 2: 手动集成验证（有设备时逐项执行，无设备记录跳过）**

1. **远程真机**：局域网 Android 设备安装 `android/spider-bridge/out/bridge.apk`，启动 BridgeService，admin 页填 `http://<设备IP>:8080` 保存 → 状态应"就绪"，模式"远程真机"，wex 站点搜索出结果
2. **第三方模拟器**：启动 LDPlayer（5555），清空远程地址，手动地址留空开自动扫描 → 状态就绪，模式"第三方模拟器"
3. **官方 AVD 回归**：前两者关闭 → AVD 自动拉起，模式"官方模拟器"（回归 ADR 0002 Phase 4 行为）
4. **env 兼容**：设 `QUANTUMTV_BRIDGE_URL=http://<设备IP>:8080` 启动应用 → 不配 UI 也应直连成功
5. **降级**：全部不可用 → wex 站点搜索报"桥接未就绪"且其他站点正常

- [ ] **Step 3: 收尾提交（如有零星修正）**

```bash
git add -A
git commit -m "chore: bridge remote-first integration fixes"
```

（无修正则跳过本步）
