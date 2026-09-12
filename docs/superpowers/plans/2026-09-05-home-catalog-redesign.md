# 首页目录化重构 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 首页改为由订阅源目录结构驱动：源切换器（默认第一个源）+ 各分类视频行；点击首页卡片一律跳搜索页；VideoCard 点击规则统一为"有 source+id 直播，否则搜索"；Spider 源无目录，静默跳转搜索页。

**Architecture:** 后端新增复合命令 `get_home_catalog(source_key)`：复用现有 `resolve_enabled_source` + `?ac=class`（分类）+ `?ac=videolist&t=&pg=1`（分类下视频），CMS 源并行拉取、每分类截前 8 条，Spider 源返回空目录；带内存 TTL 缓存（300s）。前端首页重写为源切换器 + 目录行，复用 `useSourceFilter` 源列表与 `VideoCard`。点击规则：`from==='home'` 走搜索；`actualSource && actualId` 走直播；其它走搜索。

**Tech Stack:** Rust (tauri commands, reqwest, tokio spawn), TypeScript/React (Next.js, Tauri invoke), CSS（复用现有 `ScrollableRow` / `appLayoutClasses` / glass-chip 样式）

**Spec:** `docs/superpowers/specs/2026-09-05-home-catalog-redesign.md`

**Global Constraints:**
- 首页必须完全由订阅源目录生成，替换豆瓣四栏；继续观看 / 为你推荐 / 收藏夹 Tab 保留不动。
- 首页卡片点击一律跳 `/search?q=<title>`。
- 非首页卡片：有 `source`+`id` → 直接播放 `/play?source=&id=&title=`；否则 → `/search?q=`。
- 源切换器显示所有已启用源，默认第一个，选中持久化到 `localStorage`（key `home.selectedSourceKey`）。
- Spider 源（`site_type==3`）无目录，`categories` 为空，首页静默 `router.push('/search')`。
- 目录按源缓存：后端 TTL 300s；前端 `Map<source_key, response>` 组件内缓存。
- 仅 CMS 源（`site_type==1`）走 `?ac=class` + `?ac=videolist`；每分类最多 8 条；单请求失败降级为空，不影响其它分类。
- 构建通道：Rust 用 `cd src-tauri && cargo check`（crate 名 `quantumtv`，不在 workspace 内）；前端用 `npm run typecheck` 与 `npm run lint`。

---

### Task 1: 新增 `get_home_catalog` 命令 + 内存缓存（Rust）

**Files:**
- Modify: `src-tauri/src/commands/video.rs`（新增 structs、command、cache、helper）
- Test: 无（纯集成命令；helper 单独在 Task 1b 覆盖）

**Interfaces:**
- Produces: 命令 `get_home_catalog(source_key: String, storage: State<StorageManager>, db: State<Db>) -> Result<HomeCatalogResponse, String>`
- Produces: 类型 `HomeCatalogResponse` / `HomeCategoryRow` / `HomeVideoCard`（供前端 TS 类型与 Task 4 消费）
- Consumes: 复用现有 `get_config_with_db_sources`、`resolve_enabled_source`、`source_url`、`get_video_client`、`parse_source_categories`、`parse_source_videos`、`parse_episodes_for`

- [ ] **Step 1: 在 video.rs 末尾追加 structs 与常量**

在 `src-tauri/src/commands/video.rs` 文件末尾（最后一个函数之后）追加：

