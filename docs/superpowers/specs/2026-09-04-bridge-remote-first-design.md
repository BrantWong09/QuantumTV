# 桥接子系统重构：远程优先 + 模拟器降级

状态: 已批准（设计阶段）
日期: 2026-09-04
关联: docs/adr/0002-android-bridge-for-wex-spiders.md

## 背景

ADR 0002 确立了 wex Guard 类 Spider 站点通过 Android 模拟器（AVD wexbridge）+ 桥接 APK 执行的方案。现状痛点：

1. 桌面端强依赖本地 Android SDK + AVD + WHPX，用户安装负担重（SDK 数 GB、首次冷启动约 55s、常驻约 2GB RAM）
2. 桥接协议本身是纯 HTTP（BridgeService 绑定 0.0.0.0:8080），却只能通过 adb forward 在本机回环访问
3. 启动编排（crates/core/src/bridge/mod.rs `ensure_ready`）只认官方 emulator 拉起的 `emulator-*` serial，无法利用用户机器上已有的第三方模拟器（LDPlayer/MuMu/Nox 等，均支持 TCP adb）

## 目标

1. **远程真机优先**：用户在局域网设备（电视盒子/旧手机）安装 bridge.apk 后，桌面端直接 HTTP 直连，零模拟器、零 adb
2. **第三方模拟器次之**：无远程真机时，自动发现或手动配置本机 TCP adb 端口的第三方模拟器，复用现有安装/启动/forward 流程
3. **官方 AVD 兜底**：前两者都不可用时保持现有模拟器拉起逻辑不变
4. **配置进 UI**：桥接设置从环境变量迁到 admin 管理页（env 兜底保留，向后兼容）

## 非目标（本期不做）

- 不加 token 鉴权（已决策）。UI 文案提示仅限可信局域网使用
- 桥接 APK 零改动（ServerSocket 已绑 0.0.0.0，协议不变）
- ext 配置链路（ADR 后续工作另一项，不在此范围）
- api-server（TVBox 外部设备路径）不走桥接，不涉及

## 架构

### 三阶段瀑布启动

```
ensure_ready_with(cfg):
  Phase A  远程直连     候选列表 = [cfg.url_override(若 env QUANTUMTV_BRIDGE_URL 已设置), cfg.remote_url(若已设置)]
                       逐一 probe_health，首个健康者 → 生效 URL = 该候选，完成
  Phase B  第三方模拟器  adb connect 手动地址/扫描端口 → 等设备就绪 → 装 APK → forward → 生效 URL = http://127.0.0.1:{host_port}
  Phase C  官方 AVD    现有逻辑（spawn_emulator → wait_boot → ensure_apk_installed → forward → 健康轮询），保留不变
  全部失败 → BridgeStatus::Failed（静默降级，wex 站点搜索/详情报错跳过，同现状）
```

决策纯函数 `decide_phase(...)` 输入 (remote_url 配置与否, 健康探测结果, 设备列表)，便于单测。

### 生效 URL（effective URL）

核心新概念。桥接解析成功后（任一阶段命中）将其存入 core 全局状态：

- `QUANTUMTV_BRIDGE_URL` 环境变量若设置 → 成为 Phase A 第一候选（先于 remote_url 探活），健康即生效（向后兼容；Phase A 失败仍继续瀑布）
- Phase A 命中 → 命中的候选 URL（env 覆盖值或 remote_url）
- Phase B/C 命中 → `http://127.0.0.1:{host_port}`（默认 18080）

消费方 `video.rs` 从读环境变量改为读 `effective_url()`；为 `None` 时快速失败（返回"桥接未就绪"错误），不再发起注定失败的网络请求。

注意区分两类配置：`remote_url`/`adb_addresses`/`auto_scan` 是**启动编排配置**（UI 优先、env 兜底）；`QUANTUMTV_BRIDGE_URL` 是**生效 URL 的历史兼容通道**（探活候选，不参与 UI 配置合并）。

## 组件设计

### 1. core 层（crates/core/src/bridge/mod.rs）

**BridgeConfig 新增字段：**

| 字段 | 类型 | 默认 | env 键 | UI 来源键 |
|------|------|------|--------|-----------|
| `remote_url` | `Option<String>` | None | `QUANTUMTV_BRIDGE_REMOTE_URL` | `remote_url` |
| `adb_addresses` | `Vec<String>` | [] | `QUANTUMTV_BRIDGE_ADB_ADDRESSES`（逗号分隔） | `adb_addresses` |
| `auto_scan` | `bool` | true | `QUANTUMTV_BRIDGE_AUTO_SCAN` | `auto_scan` |

`from_map` 签名不变（`&HashMap<String, String>`），Tauri 层负责把 UI 配置 JSON 与 env 合并成 map（UI 优先）。

**生效 URL 状态：**

```rust
static EFFECTIVE_URL: Mutex<Option<String>> = Mutex::new(None);
pub fn effective_url() -> Option<String>;
pub(crate) fn set_effective_url(v: Option<String>);
```

**TCP serial 支持：**

- 新增 `connectable_serials(out) -> Vec<String>`：从 `adb devices` 输出解析 `127.0.0.1:5555\tdevice` 形式的 serial（含冒号端口的视为 TCP 型）
- TCP 设备跳过 `serial_matches_avd` 校验（第三方模拟器 AVD 名不可靠），"匹配"即视为可用
- Phase B 流程：`adb connect <addr>`（幂等）→ `adb devices` 找 `addr` 对应 serial → `wait_boot` → `ensure_apk_installed` → `forward_port` → `probe_health`

**扫描端口表（常量数组，纯函数可测）：**

