# 桥接 v2：APK 主动注册 + TCP 隧道（移除 adb/AVD）

日期: 2026-09-07
状态: 已批准
关联: ADR 0002（Android 桥接）、2026-09-04-bridge-remote-first-design.md（Phase A/B/C 瀑布）

## 背景与问题

现行桥接 = 桌面端主动寻找模拟器：adb 端口扫描（Phase B）→ 失败则拉起 AVD（Phase C）。实践暴露的结构性问题：

1. **adb server 端口 5037 冲突**：MuMu 等模拟器自带 adb 与 SDK adb 打架，应用启动时扫描窗口内 adb server 不稳定（实测首次启动 Phase B 2 秒即崩、AVD 60s 注册不上）
2. **forward 端口绑定**：换设备重绑同一 host 端口需清理旧 forward，时序敏感
3. **AVD 兜底**：扫描失败自动拉起 wexbridge AVD，用户看到"模拟器自己打开"；且 AVD 的 DNS/域名池问题（kstore.vip 解析污染）导致 detail 连锁 NPE
4. NAT 方向天然不利：客户端（桌面）找服务端（模拟器），防火墙/端口/NAT 全是障碍

## 方案（v2：反转为 APK 主动注册）

```
MuMu / 任意模拟器                          桌面端 (core bridge)
┌─────────────────────────┐      ┌──────────────────────────────┐
│ BridgeService            │      │ TunnelServer  127.0.0.1:18099 │
│  ├ ServerSocket:8080     │      │   ← 接受 APK 主动注册(持久TCP) │
│  │  (保留: 局域网Phase A) │dial──▶│   多台同时拨入: 只留第一个健康者│
│  └ TunnelClient (新)     │      │ VirtualBridge 127.0.0.1:18090 │
│     每5s拨网关:18099      │      │   ← spider 层照旧 POST 这里    │
│     断线自动重拨(5s→30s)   │      │   → 打包成帧写进隧道           │
└─────────────────────────┘      └──────────────────────────────┘
```

- **网关发现（APK 侧）**: `10.0.2.2`（QEMU 系：AVD/MuMu/LDPlayer）→ `ip route` default via → `192.168.56.1`（VirtualBox 系 Nox）。拨通即用
- **effective_url 不变**: 仍为 `http://127.0.0.1:18090`，spider/video 层零改动；新增 `mode=tunnel`
- **已验证**: MuMu 内 `10.0.2.2` 可达（ping 0.5ms）；APK 已有 BootReceiver + START_STICKY 常驻

## 隧道协议

帧格式: `[u32 BE id][u32 BE len][payload]`

- 连接建立后**首帧**（APK→桌面）: JSON 注册 `{"device":"<Build.MODEL>","apk":"<versionName>"}`，id=0
- 业务帧: payload = 完整原始 HTTP 请求字节（桌面→APK，方法+路径+头+body）或 HTTP 响应体（APK→桌面，含状态行）。按 id 配对请求/响应
- **并发多路复用**: 同一隧道上多个 in-flight 请求靠 id 区分；APK 侧复用现有 `pool`(4 线程) + `detailExecutor`(单线程优先) 处理，detail 插队语义完整保留
- 心跳: 无专用心跳帧，靠 TCP keepalive + 请求超时兜底；APK 断线自动重拨（5s 起，上限 30s，指数退避），重拨成功重新注册
- 桌面端隧道断开: 所有 pending 请求立即失败（传输错误），`BRIDGE_INITED` 复位（复用现有逻辑，下次调用补发 /init）；隧道重连后自动恢复 Ready

## 组件设计

### 1. APK 侧（android/spider-bridge，~120 行）

- 新增 `TunnelClient.java`: 拨号循环线程（网关列表 × 端口 18099）+ 帧读写；断线重连指数退避
- `BridgeService` 重构: 抽出 `routeRequest(method, path, body) → respJson`（Socket 模式与隧道模式共用路由与 detailExecutor 优先级逻辑）；`handle(Socket)` 与 `TunnelClient` 都调用它
- `ServerSocket:8080` 保留（局域网真机 Phase A 直连仍用）
- 隧道模式请求解析: 桌面端发来的 payload 是完整 HTTP 请求字节 → 复用 `readLineRaw`/Content-Length 解析 → routeRequest → 响应字节写回帧

### 2. 桌面端 core（crates/core/src/bridge/tunnel.rs，新文件）

- `TunnelServer`: tokio TcpListener 127.0.0.1:18099（env `QUANTUMTV_TUNNEL_PORT` 可覆盖）
  - accept 循环: 新连接先读注册帧；已有健康隧道时新连接**拒绝**（回错误帧后关闭）；旧隧道死亡则接受新连接顶替
  - 维护 `Mutex<Option<TunnelConn>>`: conn 含 writer 半区（Mutex<mpush>）与 pending map（id → oneshot sender）
  - reader 任务: 按帧读响应 → 按 id 唤醒 pending
- `VirtualBridge`: TcpListener 127.0.0.1:18090（端口沿用 host_port 配置）
  - accept → 读完整 HTTP 请求字节 → 分配 id → 写帧进隧道 → 等 oneshot 响应 → 原样写回 HTTP 响应
  - 这一层是无状态转发，spider 层 `bridge_post_with` 完全无感
- 帧编解码纯函数 + 单测

### 3. 启动瀑布重写（startup_steps）