```rust
// ---------- 首页目录（Home Catalog） ----------

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HomeCatalogResponse {
    pub source_key: String,
    pub source_name: String,
    pub site_type: i32,
    pub categories: Vec<HomeCategoryRow>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HomeCategoryRow {
    pub type_id: String,
    pub type_name: String,
    pub list: Vec<HomeVideoCard>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HomeVideoCard {
    pub id: String,
    pub title: String,
    pub poster: String,
    pub year: Option<String>,
    pub episodes: Vec<String>,
    pub class: Option<String>,
    pub source: String,
    pub source_name: String,
}

const HOME_CATALOG_MAX_VIDEOS_PER_CATEGORY: usize = 8;
const HOME_CATALOG_TTL_SECS: u64 = 300;

struct CachedHomeCatalog {
    response: HomeCatalogResponse,
    fetched_at: std::time::Instant,
}

static HOME_CATALOG_CACHE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, CachedHomeCatalog>>,
> = std::sync::OnceLock::new();

fn home_catalog_cache()
-> &'static std::sync::Mutex<std::collections::HashMap<String, CachedHomeCatalog>> {
    HOME_CATALOG_CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn stringify_category_type_id(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn api_search_item_to_home_card(item: ApiSearchItem, source: &ApiSite) -> HomeVideoCard {
    let (episodes, _) = parse_episodes_for(item.vod_play_url.as_deref().unwrap_or(""), false);
    HomeVideoCard {
        id: match item.vod_id {
            serde_json::Value::String(s) => s,
            serde_json::Value::Number(n) => n.to_string(),
            _ => String::new(),
        },
        title: item.vod_name.trim().to_string(),
        poster: item.vod_pic,
        year: item.vod_year,
        episodes,
        class: item.vod_class,
        source: source.key.clone(),
        source_name: source.name.clone(),
    }
}
```

- [ ] **Step 2: 追加 `get_home_catalog` 命令实现**

继续追加：

```rust
#[tauri::command]
pub async fn get_home_catalog(
    source_key: String,
    storage: State<'_, StorageManager>,
    db: State<'_, crate::db::db_client::Db>,
) -> Result<HomeCatalogResponse, String> {
    let config = get_config_with_db_sources(&storage, &db)?;
    let source = resolve_enabled_source(&config, &source_key)
        .ok_or_else(|| format!("Source not found or disabled: {}", source_key))?;

    let site_type = source.site_type.unwrap_or(1);

    // Spider 站点无目录能力，直接返回空
    if site_type == 3 {
        return Ok(HomeCatalogResponse {
            source_key: source.key,
            source_name: source.name,
            site_type: 3,
            categories: Vec::new(),
        });
    }

    // TTL 缓存命中
    {
        let map = home_catalog_cache().lock().unwrap();
        if let Some(cached) = map.get(&source_key) {
            if cached.fetched_at.elapsed().as_secs() < HOME_CATALOG_TTL_SECS {
                return Ok(cached.response.clone());
            }
        }
    }

    // 1) 拉取分类列表
    let class_url = source_url(&source.api, "?ac=class");
    let class_body = get_video_client()
        .get(&class_url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;

    let categories = parse_source_categories(&class_body).unwrap_or_default();

    // 2) 并行拉每个分类的视频（pg=1，截前 N 条）
    let client = get_video_client();
    let mut handles = Vec::with_capacity(categories.len());
    for (idx, cat) in categories.iter().enumerate() {
        let type_id = stringify_category_type_id(&cat.type_id);
        if type_id.is_empty() {
            continue;
        }
        let api = source.api.clone();
        let client = client.clone();
        let source_clone = source.clone();
        handles.push((idx, tokio::spawn(async move {
            let url = source_url(
                &api,
                &format!("?ac=videolist&t={}&pg=1", urlencoding::encode(&type_id)),
            );
            let resp = match client.get(&url).send().await {
                Ok(r) => r,
                Err(_) => return Vec::new(),
            };
            let resp = match resp.error_for_status() {
                Ok(r) => r,
                Err(_) => return Vec::new(),
            };
            let body = match resp.text().await {
                Ok(t) => t,
                Err(_) => return Vec::new(),
            };
            let items = parse_source_videos(&body).unwrap_or_default();
            let mut cards: Vec<HomeVideoCard> = items
                .into_iter()
                .take(HOME_CATALOG_MAX_VIDEOS_PER_CATEGORY)
                .map(|item| api_search_item_to_home_card(item, &source_clone))
                .collect();
            cards.retain(|c| !c.id.is_empty());
            cards
        })));
    }

    let mut indexed: Vec<(usize, Vec<HomeVideoCard>)> = Vec::new();
    for (idx, handle) in handles {
        match handle.await {
            Ok(cards) => indexed.push((idx, cards)),
            Err(_) => indexed.push((idx, Vec::new())),
        }
    }
    indexed.sort_by_key(|(i, _)| *i);

    let mut category_rows = Vec::with_capacity(categories.len());
    for (idx, cat) in categories.iter().enumerate() {
        let list = indexed
            .iter()
            .find(|(i, _)| *i == idx)
            .map(|(_, c)| c.clone())
            .unwrap_or_default();
        category_rows.push(HomeCategoryRow {
            type_id: stringify_category_type_id(&cat.type_id),
            type_name: cat.type_name.clone(),
            list,
        });
    }

    let response = HomeCatalogResponse {
        source_key: source.key,
        source_name: source.name,
        site_type,
        categories: category_rows,
    };

    // 写缓存
    {
        let mut map = home_catalog_cache().lock().unwrap();
        map.insert(
            source_key,
            CachedHomeCatalog {
                response: response.clone(),
                fetched_at: std::time::Instant::now(),
            },
        );
    }

    Ok(response)
}
```