| 模拟器 | 端口 |
|--------|------|
| LDPlayer | 5555, 5557 |
| Nox | 62001, 62025, 62026, 62027 |
| MuMu | 16384, 16416, 16448 |
| WSA | 58526 |

扫描顺序：手动 `adb_addresses` 全部尝试 → `auto_scan` 时扫端口表。`adb connect` 单地址超时 5s，地址间串行，整体 Phase B 上限 120s。

**shutdown 不变**：只关闭自己拉起的官方 AVD（EMU_CHILD / STARTED_SERIAL），远程与第三方设备一概不碰。

### 2. Tauri 层（src-tauri）

**配置持久化：** `data.json` 的 `config.BridgeConfig`（与 `PlayerConfig` 平级）：

```json
{ "remote_url": "", "adb_addresses": "", "auto_scan": true }
```

`adb_addresses` 存字符串（逗号分隔），与 UI 文本域一一对应；UI 提交前将换行归一为逗号，解析/校验在 core `from_map` 内完成。

**新命令模块 `src-tauri/src/commands/bridge.rs`（4 个命令）：**

| 命令 | 行为 |
|------|------|
| `get_bridge_config` | 读 data.json 返回 BridgeConfig JSON |
| `save_bridge_config` | 校验（URL 格式、地址列表格式）→ 落盘 → 触发重试（同 retry_bridge） |
| `get_bridge_status` | 返回 `{ status, effective_url, mode }`；mode 为 `remote|emulator|avd|none`，由 core 记录命中阶段 |
| `retry_bridge` | 若当前 Starting 则拒绝（防重入）；否则 set_status(Idle) 后 spawn 后台 `ensure_ready_with` |

**启动钩子（lib.rs:151-156 改造）：** 从 storage 读 `config.BridgeConfig` → 与 env 合并（UI 优先）成 map → `ensure_ready_with(BridgeConfig::from_map(&map))`。失败仍静默降级。

**video.rs 改造（1096/1462 两处）：** `std::env::var("QUANTUMTV_BRIDGE_URL")` 替换为 `quantumtv_core::bridge::effective_url()`，None 时返回 Err("桥接未就绪")。该错误在 `search_with_cache_hit` 的 per-site 容错中被吞掉（站点结果为空），与现状一致。

### 3. 前端（src/app/admin/page.tsx）

新增"桥接设置" CollapsibleTab（模式同现有 1523-1580 行），组件 `BridgeSettings`：

- 远程桥接地址输入框（placeholder：`http://192.168.x.x:8080`；提示文案：在同一局域网设备安装 bridge.apk 后填入其 IP；仅限可信局域网）
- adb 地址列表文本域（逗号/换行分隔；提示：LDPlayer 默认 127.0.0.1:5555，MuMu 默认 127.0.0.1:16384）
- 自动扫描开关（默认开）
- 状态卡片：四态（未启动/启动中/就绪/失败）+ 生效 URL + 命中模式徽标
- "保存并重试"按钮：`save_bridge_config` 后轮询 `get_bridge_status`（1s 间隔，最长 120s）刷新状态卡片

## 数据流

```
启动: lib.rs setup → storage 读 BridgeConfig → merge env → ensure_ready_with
       → 瀑布 A/B/C → set_effective_url + set_status
搜索: video.rs search_site_results → is_bridge_class? → effective_url()
       → Some(url) → spider_bridge_search(url) / None → Err 快速失败
保存: admin UI → save_bridge_config → data.json 落盘 → 后台 ensure_ready_with → UI 轮询状态
```

## 错误处理

| 场景 | 行为 |
|------|------|
| remote_url 配置但探活失败 | 静默进入 Phase B/C；UI 状态卡片显示失败详情（保存重试后） |
| env QUANTUMTV_BRIDGE_URL 探活失败 | 不短路瀑布：继续尝试 remote_url → Phase B → Phase C |
| adb connect 超时/拒绝 | 该地址记失败，继续下一个；全部失败进 Phase C |
| 扫描到的端口不是桥接设备 | connect 成功但 probe_health 失败 → 不装 APK，跳过该设备 |
| 装 APK 失败（第三方模拟器） | Phase B 失败，进 Phase C |
| 全部阶段失败 | Failed 静默降级；wex 站点报"桥接未就绪"，非 wex 站点完全不受影响 |
| save_bridge_config 在 Starting 期间触发 | 落盘成功但重试被拒绝（Starting 防重入）；用户可在状态变 Failed/Ready 后手动重试 |

## 测试策略

- **core 单测**（现有 31 个不回归 + 新增）：
  - `from_map` 新键解析（含逗号分隔地址、非法值容错）
  - `connectable_serials` TCP serial 解析（含 device/offline 混合）
  - 扫描端口表常量正确性
  - `decide_phase` 决策矩阵（env 设置/A 命中/B 命中/全败）
- **Tauri 层**：`cargo check` + `cargo test`（现有命令不回归）
- **前端**：`npm run lint`；手动验证 admin 页保存/轮询/状态展示
- **手动集成验证**（有设备时）：远程真机直连；LDPlayer connect 路径；官方 AVD 回归

## 风险与缓解

| 风险 | 缓解 |
|------|------|
| 第三方模拟器 adb 版本兼容性（连接后 shell 命令行为差异） | 只用 wait_boot/pm path/install/forward 等 4 个稳定命令，与官方 AVD 路径共用同一实现 |
| 局域网无鉴权暴露 spider 执行能力 | 已决策接受；UI 文案明示仅限可信局域网 |
| 扫描端口误连其他进程 | connect 成功后 probe_health(bridge) 校验，非桥接设备直接跳过，不装 APK |
| 用户配错 remote_url 导致每次启动多等一次探活超时 | 探活超时 3s（沿用现有 probe_health），Phase A 最多拖慢 6s（两个候选） |
