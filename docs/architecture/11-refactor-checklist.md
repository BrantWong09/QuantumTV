# QuantumTV V2 Refactor Checklist

## Phase 0

- [ ] 记录当前播放链路
- [ ] 记录所有 mpv command
- [ ] 记录所有 HTML5/Plyr/HLS 入口
- [ ] 记录 Spider playerContent 调用
- [ ] 记录 netdisk proxy 调用
- [ ] 建立回归测试资源

## Phase 1: MediaResource

- [ ] 新增 `MediaResource`
- [ ] 新增 `ResourceType`
- [ ] 新增 `SubtitleResource`
- [ ] 新增 `MediaMetadata`
- [ ] 增加 TypeScript 类型
- [ ] 普通 MP4 转换测试
- [ ] HLS 转换测试
- [ ] Header/Cookie 测试
- [ ] 网盘结果转换测试

## Phase 2: Resolver

- [ ] 新增 `ResolveInput`
- [ ] 新增 `Resolver` trait
- [ ] 新增 `ResolverManager`
- [ ] DirectResolver
- [ ] SpiderResolver
- [ ] AndroidSpiderResolver
- [ ] NetdiskResolver
- [ ] 统一错误模型

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
