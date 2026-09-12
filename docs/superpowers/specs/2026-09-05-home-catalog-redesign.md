# 首页目录化重构设计

日期：2026-09-05
状态：待用户审阅

## 1. 背景与目标

- **变更 A（点击行为）**：首页原本点击卡片会跳到播放页、由播放页全站搜索后"找到一个合适的直接播放"。现改为：点击卡片直接进入**搜索页**。
- **变更 B（首页内容来源）**：首页内容从"豆瓣固定四栏（热门电影/剧集/新番/综艺）"改为**由订阅源目录结构驱动**。用户已全面适配 TVBox，各订阅源不一定具备电影/剧集的固定结构，因此首页应像 TVBox 一样，自动拉取订阅源里的目录并渲染，不做硬编码分类。
- **变更 C（VideoCard 点击规则统一）**：所有渲染出来的卡片遵循统一规则——有播放源（`source` + `id`）则直接播放；否则（首页目录卡片、豆瓣、推荐、聚合无源结果）一律走搜索页 `/search?q=<标题>`。

## 2. 现状（已确认的代码事实）

| 能力 | 位置 | 状态 |
| --- | --- | --- |
| CMS 源分类列表 | `?ac=list` → `parse_source_categories`（`src-tauri/src/commands/video.rs:860`） | 已有 |
| CMS 源分类下视频 | `?ac=videolist&t={type_id}&pg={page}`（`video.rs:1676`） | 已有 |
| 源分类过滤/下发 | `get_filtered_source_categories`（`douban_client.rs:1969`） | 已有 |
| Spider 源搜索/详情 | `spider_search` / `spider_detail`（`crates/core/src/spider/mod.rs:266,283`） | 已有 |
| Spider 源 homeContent | `spider_bridge_home`（`mod.rs:331`） | 仅 Android 桥接路径，JVM 无此能力 |
| 首页 | `src/app/page.tsx` | 豆瓣四栏 + 继续观看 + 为你推荐 + 收藏夹 Tab |
| 搜索页 | `src/app/search/page.tsx` | 完整聚合搜索（流式/过滤/聚合），复用 |
| VideoCard 点击 | `src/components/VideoCard.tsx:269` | douban/recommendation/聚合无源 → `/play?title=`；有 source+id → 直播 |

**关键约束**：Spider 站点（type=3）在 JVM 路径下**只有 search / detail 两个 action**，没有 homeContent / categoryContent 能力（`run_java` 只传 "search" 和 "detail"）。因此 Spider 源**无法提供目录**。

## 3. 用户决策（已确认）

1. 首页**完全**由订阅源目录生成行，替换掉豆瓣四栏（继续观看、为你推荐、收藏夹 Tab 保留）。
2. 卡片点击规则：有播放源 → 直播；否则 → 搜索页。
3. 源切换器显示**所有**已启用源，默认选第一个；Spider 源无目录，选中后**静默跳转搜索页**（方案 C）；目录按源**缓存**，切换/首屏加载时替换。

## 4. 架构设计

### 4.1 后端：新增 Tauri 命令 `get_home_catalog`

```ts
// 参数
source_key: string

// 返回
interface HomeCatalogResponse {
  source_key: string;
  source_name: string;
  site_type: 1 | 3;          // 1=CMS, 3=Spider
  categories: CategoryRow[]; // Spider 源为空数组
}
interface CategoryRow {
  type_id: string;
  type_name: string;
  list: VideoCardItem[];     // 该分类前 N 条，默认 8
}
interface VideoCardItem {
  id: string;
  title: string;
  poster: string;
  year?: string;
  episodes?: string[];
  source_name: string;
  class?: string;
}
```

**执行路径（按 `site_type` 分流）**：

- **CMS（type=1）**：
  1. `GET {api}?ac=list` → `parse_source_categories` 得到分类列表（`type_id` / `type_name` / `type_pid`）。
  2. 对每个分类**并行** `GET {api}?ac=videolist&t={type_id}&pg=1`，每条结果取前 `N=8` 条。
  3. 单请求超时 6s，失败分类降级为空 list，不影响其它分类。
- **Spider（type=3）**：直接返回 `categories: []`（无目录能力）。

**缓存**：内存 `HashMap<source_key, (HomeCatalogResponse, Instant)>`，TTL 5 分钟。命中直接返回；过期删除后重新拉取。缓存随应用生命周期存在，不持久化（与现有搜索缓存风格一致）。

**复用**：直接调用现有 `parse_source_categories`、`resolve_enabled_source`、`get_video_client`，不新增底层能力。

### 4.2 前端：重写首页 `src/app/page.tsx`

