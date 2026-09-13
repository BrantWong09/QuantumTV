# ADR 0005: 多网盘认证与播放能力 Provider 化重构

> 对应方案: docs/QuantumTV 多网盘认证与播放能力重构方案 V1.md
> 状态: 已实施 (Phase 1-5 桌面端)
> 关联: ADR 0002 (Android Bridge), ADR 0004 (Worker 进程隔离)

## 1. 现状勘察结论 (方案 §50 十项)

1. **百度登录入口**: 桌面 Tauri `cloud_login_start/poll` (`src-tauri/src/commands/netdisk.rs`) →
   `qrcodelogin::baidu` (passport getqrcode → unicast 轮询 → qrbdusslogin 换 BDUSS/STOKEN/PTOKEN)。
2. **夸克扫码入口**: `qrcodelogin::start_cas("quark", client_id=532)` → uop.quark.cn CAS,
   二维码内容 su.quark.cn。
3. **UC 扫码入口**: `qrcodelogin::start_cas("uc", client_id=381)` → api.open.uc.cn CAS,
   二维码内容 su.uc.cn (域名错会被夸克 App 拦截, 见 qrcodelogin 回归测试)。
4. **凭证保存位置**: 桌面端**不持久化**。cookie 仅在 poll Confirmed 后 `push_cookie_to_bridge`
   → APK `/setCookie` (BridgeService.doSetCookie: 主进程 CookieManager +
   files/TV/.<drive>cookie 文件 + cookieStore 内存 map + 广播全部 worker 重建)。
   桌面重启后状态即丢 (只剩 APK 侧文件); 前端不保存 token (符合 §10, 但 Rust 侧也未存)。
5. **playerContent 调用路径**: play/page.tsx `playback_play_episode` →
   `cached_resolve_with` (moka SingleFlight, TTL 10min) → `ResolverManager`
   (SpiderResolver) → `BridgeSpiderPlayFetcher` → bridge POST /playerContent。
6. **Android 处理扫码的代码**: CloudLoginActivity (WebView 登录, 模拟器备用通道) +
   BridgeService.doSetCookie (cookie 三通道落地)。
7. **Android 处理 playerContent 的代码**: BridgeService.dispatchSpider →
   WorkerManager.dispatch(ROLE_PLAYBACK) → SpiderExec.invoke("playerContent") →
   反射调用混淆 wex spider (cookie 从本进程 CookieManager 读取)。
8. **Desktop 还原 kaiser URL**: `resolver.rs:272` → `spider::unwrap_local_proxy_url`
   解 `http://127.0.0.1:8096/kaiser?url=<内层>` 得内层直链 (MuMu 与宿主同出口 IP,
   百度 dlink 按 IP 绑定故可用)。
9. **netdisk_proxy upstream 构造**: `client().get(inner_url)` + UA (URL 参数,
   缺失时 LAST_UA 全局单值兜底) + Range 原样透传 → 流式回写;
   `/media/<token>` 路径 (gateway.rs) 由 ResourceSession 注入 UA/Referer/Cookie/headers。
10. **三条真实调用链** (百度/夸克/UC 形态完全一致, 仅 spider 类与 CAS 域名不同):

```text
登录:  桌面扫码 API → (CAS|passport) → service_ticket/BDUSS → cookie 串
       → bridge /setCookie → CookieManager + .<drive>cookie 文件 + 广播 worker 重建
播放:  playback_play_episode(source, flag, rawEpisodeId)
       → SpiderResolver → bridge /playerContent {class, flag, id}
       → wex spider (读 CookieManager, 内部完成 share→file→dlink)
       → kaiser 包装 URL + UA header
       → 桌面解包内层直链 → gateway /media/<token> (session 注入请求头)
       → mpv
```

### 关键结论 (夸克/UC 不可用的根因假设)

- **登录成功 ≠ 认证有效** (§6): poll Confirmed 后即认为成功, 无 verify_login;
  CAS 换 cookie 是否真的拿到有效会话 (UC 侧业务码/cookie 完整性) 无人校验。