- [ ] **Step 3: cargo check 验证编译**

Run: `cd "D:\Program Files (x86)\QuantumTV\src-tauri" && cargo check 2>&1 | tail -n 40`
Expected: 无 `error[...]`，末尾 `Finished` / `Compiling quantumtv` 成功。若出现 borrow/clone/类型错误，按编译器提示修正（常见：`response.clone()` 需 `HomeCatalogResponse: Clone`，已 derive）。

- [ ] **Step 4: 提交**

```bash
git add src-tauri/src/commands/video.rs
git commit -m "feat(home): add get_home_catalog command with TTL cache"
```

---

### Task 1b: 单元测试 helper（可选但推荐）

**Files:**
- Test: `src-tauri/src/commands/video.rs` 底部 `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: `api_search_item_to_home_card`、`stringify_category_type_id`

- [ ] **Step 1: 写测试并运行**

追加：

```rust
#[cfg(test)]
mod home_catalog_tests {
    use super::*;

    #[test]
    fn stringify_category_type_id_handles_number_and_string() {
        assert_eq!(stringify_category_type_id(&serde_json::json!(1)), "1");
        assert_eq!(
            stringify_category_type_id(&serde_json::json!("2")),
            "2"
        );
        assert_eq!(stringify_category_type_id(&serde_json::json!(null)), "");
    }

    #[test]
    fn api_search_item_to_home_card_maps_fields() {
        let source = ApiSite {
            key: "src1".to_string(),
            api: "http://x".to_string(),
            name: "SourceOne".to_string(),
            detail: None,
            is_adult: None,
            site_type: Some(1),
            spider: None,
            searchable: Some(1),
        };
        let item = ApiSearchItem {
            vod_id: serde_json::json!(42),
            vod_name: "  Test Movie  ".to_string(),
            vod_pic: "http://pic".to_string(),
            vod_remarks: None,
            vod_play_url: Some("http://a.m3u8$$$http://b.m3u8".to_string()),
            vod_class: Some("电影".to_string()),
            vod_year: Some("2020".to_string()),
            vod_content: None,
            vod_douban_id: None,
            type_name: None,
        };
        let card = api_search_item_to_home_card(item, &source);
        assert_eq!(card.id, "42");
        assert_eq!(card.title, "Test Movie");
        assert_eq!(card.source, "src1");
        assert_eq!(card.source_name, "SourceOne");
        assert_eq!(card.year.as_deref(), Some("2020"));
        assert_eq!(card.class.as_deref(), Some("电影"));
        assert_eq!(card.episodes.len(), 2);
    }
}
```

Run: `cd "D:\Program Files (x86)\QuantumTV\src-tauri" && cargo test --lib home_catalog 2>&1 | tail -n 20`
Expected: `test result: ok. 2 passed`

- [ ] **Step 2: 提交**

```bash
git add src-tauri/src/commands/video.rs
git commit -m "test(home): cover home catalog helpers"
```

---

### Task 2: 注册命令 + Rust 编译门禁

**Files:**
- Modify: `src-tauri/src/lib.rs`（`invoke_handler` 列表）

**Interfaces:**
- Consumes: `commands::video::get_home_catalog`（Task 1）

- [ ] **Step 1: 在 lib.rs 注册命令**

在 `src-tauri/src/lib.rs` 的 `tauri::generate_handler![...]` 中，找到 `commands::video::get_source_videos_by_type,`（约 232 行），在其后追加一行：

```rust
            commands::video::get_source_videos_by_type,
            commands::video::get_home_catalog,