**布局**：
- 顶部：**源切换器**（胶囊/下拉），列出所有已启用源（`get_config` 的 `SourceConfig`，过滤 `disabled`），默认选第一个。选中项持久化到 `localStorage`（key: `home.selectedSourceKey`）。
- 正文：按选中源的 `categories` 渲染，每个分类一行 `ScrollableRow`，行内视频卡片。
- 保留：继续观看、为你推荐、收藏夹 Tab（不动）。

**数据流**：
1. 首页加载 → 读取源列表 → 默认选中第一个源 key（按 `SourceConfig` 数组顺序）。
2. 调 `get_home_catalog(selectedKey)` 拿目录。
3. 前端维护内存缓存 `Map<source_key, HomeCatalogResponse>`（组件 state）。切换源时：命中缓存 → 直接用，无命中 → 调命令（后端另有 TTL 缓存）。
4. 若选中源 `categories.length === 0`（Spider 源，或 CMS 源拉取失败）→ `router.push('/search')` 静默跳转搜索页。

> 边界情况：若默认第一个源恰好是 Spider 源，首页加载后会立即重定向到 `/search`。这是用户决策（默认所有源第一个 + Spider 走 C）的自然结果，不做特殊处理。

**卡片点击**：复用 `VideoCard`，`from='home'`，由 4.3 规则统一走 `/search?q=<title>`。

### 4.3 VideoCard 点击规则统一（`src/components/VideoCard.tsx:269`）

新规则（替换现有分支）：

```ts
if (actualSource && actualId) {
  // 有具体源 + id → 直接播放
  router.push(`/play?source=...&id=...&title=...`);
} else {
  // 无具体源 → 搜索页（首页目录卡片、豆瓣、推荐、聚合无源）
  router.push(`/search?q=<title>`);
}
```

影响的 `from`：
- 新增 `home`（需同步扩展 `VideoCardProps.from` 联合类型：`'playrecord' | 'favorite' | 'search' | 'douban' | 'recommendation' | 'home'`）→ 搜索页。
- `douban` → 由 `/play?title=` 改为 `/search?q=`。
- `recommendation` → 由 `/play?title=` 改为 `/search?q=`。
- `search` / `playrecord` / `favorite` → 有 source+id，仍直接播放（不变）。
- 聚合无源（`isAggregate && !source && !id`）→ 搜索页（原为 `/play?title=`，语义等价，路径统一）。

移动端长按菜单中的"播放"动作同步指向搜索页（与点击一致）。

### 4.4 涉及文件

| 文件 | 变更 |
| --- | --- |
| `src-tauri/src/commands/video.rs`（或新增 `home_catalog.rs`） | 新增 `get_home_catalog` 命令 + 内存缓存 |
| `src-tauri/src/lib.rs` | 注册命令 |
| `src/app/page.tsx` | 重写首页：源切换器 + 目录行；移除豆瓣四栏 |
| `src/components/VideoCard.tsx` | `from='home'`；点击规则统一为"有源直播 / 无源搜索" |
| `src/lib/types.ts` | 新增 `HomeCatalogResponse`、`CategoryRow`、`VideoCardItem` 类型 |
| `src-tauri/src/commands/mod.rs` | 若新文件，注册模块 |

## 5. 缓存策略

- **后端**：`get_home_catalog` 内存缓存，key=`source_key`，TTL 5 分钟。切换源命中即返回。
- **前端**：`Map<source_key, HomeCatalogResponse>` 组件内缓存，切换源无命中才发命令，保证切换瞬时。
- **源选择**：`localStorage` 持久化，刷新后保持上次选择。

## 6. 错误处理

- 源列表为空：首页展示空态"暂无可用订阅源，请前往管理页配置"。
- `?ac=list` 失败/非 JSON：该源 `categories: []`，首页重定向搜索页（与 Spider 同处理）。
- 单个分类 `ac=videolist` 失败：该分类 list 降级为空，其它分类正常。
- `get_home_catalog` 整体异常：前端 catch 后回退到空目录并重定向搜索页，避免白屏。

## 7. 测试要点

- CMS 源：`get_home_catalog` 返回非空 categories，每行 list ≤ 8 条。
- Spider 源：返回 `site_type=3, categories=[]`。
- 缓存：同源二次调用不产生新的 HTTP 请求（TTL 内）。
- 首页：默认选中第一个源；切换源替换目录；Spider 源重定向 `/search`。
- VideoCard：有 source+id → `/play`；无 source+id（home/douban/recommendation）→ `/search?q=`。

## 8. 待确认（无）

本 spec 已与用户确认的三点决策一致，无 TBD。