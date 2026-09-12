# ADR 0004: 桥接 worker 进程隔离 — spider 执行体迁出 control，超时即杀进程

日期: 2026-09-12
状态: 已接受
关联: ADR 0003（TCP 隧道）、2026-09-12-android-bridge-worker-isolation.md、方案《Android Bridge 防死锁与故障隔离重构方案 V2》

## 背景

桥接 v2 的 control 进程（`BridgeService`）既做 HTTP/隧道入口又直接反射执行 spider。wex spider 的 native 初始化（`guard` 的 `System.loadLibrary`/`init0`）存在**永久挂死**（不抛异常、不返回）的实测前科（夸克 `playerContent` 45s 级）。后果链：单线程被挂死吞噬 → 请求堆积 → 桌面侧超时谎报"需要登录"（T9 修复的错分类）；native 卡死持有类锁/监视器 → 后续**一切** spider 调用连坐。任何应用内"取消"手段（`Thread.stop` 被移除、`interrupt` 不碰 native、`ThreadGroup` 管不了 native 阻塞）都无法恢复——挂死的线程连 `finally` 都不执行，进程内清理逻辑也永远等不到。

## 决策

1. **进程即取消原语**：spider 执行体迁入两个独立 worker 进程（`:spider_general` 承担 home/category/detail/search/init，`:spider_playback` 承担 playerContent）。control 进程零 spider 代码、零全局锁，只做 HTTP/隧道 → 帧路由。
2. **超时 = Watchdog SIGKILL worker 进程 + 按 §22 重启**（restart<3s，指数退避+连死熔断），永不 kill 桌面 TCP 隧道；kill 前先以 `worker_killed` 应答在途请求，使排队者/桌面侧不悬死。
3. **硬超时 ≠ 取消，是进程回收依据**（`TimeoutPolicy`：阶段 A 宽容值 playerContent 90s / search·detail 60s / init 30s，只防永久挂死、不引入新失败）。Task 8 已布好 `[SpiderPerf]` 双端日志与 `test_metrics.ps1`，**正式定值 = 真夸克采样 P95×2（下限 30s）待用户采样后执行**；§74 禁止反向加大当解药。
4. **类级熔断**（`SourceBreaker`：3×超时/同错 → 该类 OPEN，他类不受影响）；`/init`、cookie 与 worker 状态彻底解耦（control 本地落盘 + `broadcastCookie`，worker READY 时经 `readyHook` 补发，native 下载惰性移入 worker 的 `SpiderExec.ensureInit`）。
5. **桌面端错误语义**：`worker_killed/restarting/disabled/unavailable/source_circuit_open/bridge_circuit_open` 一律映射为 NetworkError（"桥接执行环境故障(正在恢复)"），不再落入 `bridge error` 兜底而错报 AuthenticationRequired。

## 后果

- 正面: 挂死半径 = 单个 worker 进程；general 与 playback 互不连坐（§61 风暴验收过）；control 永不阻塞，`/health`、`/init`、隧道在 worker 全灭时仍正常；native 崩溃/内存泄漏可被重启回收；健康度与故障计数入 `/health`（§48 `sources_open`）。
- 负面: 每请求多一次本机 socket 帧往返（实测排队+IPC 开销 ≈ 2ms，可忽略）；worker 重启丢 spider 实例缓存（首次类加载 ~2.5s 已在日志量化）；内存占用 +2 进程（~30MB×2，换取隔离值得）；JAR 反射需与 control 同 dex（经 build 合并 `spider_classes.dex`）。
- 兼容: 隧道帧协议、HTTP 路由、`csp_*` 类名、订阅格式均不变；外部 TVBox 直连 8080 路径行为增强（不再全局连坐）。

## 真机教训 (2026-09-12 首轮 PGBM10+MuMu 验证即暴雷, 已全部修复并有 Phase C 回归)

1. **在途请求的 HB 静默 ≠ 死亡**：wex native 反调试/houdini dlopen 会连 HB 线程一起冻住 >5s，旧 Watchdog 的 `hbStale>5s → kill` 把合法慢初始化误杀 → 连环 3 杀 → crash_loop DISABLED → 全站 `worker_disabled`。修复：kill 权只属于 per-method 硬超时；`__test_freeze` 钩子常驻回归（Phase C1）。
2. **watchdog 必须单例**：误建在 per-role 循环内 → 双 watchdog 并发 kill/重复计 death，2 次 hang 即提前熔。
3. **每个 role 只认最新连接**：worker 重连产生的双 `connectionLoop` 会互相踩 `h.out`，RESP 滞留旧 socket 90s（orphan RESP）→ 已死 worker 被二次误杀。修复：connId 代数守卫 + supersede 旧连接；kill/disconnect 一律 `failAllPending`（§35 完整化）。
4. **DISABLED 必须有半开自愈**：冷却窗口（默认 60s，`/health` 可见）到期自动重探，否则任何一次误杀都永久砖化本会话。
