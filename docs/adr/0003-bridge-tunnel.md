# ADR 0003: 桥接 v2 — APK 主动注册 + TCP 隧道，移除 adb/AVD 瀑布

日期: 2026-09-07
状态: 已接受
关联: ADR 0002（Android 桥接）、2026-09-07-bridge-tunnel-design.md

## 背景

ADR 0002 确立的桥接路径是"桌面端主动找模拟器"：adb 端口扫描（Phase B）→ 失败拉起官方 AVD（Phase C）。实践中暴露结构性问题：

1. MuMu 等模拟器自带 adb 与 SDK adb 在 5037 端口打架，启动窗口内 adb server 不稳定（实测 Phase B 2 秒即崩）
2. adb forward 端口绑定在换设备时冲突，时序敏感
3. AVD 兜底自动拉起官方模拟器（用户感知为"模拟器自己打开"），且 AVD DNS/域名池问题导致 wex spider detail 连锁 NPE（kstore.vip 解析污染）
4. NAT 方向不利：桌面找模拟器，每一步都是障碍

## 决策

反转为 **APK 主动注册**：

- APK 内新增 TunnelClient：每 5s 拨宿主网关（候选 10.0.2.2 → `ip route` default via → 192.168.56.1）:18099，建立持久 TCP 隧道
- 帧协议 `[u32 id][u32 len][payload]`：首帧注册 JSON，之后桌面→APK 帧为原始 HTTP 请求字节，APK 按帧处理（复用 detailExecutor 优先级）并按 id 回响应帧
- 桌面端 `bridge/tunnel.rs`：TunnelServer(127.0.0.1:18099) 收注册 + VirtualBridge(127.0.0.1:18090) 收 spider 层请求转进隧道；`effective_url` 语义不变，spider 层零改动
- 启动瀑布收缩为 Phase 0（等注册 6s）→ Phase A（局域网真机直连，保留）→ Failed + 可操作文案
- **彻底删除** Phase B/C：adb 扫描、forward、AVD 拉起及全部配套（约 -776 行）

## 后果

- 正面: 运行时零 adb 依赖；连接方向与 NAT 一致；APK 断线 5s 自愈重拨；多路复用保留 detail 优先级；删除大量时序敏感代码
- 负面: APK 安装/升级需手动（MuMu 拖装或 adb 一次）；多台设备同时在线先到先得（暂不做切换管理）；隧道无加密（流量仅本机回环 + 模拟器 NAT 内网，安全模型与此前 forward 等价）
- 兼容: ServerSocket:8080 保留，局域网真机 Phase A 直连不受影响；旧 data.json 的 adb_addresses/auto_scan 字段被静默忽略
