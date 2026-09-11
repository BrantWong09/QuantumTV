# QuantumTV V2 Refactor Checklist

## Phase 0

- [ ] 记录当前播放链路
- [ ] 记录所有 mpv command
- [ ] 记录所有 HTML5/Plyr/HLS 入口
- [ ] 记录 Spider playerContent 调用
- [ ] 记录 netdisk proxy 调用
- [ ] 建立回归测试资源

## Phase 1: MediaResource

- [x] 新增 `MediaResource`（crates/core/src/media.rs）
- [x] 新增 `ResourceType`（含 URL 特征推断 detect）
- [x] 新增 `SubtitleResource`
- [x] 新增 `MediaMetadata`
- [x] 增加 TypeScript 类型（src/lib/types.ts，未接线）
- [x] 普通 MP4 转换测试
- [x] HLS 转换测试
- [x] Header/Cookie 测试
- [x] 网盘结果转换测试（含本地代理伪装形态 + 百度 UA 场景）

## Phase 2: Resolver

- [x] 新增 `ResolveInput`（crates/core/src/resolver.rs）
- [x] 新增 `Resolver` trait（async_trait）
- [x] 新增 `ResolverManager`（按注册顺序派发）
- [x] DirectResolver（吃掉前端 isDirectPlayableUrl 判定）
- [x] SpiderResolver（吃掉 resolve_spider_episode 的 unwrap+wrap 序列；bridge 细节经 SpiderPlayFetcher 注入）
- [ ] AndroidSpiderResolver（当前由 SpiderResolver + BridgeSpiderPlayFetcher 承担同一职责，bridge 直连形态留待需要时拆分）
- [x] NetdiskResolver（扩展位，can_resolve=false 待 Phase 3+ 直连网盘 API）
- [x] 统一错误模型（八类 ResolveError + Display）

## Phase 3: Gateway

- [x] 重构 netdisk proxy 为 PlaybackGateway（新增 crates/core/src/gateway.rs，与旧 netdisk_proxy 共用监听；旧 /netdisk 路径兼容并存）
- [x] ResourceSession
- [x] opaque token（uuid v4 simple，32 位不可枚举）
- [x] Range（透传 Content-Range/206/Accept-Ranges）
- [x] HEAD
- [x] Redirect（显式策略 Policy::limited(5)，Phase 7 补配置；网盘 302 → CDN 跟随语义不变）
- [x] Header/Cookie（UA/Referer/Cookie/自定义头注入，CRLF 防注入）
- [x] TTL（120s 空闲过期 + 访问续期 + 过期计数）
- [x] localhost-only（沿用 127.0.0.1 随机端口绑定）
- [x] SSRF 防护（仅公网 http(s)，禁环回/私网/链路本地/元数据端点/本地文件）

## Phase 4: Playback

- [x] PlaybackManager（crates/core/src/playback/manager.rs，播放控制唯一入口 + 进度保存节流）
- [x] PlaybackState（crates/core/src/playback/state.rs，状态机迁移纯函数 + 单测）
- [x] MpvBackend（crates/core/src/playback/mpv_backend.rs，从 src-tauri/commands/mpv_embed.rs 迁入；Core 不依赖 tauri，事件经回调上抛）
- [x] mpv JSON IPC（--input-ipc-server 命名管道，observe_property 订阅 playback-time/duration/pause/eof-reached）
- [x] load/play/pause/stop/seek（playback_play/pause/set_paused/stop/seek 命令 + manager seek_relative/seek_absolute）
- [x] duration/time-pos（playback_time / playback_duration 事件，Time 500ms 节流沿用）
- [x] error（end-file reason=error → PlaybackError → playback_error 事件 + 状态 Error）
- [x] mpv crash recovery（recover_after_crash：非用户关闭 + 活跃态 + max_crash_restarts 内自动重拉断点续播）
- [x] src-tauri 接线（commands/playback.rs：PlaybackManagerState + playback_* 命令；旧 mpv_embed_* 命令委托同一 manager，旧 mpv-embed-event 兼容翻译）

## Phase 5: UI