```

- [ ] **Step 2: cargo check 全量验证**

Run: `cd "D:\Program Files (x86)\QuantumTV\src-tauri" && cargo check 2>&1 | tail -n 20`
Expected: 编译通过，无 `error`，命令注册成功。

- [ ] **Step 3: 提交**

```bash
git add src-tauri/src/lib.rs
git commit -m "feat(home): register get_home_catalog command"
```

---

### Task 3: 前端类型

**Files:**
- Modify: `src/lib/types.ts`

**Interfaces:**
- Produces: `HomeCatalogResponse`、`HomeCategoryRow`、`HomeVideoCard`（供 Task 4 消费）
- Consumes: 无

- [ ] **Step 1: 在 types.ts 末尾追加类型**

在 `src/lib/types.ts` 文件末尾（`HomeBootstrapResponse` 之后）追加：

```typescript
/** 首页目录：一个订阅源的分类 + 每分类下的视频 */
export interface HomeCatalogResponse {
  source_key: string;
  source_name: string;
  site_type: number;
  categories: HomeCategoryRow[];
}

export interface HomeCategoryRow {
  type_id: string;
  type_name: string;
  list: HomeVideoCard[];
}

export interface HomeVideoCard {
  id: string;
  title: string;
  poster: string;
  year?: string | null;
  episodes: string[];
  class?: string | null;
  source: string;
  source_name: string;
}
```

- [ ] **Step 2: typecheck 验证**

Run: `npm run typecheck`
Expected: `npx tsc --noEmit --incremental false` 退出 0，无新增类型错误。

- [ ] **Step 3: 提交**

```bash
git add src/lib/types.ts
git commit -m "feat(home): add HomeCatalogResponse types"
```

---

### Task 4: VideoCard 点击规则统一 + 新增 `from='home'`

**Files:**
- Modify: `src/components/VideoCard.tsx`（`from` 类型、`handleClick`、`mobileActions`、`config`）

**Interfaces:**
- Consumes: `HomeVideoCard` 字段（Task 3）；`VideoCardProps.from` 新增 `'home'`
- Produces: `from='home'` 的卡片点击跳 `/search?q=`；非 home 卡片规则改为"有 source+id 直播，否则搜索"

- [ ] **Step 1: 扩展 `from` 联合类型**

在 `src/components/VideoCard.tsx` 第 36 行 `from: 'playrecord' | 'favorite' | 'search' | 'douban' | 'recommendation';` 改为：

```typescript
from:
  | 'playrecord'
  | 'favorite'
  | 'search'
  | 'douban'
  | 'recommendation'
  | 'home';
```

- [ ] **Step 2: 改写 `handleClick`（第 269-299 行）**

将现有 `handleClick` 的 `useCallback` 函数体替换为：

```typescript
    const handleClick = useCallback(() => {
      // 首页目录卡片：一律跳搜索页
      if (from === 'home') {
        const q = encodeURIComponent(actualTitle.trim());
        router.push(`/search${q ? `?q=${q}` : ''}`);
        return;
      }
      // 有具体源 + id：直接播放
      if (actualSource && actualId) {
        const url = `/play?source=${actualSource}&id=${actualId}&title=${encodeURIComponent(
          actualTitle,
        )}${actualYear ? `&year=${actualYear}` : ''}${
          actualQuery ? `&stitle=${encodeURIComponent(actualQuery.trim())}` : ''
        }${actualSearchType ? `&stype=${actualSearchType}` : ''}`;
        router.push(url);
        return;
      }
      // 其它（豆瓣 / 推荐 / 聚合无源）：跳搜索页
      const q = encodeURIComponent(actualTitle.trim());
      router.push(`/search${q ? `?q=${q}` : ''}`);
    }, [
      from,
      actualSource,
      actualId,
      router,
      actualTitle,
      actualYear,
      actualQuery,
      actualSearchType,
    ]);
```

- [ ] **Step 3: 新增 `home` 的样式配置**

在 `config` 的 `useMemo`（第 361-415 行）的 `configs` 对象中，`search` 之后追加：

```typescript
        home: {
          showSourceName: true,
          showProgress: false,
          showPlayButton: true,
          showHeart: true,
          showCheckCircle: false,
          showDoubanLink: false,
          showRating: false,
          showYear: true,
        },
