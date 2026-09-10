# QuantumTV AI Coding Rules

本文件是 QuantumTV AI Coding Agent 的最高级项目级约束之一。

## Before coding

必须阅读：

```text
docs/architecture/README.md
docs/architecture/00-current-state.md
docs/architecture/08-ai-implementation.md
```

## Architecture rules

- Source 不调用 Player。
- Resolver 不调用 Player。
- UI 不直接操作 mpv。
- 所有播放资源统一为 MediaResource。
- mpv 是唯一正式播放器。
- Gateway 与 Resolver/Player 保持职责分离。
- 不增加第二套播放状态。
- 不为了修复局部问题扩大模块职责。

## Change rules

- 先搜索，再修改。
- 先理解调用链，再改接口。
- 最小改动优先。
- 一个阶段完成后编译/测试。
- 不要顺手重写无关代码。
- 如果代码和文档冲突，先报告差异。

## Refactor rule

禁止一次性进行“大重构”。

必须按：

```text
MediaResource
→ Resolver
→ Gateway
→ PlaybackManager
→ UI migration
→ cleanup
```

逐阶段执行。
