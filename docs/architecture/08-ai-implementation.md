# AI Implementation Guide

本文档用于让 AI Coding Agent 按架构执行 QuantumTV 重构。

## 1. 总规则

AI 修改代码前必须：

1. 阅读 `docs/architecture/README.md`
2. 阅读与当前任务相关的专项文档
3. 搜索现有实现
4. 判断现有代码属于 Source、Resolver、Media、Gateway、Playback 还是 UI
5. 优先做最小改动
6. 不因为架构重构而顺手重写无关代码

## 2. 任务拆分

不要一次要求 AI：

> “重构整个 QuantumTV 播放器。”

应该拆成：

```text
Task 1: 增加 MediaResource
Task 2: 增加 DirectResolver
Task 3: 接入 SpiderResolver
Task 4: 接入 AndroidSpiderResolver
Task 5: 重构 Gateway
Task 6: 增加 PlaybackManager
Task 7: 接入 mpv IPC
Task 8: 删除旧播放器路径
```

## 3. 修改前必须回答

AI 在实施任务前应明确：

- 当前代码入口是什么？
- 当前调用链是什么？
- 哪个模块拥有这个职责？
- 修改后调用链是什么？
- 是否需要兼容旧接口？
- 如何验证？

## 4. 架构约束

AI 不得：

- 在 React 中解析 TVBox
- 在 Resolver 中调用 mpv
- 在 Spider 中控制播放器
- 新增第二个播放器
- 把真实 URL 和 Cookie 无必要地暴露给前端
- 将所有逻辑塞进一个 Rust module
- 为了通过编译而删除已有功能

## 5. 推荐实施方式

每个任务按照：

```text
分析
  ↓
设计
  ↓
最小实现
  ↓
编译
  ↓
测试
  ↓
检查依赖方向
```

## 6. 验收重点

每次修改至少验证：

### Source

能搜索、详情、选集。

### Resolver

Episode 能解析为 MediaResource。

### Gateway

需要代理的资源可以 Range 播放。

### Playback

mpv 可以：

- load
- play
- pause
- seek
- stop

### UI

可以：

- 点击选集
- 开始播放
- 显示进度
- 暂停
- seek
- 下一集

## 7. 出现问题时

优先定位问题属于：

```text
Source
Resolver
MediaResource
Gateway
PlaybackManager
mpv
UI
```

不要直接修改播放器。

## 8. Definition of Done

一个功能完成的标准：

- 架构边界正确
- 无重复播放器逻辑
- 错误可定位
- 有最小测试
- 编译通过
- 核心功能没有退化
- 文档同步更新
