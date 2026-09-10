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

## Phase 6: 稳定性

增加：

- 超时
- 进程异常退出检测
- mpv 自动重启
- resource session 清理
- 播放错误分类
- 日志

## Phase 7: 可选优化

只有在独立 mpv 窗口已经稳定后，再评估 libmpv embedding。

## 每阶段原则

每完成一个阶段都必须保证：

- 项目可以编译
- 核心播放功能可用
- 不破坏已有 Source
- 不引入新的播放器分支
