# Migration Plan

## Phase 0: 建立基线

先不要重写。

记录现有：

- 播放入口
- mpv 调用位置
- WebView 播放逻辑
- Android Bridge
- 网盘解析
- Proxy
- Tauri Commands
- 播放历史

目标：找到耦合点。

## Phase 1: MediaResource

新增统一模型。

把现有各种播放结果转换成：

```text
RawPlayResult -> MediaResource
```

此阶段播放器暂时不改。

验收：

- 普通 MP4 可以转换
- M3U8 可以转换
- 网盘资源可以转换
- Header/Cookie 可以保留

## Phase 2: Resolver

建立 ResolverManager。

把现有：

```text
TVBox
Spider
Android Bridge
Netdisk
```

全部接入 Resolver。

验收：

同一集无论来自哪个 Source，最终都得到相同类型的 MediaResource。

## Phase 3: Gateway

将现有 localhost proxy 重构成 PlaybackGateway。

重点：

- Range
- Header
- Cookie
- Redirect
- TTL
- token

## Phase 4: PlaybackManager

统一 mpv 生命周期。

UI 不再直接操作 mpv。

## Phase 5: 删除双播放器

逐步删除：

- HTML5 播放兜底
- mpv fallback
- 两套状态同步
- 播放器之间切换逻辑

最终：

```text
MediaResource -> PlaybackManager -> mpv
```

Phase 5 落地记录 (v0.9.0):

- 播放编排收口 Rust: 新增 `playback_play_episode(source, flag, episodeId)`
  命令, 内部走 ResolverManager → Gateway.wrap_resource → PlaybackManager。
  前端只传"哪一集", 不再判定直链/解析 raw id/包装代理地址。
- 前端删 HTML5/Plyr/HLS 播放器整条路径, mpv 是唯一播放引擎
  (03-playback.md §7); 页面只剩遥控面板 + 列表 UI, 状态订阅
  `playback_state` 事件。
- 兼容期: `mpv_embed_*` 命令与 `mpv-embed-event` 事件保留 (speed/volume
  面板与旧事件消费者), Phase 6 删除。

## Phase 6: 稳定性

增加：

- 超时
- 进程异常退出检测
- mpv 自动重启
- resource session 清理
- 播放错误分类
- 日志

Phase 6 落地记录 (v0.9.0, 详见 11-refactor-checklist.md):

- 进程异常退出检测 + mpv 自动重启: Phase 4 已引入 (ProcessDead 事件 +
  recover_after_crash), Phase 6 修复判定 bug — 原实现读已被 ProcessDead
  重置回 Idle 的 state, 活跃态条件永假导致 recovery 实际不触发; 现改为
  pre_dead 死亡前快照判定, 断点续播位置同样取自快照。
- 播放错误分类: ResolverError 八类 (Phase 2) + end-file reason=error →
  PlaybackError → playback_error 事件 (Phase 4)。
- resource session 清理: Gateway TTL 120s 空闲过期 + 访问续期 (Phase 3)。
- 超时 (mpv IPC 命令级) 未实现, 留待 Phase 7/按需补充。
- 同步完成 Cleanup 清单: 旧播放器路径 (mpv_embed.rs / mpv_player.rs)、
  重复命令 (mpv_embed_* / resolve_spider_episode)、无用依赖 (hls.js /
  plyr) 删除; mpv-embed-event 兼容翻译移除。

## Phase 7: 可选优化

只有在独立 mpv 窗口已经稳定后，再评估 libmpv embedding。

Phase 7 落地记录 (v0.9.0, 详见 11-refactor-checklist.md):

- 超时 (mpv IPC 命令级) 已实现: send_command 带 request_id, 命令实际写入
  管道后登记 awaiting + 5s 看门狗; 读循环按 request_id 派发响应; 超时或
  mpv 报错 → CommandFailed 事件, loadfile 在活跃态转 Error (用户可见可
  重试), 控制命令只留日志。排队期 (管道连接最长 10s) 不计时, 避免慢连接
  误报; Idle 下的迟到超时不劫持状态。
- Gateway Redirect 显式策略: netdisk_proxy::client (与 PlaybackGateway
  共用) 显式 Policy::limited(5), 补 Phase 3 遗留配置项, 跟随语义不变。
- libmpv embedding: 继续冻结 (ADR-004)。独立窗口方案 C 已稳定, 嵌入的
  跨平台渲染/GPU 集成风险无对应收益, 评估结论为不启动。

## 每阶段原则

每完成一个阶段都必须保证：

- 项目可以编译
- 核心播放功能可用
- 不破坏已有 Source
- 不引入新的播放器分支