```

- [ ] **Step 4: typecheck + lint 验证**

Run: `npm run typecheck` 然后 `npm run lint`
Expected: 两者均无新增 error（`from='home'` 已在类型中；`handleClick` 依赖项齐备）。

- [ ] **Step 5: 提交**

```bash
git add src/components/VideoCard.tsx
git commit -m "feat(home): unify VideoCard click rule and add from=home"
```

---

### Task 5: 重写首页（源切换器 + 目录行）

**Files:**
- Create: `src/components/HomeCatalogSection.tsx`（新组件，保持 page.tsx 清晰）
- Modify: `src/app/page.tsx`（删除豆瓣四栏，插入 `<HomeCatalogSection />`）

**Interfaces:**
- Consumes: `useSourceFilter()`（`sources`、`isLoadingSources`）；`invoke<HomeCatalogResponse>('get_home_catalog', ...)`；`VideoCard`；`ScrollableRow`；`appLayoutClasses`
- Produces: 首页目录区块；无目录时 `router.push('/search')`

- [ ] **Step 1: 创建 `HomeCatalogSection.tsx`**

写入 `src/components/HomeCatalogSection.tsx`：

```tsx
/* eslint-disable no-console */
'use client';

import { useRouter } from 'next/navigation';
import { useEffect, useRef, useState } from 'react';

import { HomeCatalogResponse } from '@/lib/types';
import { appLayoutClasses, getRailItemClass } from '@/lib/ui-layout';
import { useSourceFilter } from '@/hooks/useSourceFilter';
import ScrollableRow from './ScrollableRow';
import VideoCard from './VideoCard';

const STORAGE_KEY = 'home.selectedSourceKey';
const EMPTY_CATALOG: HomeCatalogResponse = {
  source_key: '',
  source_name: '',
  site_type: 1,
  categories: [],
};

function HomeSourceSwitcher({
  sources,
  selected,
  onSelect,
}: {
  sources: { key: string; name: string }[];
  selected: string;
  onSelect: (key: string) => void;
}) {
  return (
    <div className='mb-6 flex justify-center'>
      <div className='mx-auto flex max-w-full flex-wrap items-center justify-center gap-1.5 overflow-x-auto py-1 scrollbar-hide'>
        {sources.map((s) => {
          const active = s.key === selected;
          return (
            <button
              key={s.key}
              type='button'
              onClick={() => onSelect(s.key)}
              className={`tap-target inline-flex shrink-0 cursor-pointer items-center gap-1.5 rounded-full px-3 py-1.5 text-sm font-medium transition-colors duration-200
                ${
                  active
                    ? 'bg-purple-600 text-white shadow-md shadow-purple-500/30 dark:bg-purple-500'
                    : 'glass-chip chip-theme chip-glow chip-home hover:shadow-sm'
                }`}
            >
              {s.name}
            </button>
          );
        })}
      </div>
    </div>
  );
}