- [x] play/page.tsx 只保留 UI orchestration（3725 → ~1560 行, 播放编排经 playback_play_episode 在 Rust 侧）
- [x] 删除 resolve 逻辑（isDirectPlayableUrl / resolveEpisodeUrl / resolveTokenRef 前端判定全删, 解析链在 ResolverManager）
- [x] 删除 HLS 播放逻辑（hls.js / TauriHlsJsLoader / fetch_m3u8 前端路径全删; m3u8 由 mpv 直接拉流）
- [x] 删除直接 mpv IPC（前端 mpv_embed_command 透传全删, 控制经 playback_* 命令; speed/volume 走 mpv_embed_command 委托版）
- [x] 删除第二套 playback state（mpvState useState 全删, 订阅 playback_state 事件, TS PlaybackState 类型接线）
- [x] UI 通过 Tauri IPC 控制播放（playback_play_episode/pause/set_paused/seek/add_volume/stop/playback_state）
- [x] 同步删除: Plyr 整条路径 (initPlyr/enhancePlyrUi/手势层/音量增强/TauriHlsJsLoader/触屏滑动快进)、HEVC 黑屏兜底切 mpv (mpv 为唯一播放器, 无需兜底)、plyrReloadTick 双状态切换

## Phase 5 兼容期残留 (Phase 6 删除)

- [x] mpv_embed_launch/command/close 委托命令 (speed/volume 已换 playback_set_speed/playback_set_volume)
- [x] mpv-embed-event 兼容翻译 (emit_playback_event 内)
- [x] src-tauri/src/commands/mpv_embed.rs 旧实现文件
- [x] resolve_spider_episode 命令 (播放编排已迁 playback_play_episode)
- [x] mpv_player.rs launch_mpv HEVC 兜底路径 (mpv 为唯一播放器, 兜底无意义)

## Phase 6: Cleanup

- [x] 删除旧播放器路径 (mpv_embed.rs / mpv_player.rs / 前端 mpv_embed_command 直传)
- [x] 删除重复 command (mpv_embed_launch/command/close, resolve_spider_episode)
- [x] 删除无用依赖 (hls.js, plyr + globals.css Plyr 样式段 ~620 行)
- [x] 更新文档 (本清单 + 模块级注释; playback.rs 头注释去兼容期说明)
- [x] 更新测试 (crash recovery 改为纯同步判定 consume_crash_restart, 不再依赖 mpv 环境; 新增片尾不续播用例; core 122 tests 全绿)
- [x] 架构依赖检查 (quantumtv-core 无 tauri 依赖, grep 验证)
- [x] 修复存量 bug: crash recovery 读已被 ProcessDead 重置的 state → 活跃态判定永假。改为 pre_dead 死亡前快照判定
- [x] 清理 dead code: commands/playback.rs app_data_dir / tauri::Manager unused import
- [x] 补充: 修复 gateway 测试并行 flaky (SESSIONS 全局 map 被并行测试污染, 触碰共享 map 的测试加锁串行; gateway 模块 10 次全绿)
- [x] 补充: 删除被跟踪的遗留 package-lock.json (项目用 pnpm, 该文件仍含已删的 hls.js/plyr)

### Phase 6 备注

- 已知存量问题 (非本次引入): src-tauri `cargo test --lib` 测试二进制启动失败
  (STATUS_ENTRYPOINT_NOT_FOUND), 因 windows-gnu 工具链下 tauri-build 嵌入
  manifest 与 rustc 默认 manifest 冲突 (链接期 ".rsrc merge failure: multiple
  non-default manifests")。与 Phase 6 改动无关 (HEAD 同样复现), 待单独处理。
- 前端 jest 无测试文件 (No tests found, 存量状态)。
- lint:strict 45 warnings 全部为存量 (改动文件 0 warning, 0 error)。

## Phase 7: 可选优化

- [x] mpv IPC 命令级超时 (send_command 带 request_id, 写入管道后登记 awaiting
  + 5s 看门狗; 响应由读循环按 request_id 派发; 超时/mpv 报错 → CommandFailed
  事件; loadfile 活跃态转 Error 用户可见可重试, 控制命令仅日志; Idle 下迟到
  超时忽略; 排队期不计时避免管道慢连接误报)
- [x] Gateway Redirect 显式策略 (netdisk_proxy::client Policy::limited(5),
  与网盘代理共用连接池同语义)
- [x] libmpv embedding 评估结论: 继续冻结 (ADR-004; 独立窗口方案 C 已稳定,
  嵌入的跨平台渲染/GPU 集成风险无对应收益)
- [x] AndroidSpiderResolver 拆分: 维持 SpiderResolver + BridgeSpiderPlayFetcher
  承担同一职责, bridge 直连形态仍留待需要时拆分 (非优化项, 登记决策)
- [x] 回归: core 131 tests 全绿 (新增 9: 状态机 3 + 响应关联 3 + 超时 2 + 排队 1),
  src-tauri cargo check 通过

### Phase 7 备注

- 超时值为常量 COMMAND_TIMEOUT_MS_DEFAULT = 5000 (mpv_backend.rs), 测试可注入。