- **Phase 0（新）**: 等隧道注册，最多 6s（APK 每 5s 拨一次，一次等待覆盖至少一个拨号周期）；成功 → `set_effective(bridge_url, EFFECTIVE_TUNNEL)` → Ready
- **Phase A**: 远程直连（不变，真机场景）
- 失败 → Failed，错误文案: 「未发现桥接设备：请将 bridge.apk 拖入模拟器安装并保持其运行，或在局域网真机上填写远程桥接地址」
- **删除 Phase B/C**: `spawn_emulator`/`wait_boot`/`ensure_apk_installed`/`forward_port`/`SCAN_PORTS`/`phase_b_addresses`/`ensure_avd_bridge`/`serial_matches_avd`/`wait_for_avd_serial`/`emulator_args`/`emulator_serial(s)`/`EMU_CHILD`/`STARTED_SERIAL`/`we_started`
- ENV_KEYS 清理: 移除 `QUANTUMTV_BRIDGE_AVD/SDK/ADB_ADDRESSES/AUTO_SCAN/APK`；新增 `QUANTUMTV_TUNNEL_PORT`
- shutdown(): 不再有模拟器可杀，仅关闭 TunnelServer/VirtualBridge 监听与现有隧道

### 4. UI/命令层

- `BridgeSettingsDto` 收缩: 移除 `adb_addresses`/`auto_scan` 字段（保留 serde(default) 兼容旧 data.json 反序列化）；`remote_url` 保留
- 桥接设置 UI: 删除 adb 地址/自动扫描输入框；新增 APK 路径提示行（默认 `android/spider-bridge/out/bridge.apk`，文案「将 bridge.apk 拖入模拟器窗口安装，保持 BridgeService 运行」）
- 状态 mode 新增 `tunnel`（EFFECTIVE_TUNNEL=4）
- `commands/bridge.rs` 的 `build_bridge_map`/`validate_settings` 同步收缩
- `launch_bridge_activity`（WebView 网盘登录兜底）保留；无 adb 时其内部 adb 调用自然报错，主路径已是桌面扫码

### 5. APK 安装与升级

- 手动拖装: 用户将 `bridge.apk` 拖入 MuMu 窗口安装（MuMu 原生支持）；其他模拟器用各自安装方式
- 无自动升级: APK 版本随订阅 spider JAR 变化需重装时，管理页显示提示文案「APK 已更新，请重新拖装」（版本号写入注册帧，桌面端记录并与本地 out/bridge.apk 比对——本期仅记录，不做自动提示，留 TODO 之外的后续项）

## 错误处理汇总

| 场景 | 行为 |
|------|------|
| APK 未装/未运行 | Phase 0 6s 超时 → Phase A 失败 → Failed + 可操作文案；APK 拨入自动转 Ready（无需用户重试） |
| 隧道中断（模拟器重启等） | pending 请求立即传输错误；APK 5s 内重拨 → 自动恢复 Ready |
| 多台设备同时拨入 | 先到先得，后者拒绝（回错误帧）；前者断开后新连接自动顶替 |
| detail 优先级 | 多路复用 id 隔离 + APK 侧 detailExecutor 单线程优先，语义不变 |
| 旧 data.json 含 adb_addresses/auto_scan | serde(default) 静默忽略；UI 不再展示 |

## 不做的事（YAGNI）

- 多设备管理/切换（单隧道先到先得）
- APK 自升级/隧道推送安装
- 隧道加密（流量仅本机回环 + 模拟器 NAT 内网；局域网真机走 Phase A 直连，安全模型不变）
- 心跳帧（TCP keepalive + 请求超时足够）
- 局域网网关发现广播（网关列表三连覆盖主流模拟器）

## 测试

- Rust 单测（tunnel.rs）: 帧编解码 round-trip、注册帧解析、第二连接拒绝、pending 断线清理、VirtualBridge 请求→帧→响应 happy path（用内存双工 mock）
- 手动 E2E: MuMu 拖装新 APK → 启动应用（全程无 adb）→ 状态 Ready(mode=tunnel) → 搜索/详情/扫码登录 setCookie 全走隧道 → 杀 BridgeService → 请求报传输错误 → 重启服务 5s 内自愈 → 退出应用隧道关闭
- 回归: 局域网真机 Phase A（remote_url）不受影响

## 涉及文件

| 文件 | 动作 |
|------|------|
| crates/core/src/bridge/tunnel.rs | 新增（帧编解码 + TunnelServer + VirtualBridge + 单测） |
| crates/core/src/bridge/mod.rs | 重写 startup_steps（Phase 0/A）；删除 Phase B/C 全部；shutdown 收缩；ENV_KEYS/常量清理 |
| crates/core/src/bridge/mod.rs tests | 删除 adb 相关用例，新增 Phase 0 用例 |
| android/spider-bridge/src/.../TunnelClient.java | 新增 |
| android/spider-bridge/src/.../BridgeService.java | 重构 routeRequest；接 TunnelClient |
| android/spider-bridge/out/bridge.apk | build.ps1 重打包 |
| src-tauri/src/commands/bridge.rs | DTO/build_bridge_map/validate 收缩 |
| src/components/BridgeSettings.tsx（现有桥接设置组件） | 删 adb 地址/自动扫描框；加 APK 拖装提示 |
| src/components/CloudAccountSettings.tsx | 无改动 |
| docs/adr/0003-bridge-tunnel.md | 新增 ADR 记录决策 |