- **桌面无凭证持久化** (§11/§12): 无状态查询、无"测试连接"、重启后无法主动恢复推送。
- **LAST_UA 全局单值**: 多网盘并发时 UA 兜底可能串网盘。
- **上游 401/403 无自动恢复** (§26): dlink/签名过期直接报错。

## 2. 决策

### D1: Provider 层落在桌面 Rust (crates/core/src/clouddrive/)

新增 `CloudDriveType / AuthStatus / AuthState / ProviderCredential / QrLoginSession /
LoginResult / ShareResource / CloudFile / PlayResource / CloudDriveCapabilities /
CloudDriveError` 与 `CloudDriveProvider` trait; `CloudDriveManager` 编排
认证状态机 + 凭证持久化 + verify + bridge 推送。

### D2: share→file→dlink 解析保留在 wex spider (经 bridge), 桌面不重复实现网盘 API

方案 §34-§36 的意图是"Bridge 只做 RPC、Provider 拥有领域逻辑"。现状中
share/file/resolve 的领域逻辑在混淆 wex spider 内 (ADR 0002/0004), 桌面复刻
夸克/UC API 属于方案 §45 明令禁止的"复制改域名"。因此:

- `CloudDriveProvider.parse_share` = 桌面纯解析 (share URL → ShareResource);
- `CloudDriveProvider.resolve_play_url` = 经 bridge playerContent 的标准通道,
  输出统一 `PlayResource` (携带 UA/Referer/Cookie headers, §20/§21);
- Android 侧不新增 Java endpoint (现有 /playerContent 即 BridgeClient 传输),
  后续若需要原生 Adapter 再扩展 APK。

### D3: 凭证持久化 = SQLite + AES-256-GCM

复用 `quantumtv.db` (rusqlite, 方案 §11 优先 SQLite), 新表 `cloud_credentials`,
凭证 JSON 以 AES-256-GCM 加密 (ring, 已在依赖闭包), 密钥文件
`<app_data>/cloud_cred.key` 本机随机生成 (§12 不落明文)。

### D4: verify_login 为登录确认后的强制步骤 (§6/§13/§16)

Confirmed → 保存凭证 → 调网盘账号 API 校验业务码 (百度 xpan uinfo / 夸克
clouddrive user / UC clouddrive user) → 业务码非 0 即 Authenticated 不成立,
前端展示"登录确认成功但验证失败"。播放能力验证 (share/resolve) 作为
"测试连接"的分步检查项输出 (§31), 不阻塞登录。

### D5: PlaybackResourceStore = gateway ResourceSession 扩展

session 增加 provider / RefreshHint(source+flag+episode+class) / expires_at_ms;
上游 401/403/410 或 expires 到期 → 用 RefreshHint 重新 resolve 一次 (仅
playerContent, 严禁 Search, §27) → 原地替换 session → 重试请求一次。

### D6: 时间戳用 i64 unix_ms, 不引入 chrono

chrono 不在本仓 Cargo.lock, 避免离线解析风险; expires_at 语义不变。

## 3. 与方案条目的偏差

- §3 trait 的 `get_share/get_file` 拆分: 受限于 wex spider 单次 playerContent
  完成 share→file→dlink 的事实 (D2), trait 以 `parse_share + resolve_play_url`
  表达同一链路; capabilities.file_list 如实报 false。
- §35 bridge.cloud.* 标准化端点、§36 Android CloudDriveAdapter: 本轮以现有
  /playerContent /setCookie 作为传输 (功能等价), APK 侧改造留待后续。
- §9 轮询节奏由前端实现 (1s×10 → 2s 至 60s → 过期), 后端 poll 保持无状态。

## 4. 验收对照

- 百度不回归: 登录/推送/播放链路未改动既有行为 (qrcodelogin 原实现保留,
  clouddrive 层委托之)。
- 夸克/UC: 扫码 → 保存(加密) → verify(业务码) → 推送 bridge → 状态展示;
  "测试连接"给出分步结果; 播放 401/403 自动 refresh 重试。
- 场景 A (重启免扫码): 启动时 CloudDriveManager 从 SQLite 恢复并重推 cookie 到 bridge。