export default function HomeCatalogSection() {
  const router = useRouter();
  const { sources, isLoadingSources } = useSourceFilter();

  const [selectedSource, setSelectedSource] = useState<string>('');
  const [catalog, setCatalog] = useState<HomeCatalogResponse | null>(null);
  const [loading, setLoading] = useState(true);

  // 组件内缓存：Map<source_key, response>
  const cacheRef = useRef(new Map<string, HomeCatalogResponse>());
  const cancelledRef = useRef(false);

  // 初始化选中源（默认第一个）
  useEffect(() => {
    if (sources.length === 0) return;
    let initial = sources[0].key;
    try {
      const saved = localStorage.getItem(STORAGE_KEY);
      if (saved && sources.some((s) => s.key === saved)) {
        initial = saved;
      }
    } catch {
      /* ignore */
    }
    setSelectedSource(initial);
  }, [sources]);

  // 拉取 / 复用缓存
  useEffect(() => {
    if (!selectedSource) return;
    let cancelled = false;
    cancelledRef.current = false;

    const cached = cacheRef.current.get(selectedSource);
    if (cached) {
      setCatalog(catalog);
      setLoading(false);
      // 无目录 → 静默跳转搜索页
      if (cached.categories.length === 0) {
        router.push('/search');
      }
      return;
    }

    setLoading(true);
    invoke<HomeCatalogResponse>('get_home_catalog', {
      sourceKey: selectedSource,
    })
      .then((resp) => {
        if (cancelled) return;
        cacheRef.current.set(selectedSource, resp);
        setCatalog(resp);
        if (resp.categories.length === 0) {
          router.push('/search');
        }
      })
      .catch((err) => {
        console.error('获取首页目录失败:', err);
        if (cancelled) return;
        setCatalog(EMPTY_CATALOG);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });

    return () => {
      cancelled = true;
      cancelledRef.current = true;
    };
  }, [selectedSource, router]);

  const handleSelect = (key: string) => {
    setSelectedSource(key);
    try {
      localStorage.setItem(STORAGE_KEY, key);
    } catch {
      /* ignore */
    }
  };

  // 骨架屏
  const SkeletonRow = () => (
    <section className={appLayoutClasses.sectionGap}>
      <div className='mb-5 h-5 w-24 bg-gray-200 dark:bg-gray-700 rounded animate-pulse' />
      <div className='flex gap-3 overflow-x-auto pb-2 scrollbar-hide'>
        {Array.from({ length: 6 }).map((_, i) => (
          <div
            key={i}
            className={`${getRailItemClass('default')} aspect-2/3 w-full max-w-[10.75rem] rounded-xl bg-gradient-to-br from-gray-200 to-gray-300 dark:from-gray-800 dark:to-gray-700 animate-pulse`}
          />
        ))}
      </div>
    </section>
  );

  if (isLoadingSources || sources.length === 0) {
    return null;
  }

  if (loading || !catalog) {
    return (
      <>
        <HomeSourceSwitcher
          sources={sources}
          selected={selectedSource || sources[0]?.key || ''}
          onSelect={handleSelect}
        />
        <SkeletonRow />
      </>
    );
  }

  // 目录为空时正在跳转，仅渲染切换器
  if (catalog.categories.length === 0) {
    return (
      <HomeSourceSwitcher
        sources={sources}
        selected={catalog.source_key || selectedSource}
        onSelect={handleSelect}
      />
    );
  }

  return (
    <>
      <HomeSourceSwitcher
        sources={sources}
        selected={catalog.source_key}
        onSelect={handleSelect}
      />
      {catalog.categories.map((cat) => (
        <section key={cat.type_id} className={appLayoutClasses.sectionGap}>
          <div className='mb-5 flex items-center justify-between'>
            <h2 className='text-lg font-bold text-gray-800 max-[375px]:text-base min-[834px]:text-[1.35rem] min-[1440px]:text-[1.5rem] dark:text-gray-100'>
              {cat.type_name}
            </h2>
          </div>
          <ScrollableRow>
            {cat.list.map((card) => (
              <div key={card.id} className={getRailItemClass('default')}>
                <VideoCard
                  from='home'
                  id={card.id}
                  source={card.source}
                  title={card.title}
                  poster={card.poster}
                  episodes={card.episodes.length || undefined}
                  source_name={card.source_name}
                  year={card.year || undefined}
                  class={card.class || undefined}
                />
              </div>
            ))}
          </ScrollableRow>
        </section>
      ))}
    </>
  );
}
```

- [ ] **Step 2: 修改 page.tsx，删除豆瓣四栏并插入组件**

在 `src/app/page.tsx`：
1. 移除 `import { ChevronRight, Sparkles, X } from 'lucide-react';` 中的 `ChevronRight`（首页不再用"查看更多"链接）——若它在其它地方仍被使用请保留；安全做法：只在确认仅首页使用后再删。**保守做法：保留 import 不删**，仅移除使用处，避免误删。实际检查：`ChevronRight` 在 page.tsx 中仅用于豆瓣四栏的"查看更多"链接，删除后无其它引用，可一并移除 import。
2. 移除 `hotMovies/hotTvShows/hotVarietyShows/todayBangumi` 状态与 `loadHomeBootstrap` 中的对应赋值（保留 `recommendations`、`showAnnouncement`、`favoriteItems`、`activeTab`）。
3. 在首页视图的 `<ContinueWatching />` 之后、"为你推荐"之前插入 `<HomeCatalogSection />`。
4. 删除"热门电影 / 热门剧集 / 新番放送 / 热门综艺"四个 `<section>`（含对应的 `Link 查看更多` 与 `ScrollableRow`）。

具体替换：将第 272-443 行附近的四个豆瓣 section（从 `{/* 热门电影 */}` 到 `{/* 热门综艺 */}` 结束）整段替换为：

```tsx
              {/* 配置源目录 */}
              <HomeCatalogSection />
