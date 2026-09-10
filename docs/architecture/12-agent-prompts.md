# AI Coding Agent Prompt Templates

## 总规则

AI 开始任何 QuantumTV 重构任务前，必须阅读：

```text
docs/architecture/README.md
docs/architecture/00-current-state.md
docs/architecture/08-ai-implementation.md
docs/architecture/07-migration.md
```

如果任务涉及具体模块，还必须阅读对应专项文档。

## Prompt 1: 分析现状

```text
请先不要修改代码。

阅读 docs/architecture/ 下的架构文档，并分析当前仓库。

目标：
1. 找出当前功能对应的代码入口。
2. 画出当前调用链。
3. 判断每个代码文件属于 Source / Resolver / Media / Gateway / Playback / UI 哪一层。
4. 找出与目标架构冲突的地方。
5. 给出最小迁移方案。

禁止：
- 修改代码
- 顺手重构无关模块
- 推测不存在的代码
```

## Prompt 2: 实现一个阶段

```text
按照 docs/architecture/07-migration.md 执行当前阶段。

要求：
1. 先检查现有实现。
2. 只实现当前阶段。
3. 保持旧功能可用。
4. 不修改无关模块。
5. 完成后运行编译和相关测试。
6. 报告修改文件、调用链变化和测试结果。

如果发现现有代码与文档不一致：
先说明差异，再采用最小兼容方案，不要擅自改变总体架构。
```

## Prompt 3: MediaResource

```text
实现 MediaResource。

要求：
- 参考 01-core-model.md。
- 不修改播放器。
- 不修改 Next.js UI。
- 将现有播放结果增加到 MediaResource 转换层。
- 保留 Header/Cookie/Referer/User-Agent。
- 增加单元测试。

完成后说明：
1. 新增类型
2. 转换入口
3. 哪些旧代码仍在使用
4. 测试结果
```

## Prompt 4: Resolver

```text
实现 ResolverManager 和当前指定 Resolver。

严格遵循 02-resolver.md。

Resolver 只能负责：
Raw Input -> MediaResource

禁止：
- 调用 mpv
- 操作 UI
- 修改 React state
- 直接控制播放窗口

完成后验证同一 Episode 能够从 Resolver 得到 MediaResource。
```

## Prompt 5: Playback

```text
实现 PlaybackManager。

严格遵循 03-playback.md。

PlaybackManager 是唯一播放器控制入口。

禁止：
- Resolver 调用 mpv
- UI 直接访问 mpv IPC
- 增加第二播放器
- 新增 HTML5 fallback

优先使用独立 mpv 进程 + JSON IPC。
```

## Prompt 6: UI 清理

```text
将 play/page.tsx 改造成纯 UI orchestration。

目标：
Episode -> resolve_episode -> MediaResource -> playback.play

页面不得：
- 解析 TVBox
- 调用 Spider
- 拼接网盘 URL
- 直接调用 mpv
- 维护第二套真实播放状态

保留：
- UI
- 用户操作
- Tauri IPC
- 播放状态展示
```

## Prompt 7: 最终检查

```text
请进行 QuantumTV V2 架构审计，不修改代码。

检查：
1. UI 是否直接依赖播放器？
2. Resolver 是否依赖播放器？
3. Source 是否依赖播放器？
4. Gateway 是否与播放器职责分离？
5. 是否仍存在第二播放器？
6. 是否存在重复 Playback State？
7. 新增网盘是否需要修改 Player？
8. 新增 Source 是否需要修改 Player？
9. 依赖方向是否符合 06-project-structure.md？

输出：
- PASS 项
- FAIL 项
- 风险
- 建议修复顺序
