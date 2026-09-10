# 桥接自动拉起（ADR 0002 Phase 4）设计

日期: 2026-09-04
状态: 已确认
关联: docs/adr/0002-android-bridge-for-wex-spiders.md

## 目标

wex Guard 类站点需要 Android 模拟器内桥接 APK 提供执行能力。本设计让桌面端
(Tauri) 启动时自动完成 AVD 拉起、adb forward、桥接 APK 健康检查，应用退出时
自动关闭模拟器，全程无需用户手动操作；失败时静默降级，不影响其它功能。

## 非目标

- bridge_url 配置进管理界面（ADR 后续工作第 3 项，另行处理）
- 内容 spider（Wexzhizhen 等）ext 业务配置链路（另行处理）
- 桥接 APK 的自动构建/重打包（仍用 android/spider-bridge/build.ps1 手工构建）

## 架构

core 新增 `bridge` 模块（`crates/core/src/bridge/mod.rs`），职责单一：保证
"AVD 运行 + adb forward + APK 健康" 三件事，不碰 spider 业务逻辑。Tauri 层
负责在应用生命周期钩子中调用；api-server 后续可复用。

### 数据流

```
应用启动 (tauri setup)
  └─ tokio::spawn(bridge::ensure_ready())          // 后台执行,不阻塞启动
       ├─ 1. 配置解析 (BridgeConfig::from_env)
       ├─ 2. locate_adb()     — 定位 adb.exe
       ├─ 3. probe_health()   — GET /health (3s 超时)；已就绪 → 直接做 forward 后结束
       ├─ 4. devices()        — adb devices；wexbridge 已运行 → 跳过启动
       ├─ 5. start_emulator() — spawn emulator @wexbridge
       │                        (-no-snapshot-save -no-boot-anim -gpu auto)
       ├─ 6. wait_boot()      — adb wait-for-device + sys.boot_completed 轮询 (上限 120s)
       ├─ 7. install_apk()    — adb shell pm path 已装则跳过，否则 adb install -r
       ├─ 8. start_app()      — adb shell am start 桥接 activity/service
       ├─ 9. adb forward tcp:18080 tcp:8080
       └─ 10. probe_health()  — 最终校验；失败 → 状态 Failed (静默降级)
应用退出 (RunEvent::ExitRequested / Exit)
  └─ bridge::shutdown() — adb emu kill；进程未退则 kill
```

### 状态管理

- `static STATE: OnceLock<BridgeState>`，内部 `AtomicU8`
- 状态机：`Idle → Starting → Ready | Failed`
- `bridge::status() -> BridgeStatus` 供调用侧/调试查询
- spider 分流逻辑（`is_bridge_class`）不变；状态非 Ready 时请求照常发出，
  失败走现有 bridge error 路径（与现状一致）

## 配置

全部走环境变量，有默认值，零配置可用：

| 变量 | 默认 | 说明 |
|------|------|------|
| `QUANTUMTV_BRIDGE_ENABLED` | `1` | `0` 时模块完全不动 |
| `QUANTUMTV_BRIDGE_AVD` | `wexbridge` | AVD 名称 |
| `QUANTUMTV_BRIDGE_SDK` | `%LOCALAPPDATA%\Android\Sdk` | SDK 根目录；定位不到 adb → Failed |
| `QUANTUMTV_ADB_HOST_PORT` | `18080` | adb forward 主机端口 |
| `QUANTUMTV_BRIDGE_URL` | `http://127.0.0.1:18080` | spider 模块现有变量，默认值与 forward 端口对齐（原 8080 易冲突） |

## 错误处理

- 每步失败：`log::warn!/error!` 记录 + 状态置 `Failed`，不 panic、不自动重试
- 健康轮询：指数退避（1s 起，封顶 5s，总时长 120s），异步 sleep，不阻塞 runtime
- `emulator` 进程以 detached spawn 持有，`shutdown()` 先 `adb emu kill`，
  3s 内未退出再强杀进程句柄

## 测试

沿用 core 现有测试模式：

- 单测（`crates/core/src/bridge/mod.rs` `#[cfg(test)]`）：
  - 配置解析（默认值 / 环境变量覆盖 / ENABLED=0）
  - 状态机转换（Idle→Starting→Ready / →Failed）
  - adb 命令行参数构造（纯字符串断言，不出进程）
- 集成（`crates/core/tests/bridge_lifecycle.rs`，全部 `#[ignore]`，需本机
  SDK + wexbridge AVD 才跑）：
  - ensure_ready → health OK → shutdown → adb devices 中设备消失

## Tauri 集成

- `setup` 钩子：`tokio::spawn(quantumtv_core::bridge::ensure_ready())`
- `RunEvent::ExitRequested` / `Exit`：调用 `quantumtv_core::bridge::shutdown().await`
  （用 tauri 的异步 runtime handle 阻塞等待，保证 kill 完成）
- `src-tauri` 中 `QUANTUMTV_BRIDGE_URL` 默认值 8080 → 18080，与 forward 一致

## 影响面

- 新增文件：`crates/core/src/bridge/mod.rs`、`crates/core/tests/bridge_lifecycle.rs`
- 修改文件：`crates/core/src/lib.rs`（挂模块）、`src-tauri/src/main.rs`（setup/exit 钩子）、
  `src-tauri/src/commands/video.rs`（BRIDGE_URL 默认端口两处）
- 不改动：spider 模块协议、桥接 APK、前端