```

并在文件顶部 imports 处加入：

```tsx
import HomeCatalogSection from '@/components/HomeCatalogSection';
```

> 说明：`ContinueWatching`（第 244 行）与"为你推荐"（第 247-270 行）保留不动；`loading` 骨架屏原本用于豆瓣行，现改为目录组件内部自行 loading，page.tsx 侧的 `SkeletonCard` 仅在收藏夹空态等处如仍被引用则保留（检查：`SkeletonCard` 仅用于豆瓣行骨架，删除后若无其它引用可一并移除；**保守做法：保留 `SkeletonCard` 定义不删**，避免破坏其它引用）。

- [ ] **Step 3: typecheck + lint 验证**

Run: `npm run typecheck` 然后 `npm run lint`
Expected: 无新增 error。重点确认：`HomeCatalogSection` 未使用 `ChevronRight`/`X` 等已移除 import；`page.tsx` 中无悬空引用。

- [ ] **Step 4: 提交**

```bash
git add src/components/HomeCatalogSection.tsx src/app/page.tsx
git commit -m "feat(home): rewrite home page to source-driven catalog"
```

---

### Task 6: 全量门禁验证

**Files:**
- 全部变更

- [ ] **Step 1: Rust 编译**

Run: `cd "D:\Program Files (x86)\QuantumTV\src-tauri" && cargo check 2>&1 | tail -n 15`
Expected: `Finished`，无 error。

- [ ] **Step 2: 前端类型 + Lint**

Run: `npm run typecheck` 然后 `npm run lint`
Expected: 退出码 0。

- [ ] **Step 3:（可选）前端生产构建 smoke test**

Run: `npm run build`（如耗时可省略，但建议跑一次确认首页无 SSR/编译错误）
Expected: `Compiled successfully` / `Creating optimized build` 完成。

- [ ] **Step 4: 最终提交（如有未提交变更）**

```bash
git status --short
git add -A
git commit -m "chore(home): full gate verification"  # 仅当有实质变更时
```

---

## 自审（本计划 vs spec）

**1. Spec 覆盖：**
- 首页完全由订阅源目录生成、替换豆瓣四栏 → Task 5（`HomeCatalogSection` + page.tsx 删除豆瓣四栏）✓
- 点击首页卡片 → 搜索页 → Task 4（`from='home'` → `/search?q=`）+ Task 5（`from='home'`）✓
- 有 source+id 直播 / 豆瓣走搜索 → Task 4（handleClick 三分支）✓
- 源切换器默认第一个、可切换、缓存、localStorage 持久化 → Task 5 ✓
- Spider 源静默跳搜索 → Task 1（`site_type==3` 返回空）+ Task 5（空目录 `router.push('/search')`）✓
- 目录按源缓存（后端 TTL + 前端 Map）→ Task 1（`HOME_CATALOG_CACHE`）+ Task 5（`cacheRef`）✓
- CMS `?ac=class` + `?ac=videolist&t=&pg=1`，每分类前 8 条，失败降级 → Task 1 ✓
- 继续观看 / 为你推荐 / 收藏夹 Tab 保留 → Task 5（仅替换豆瓣四栏）✓

**2. Placeholder 扫描：** 无 TBD/Todo/相似任务简述；每步均含真实代码与运行命令。✓

**3. 类型一致性：**
- `HomeCatalogResponse` / `HomeCategoryRow` / `HomeVideoCard` 在 Task 3（TS）与 Task 1（RS）字段名一致（`source_key`、`source_name`、`site_type`、`categories`、`type_id`、`type_name`、`list`、`id`/`title`/`poster`/`year`/`episodes`/`class`/`source`/`source_name`），serde 默认 snake_case 与 TS camelCase 对齐（Rust 字段已是 snake，TS 用同名 snake 即可，Tauri invoke 按 key 匹配）✓
- `get_home_catalog` 命令名与 Task 5 调用 `invoke('get_home_catalog', { sourceKey })` 一致 ✓
- `VideoCardProps.from` 联合类型含 `'home'`（Task 4 Step 1）与 Task 5 使用的 `from='home'` 一致 ✓