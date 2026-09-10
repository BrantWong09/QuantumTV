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

- [ ] 重构 netdisk proxy 为 PlaybackGateway
- [ ] ResourceSession
- [ ] opaque token
- [ ] Range
- [ ] HEAD
- [ ] Redirect
- [ ] Header/Cookie
- [ ] TTL
- [ ] localhost-only
- [ ] SSRF 防护

## Phase 4: Playback

- [ ] PlaybackManager
- [ ] PlaybackState
- [ ] MpvBackend
- [ ] mpv JSON IPC
- [ ] load/play/pause/stop/seek
- [ ] duration/time-pos
- [ ] error
- [ ] mpv crash recovery

## Phase 5: UI

- [ ] play/page.tsx 只保留 UI orchestration
- [ ] 删除 resolve 逻辑
- [ ] 删除 HLS 播放逻辑
- [ ] 删除直接 mpv IPC
- [ ] 删除第二套 playback state
- [ ] UI 通过 Tauri IPC 控制播放

## Phase 6: Cleanup

- [ ] 删除旧播放器路径
- [ ] 删除重复 command
- [ ] 删除无用依赖
- [ ] 更新文档
- [ ] 更新测试
- [ ] 架构依赖检查
