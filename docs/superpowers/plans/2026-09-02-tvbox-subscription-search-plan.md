# TVBox 订阅搜索支持实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复 TVBox 订阅导入后 type:3 spider 站点搜索无结果的问题，通过 API Server 代理搜索。

**Architecture:** 扩展 `video_sources` 数据库存储 TVBox 元数据（site_type, spider, searchable），在搜索时根据 site_type 分流：type:1 走现有 CMS 逻辑，type:3 转发到 API Server 的 /api/search 端点由 spider JAR 执行。

**Tech Stack:** Rust (Tauri 2, Axum, SQLite), TypeScript (React 19, Next.js 16), Java subprocess bridge

## Global Constraints

- Rust edition: 2021
- Node.js: >= 18 (from .nvmrc)
- SQLite via rusqlite (bundled)
- Tauri 2 with IPC invoke
- No new npm dependencies unless absolutely necessary
- All Rust code must pass `cargo check` and `cargo test`
- All TypeScript must pass ESLint

---

### Task 1: 扩展 video_sources 数据库 Schema

**Files:**
- Modify: `src-tauri/src/db/db_init.rs:183-203` (CREATE TABLE)
- Modify: `src-tauri/src/db/db_init.rs:269+` (migration block)

**Interfaces:**
- Consumes: None (first task)
- Produces: `video_sources` 表新增 `site_type`, `spider`, `searchable` 三列

- [ ] **Step 1: 修改 CREATE TABLE 语句**

在 `src-tauri/src/db/db_init.rs` 第 193 行（`updated_at` 列之后）新增三列：

```rust
CREATE TABLE IF NOT EXISTS video_sources (
    source_key TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    api TEXT NOT NULL,
    detail TEXT NOT NULL DEFAULT '',
    from_type TEXT NOT NULL DEFAULT 'custom',
    disabled INTEGER NOT NULL DEFAULT 0,
    is_adult INTEGER NOT NULL DEFAULT 0,
    sort_order INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    site_type INTEGER NOT NULL DEFAULT 1,
    spider TEXT NOT NULL DEFAULT '',
    searchable INTEGER NOT NULL DEFAULT 1
);
```

- [ ] **Step 2: 添加数据库迁移**

在 `db_init.rs` 的迁移块中（`user_version` 检查处），添加从 version 2 到 3 的迁移：

```rust
if user_version < 3 {
    conn.execute_batch(
        "ALTER TABLE video_sources ADD COLUMN site_type INTEGER NOT NULL DEFAULT 1;
         ALTER TABLE video_sources ADD COLUMN spider TEXT NOT NULL DEFAULT '';
         ALTER TABLE video_sources ADD COLUMN searchable INTEGER NOT NULL DEFAULT 1;
         PRAGMA user_version = 3;"
    )?;
}
```

- [ ] **Step 3: 验证编译通过**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: no errors

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/db/db_init.rs
git commit -m "feat(db): add site_type, spider, searchable columns to video_sources"
```

---

### Task 2: 扩展 Rust SourceConfig 结构体

**Files:**
- Modify: `src-tauri/src/commands/config.rs:92-101` (SourceConfig struct)
- Modify: `src-tauri/src/commands/video.rs:659-666` (ApiSite struct)

**Interfaces:**
- Consumes: Task 1 的数据库 schema
- Produces: `SourceConfig` 和 `ApiSite` 结构体包含 `site_type`, `spider`, `searchable` 字段

- [ ] **Step 1: 修改 SourceConfig 结构体**

在 `src-tauri/src/commands/config.rs` 第 92-101 行，扩展 SourceConfig：

```rust
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SourceConfig {
    pub key: String,
    pub name: String,
    pub api: String,
    pub detail: String,
    pub from: From,
    pub disabled: bool,
    pub is_adult: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site_type: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub searchable: Option<i32>,
}
```

注意：原结构体没有 `Clone`，需要加上。

- [ ] **Step 2: 修改 ApiSite 结构体**

在 `src-tauri/src/commands/video.rs` 第 659-666 行，扩展 ApiSite：

```rust
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ApiSite {
    pub key: String,
    pub api: String,
    pub name: String,
    pub detail: Option<String>,
    pub is_adult: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site_type: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub searchable: Option<i32>,
}
```

- [ ] **Step 3: 验证编译通过**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 编译可能有其他地方因新增字段报错，逐一修复

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/commands/config.rs src-tauri/src/commands/video.rs
git commit -m "feat(config): extend SourceConfig and ApiSite with TVBox fields"
```

---

### Task 3: 扩展数据库读写函数

**Files:**
- Modify: `src-tauri/src/commands/config.rs:183-207` (load_source_config_values)
- Modify: `src-tauri/src/commands/config.rs:209-339` (persist_source_config_values)

**Interfaces:**
- Consumes: Task 2 的 SourceConfig 结构体
- Produces: 数据库读写支持新字段

- [ ] **Step 1: 修改 load_source_config_values SELECT 语句**

在 `config.rs` 第 185-189 行，修改 SELECT：

```rust
let mut stmt = conn.prepare(
    "SELECT source_key, name, api, detail, from_type, disabled, is_adult,
            site_type, spider, searchable
     FROM video_sources
     ORDER BY sort_order ASC, updated_at DESC, source_key ASC",
)?;
```

- [ ] **Step 2: 修改 load_source_config_values 结果映射**

在 `config.rs` 第 193-201 行的 JSON 构建中，添加新字段：

```rust
let source = serde_json::json!({
    "key": row.get::<_, String>(0)?,
    "name": row.get::<_, String>(1)?,
    "api": row.get::<_, String>(2)?,
    "detail": row.get::<_, String>(3)?,
    "from": row.get::<_, String>(4)?,
    "disabled": row.get::<_, i32>(5)? != 0,
    "is_adult": row.get::<_, i32>(6)? != 0,
    "site_type": row.get::<_, i32>(7).unwrap_or(1),
    "spider": row.get::<_, String>(8).unwrap_or_default(),
    "searchable": row.get::<_, i32>(9).unwrap_or(1),
});
```

- [ ] **Step 3: 修改 persist_source_config_values INSERT 语句**

在 `config.rs` 第 280-301 行，修改 INSERT 语句，添加新列：

```rust
conn.execute(
    "INSERT INTO video_sources (source_key, name, api, detail, from_type, disabled, is_adult, sort_order, created_at, updated_at, site_type, spider, searchable)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
     ON CONFLICT(source_key) DO UPDATE SET
       name = ?2, api = ?3, detail = ?4, from_type = ?5, disabled = ?6, is_adult = ?7,
       sort_order = ?8, updated_at = ?10, site_type = ?11, spider = ?12, searchable = ?13",
    // ... parameters including site_type, spider, searchable
)?;
```

- [ ] **Step 4: 修改 SourceRow 结构体**

在 `config.rs` 第 213-223 行的 SourceRow 中添加新字段：

```rust
struct SourceRow {
    key: String,
    name: String,
    api: String,
    detail: String,
    from_type: String,
    disabled: bool,
    is_adult: bool,
    site_type: i32,
    spider: String,
    searchable: i32,
}
```

- [ ] **Step 5: 验证编译通过**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: no errors

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/commands/config.rs
git commit -m "feat(config): support TVBox fields in source config read/write"
```

---

### Task 4: 扩展 Admin Config 解析器

**Files:**
- Modify: `crates/core/src/admin_config.rs:149-222` (normalize_source_config_item)
- Modify: `crates/core/src/admin_config.rs:224-270` (normalize_api_site_object)

**Interfaces:**
- Consumes: Task 2 的 SourceConfig 结构体
- Produces: 解析器保留 TVBox 原始字段

- [ ] **Step 1: 在 normalize_source_config_item 中提取 TVBox 字段**

在 `admin_config.rs` 第 211 行（`detail` 提取之后），添加：

```rust
// TVBox-specific fields
let searchable = item.get("searchable").and_then(|v| v.as_i64()).unwrap_or(1) as i32;
let quick_search = item.get("quick_search").and_then(|v| v.as_i64());
let filterable = item.get("filterable").and_then(|v| v.as_i64());
let changeable = item.get("changeable").and_then(|v| v.as_str());
let jar = item.get("jar").and_then(|v| v.as_str());
let site_type = item.get("type").and_then(|v| v.as_i64()).unwrap_or(1) as i32;
```

- [ ] **Step 2: 将新字段插入标准化对象**

在 `admin_config.rs` 第 213-221 行的 `normalized` 对象构建中，添加：

```rust
normalized.insert("site_type".to_string(), serde_json::json!(site_type));
normalized.insert("searchable".to_string(), serde_json::json!(searchable));
if let Some(qs) = quick_search {
    normalized.insert("quick_search".to_string(), serde_json::json!(qs));
}
if let Some(f) = filterable {
    normalized.insert("filterable".to_string(), serde_json::json!(f));
}
if let Some(c) = changeable {
    normalized.insert("changeable".to_string(), serde_json::json!(c));
}
if let Some(j) = jar {
    normalized.insert("jar".to_string(), serde_json::json!(j));
}
```

- [ ] **Step 3: 在 normalize_api_site_object 中添加相同逻辑**

对 `admin_config.rs` 第 224-270 行的 `normalize_api_site_object()` 做相同修改。

- [ ] **Step 4: 处理订阅级别的 spider URL**

在 `normalize_admin_config_value()` 的 TVBox `sites` 格式处理分支（第 39-41 行），提取顶层 `spider` 字段并传递给每个站点：

```rust
} else if let Some(sites) = map.get("sites").and_then(|v| v.as_array()) {
    let global_spider = map.get("spider").and_then(|v| v.as_str()).unwrap_or("");
    let sources = normalize_source_config_array_with_spider(sites, "config", global_spider);
    Ok(build_config_with_sources(sources))
```

新增辅助函数 `normalize_source_config_array_with_spider`，在每个站点的 `jar` 字段为空时使用全局 `spider`。

- [ ] **Step 5: 验证编译通过**

Run: `cargo check --manifest-path crates/core/Cargo.toml`
Expected: no errors

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/admin_config.rs
git commit -m "feat(config): parse TVBox site_type, spider, searchable in admin config"
```

---

### Task 5: 扩展前端 TypeScript 类型

**Files:**
- Modify: `src/lib/admin.types.ts:39-47` (SourceConfig interface)
- Modify: `src/lib/types.ts:3-9` (ApiSite interface)

**Interfaces:**
- Consumes: Task 2 的 Rust 结构体定义
- Produces: TypeScript 类型与 Rust 端一致

- [ ] **Step 1: 修改 admin.types.ts SourceConfig**

在 `src/lib/admin.types.ts` 第 39-47 行，扩展 SourceConfig：

```typescript
SourceConfig: {
    key: string;
    name: string;
    api: string;
    detail?: string;
    from: 'config' | 'custom';
    disabled?: boolean;
    is_adult?: boolean;
    site_type?: number;
    spider?: string;
    searchable?: number;
    quick_search?: number;
    filterable?: number;
    changeable?: string;
    jar?: string;
}[];
```

- [ ] **Step 2: 修改 types.ts ApiSite**

在 `src/lib/types.ts` 第 3-9 行，扩展 ApiSite：

```typescript
export interface ApiSite {
    key: string;
    api: string;
    name: string;
    detail?: string;
    is_adult?: boolean;
    site_type?: number;
    spider?: string;
    searchable?: number;
}
```

- [ ] **Step 3: 修改 SearchResult 类型（可选）**

在 `src/lib/types.ts` 的 SearchResult 接口中，添加来源站点类型信息：

```typescript
export interface SearchResult {
    // ... existing fields
    source_site_type?: number;
}
```

- [ ] **Step 4: 验证 TypeScript 编译**

Run: `npx tsc --noEmit`
Expected: no errors

- [ ] **Step 5: Commit**

```bash
git add src/lib/admin.types.ts src/lib/types.ts
git commit -m "feat(types): add TVBox fields to TypeScript interfaces"
```

---

### Task 6: 搜索路由分流 — CMS vs Spider

**Files:**
- Modify: `src-tauri/src/commands/video.rs:1092-1116` (source list building)
- Modify: `src-tauri/src/commands/video.rs:1162-1296` (search loop)

**Interfaces:**
- Consumes: Task 2 的 ApiSite 结构体
- Produces: 搜索时根据 site_type 分流请求

- [ ] **Step 1: 在 source 列表构建中传递 site_type**

在 `video.rs` 第 1092-1116 行的 ApiSite 构建中，添加新字段：

```rust
let site = ApiSite {
    key: source.key.clone(),
    api: source.api.clone(),
    name: source.name.clone(),
    detail: if source.detail.is_empty() { None } else { Some(source.detail.clone()) },
    is_adult: Some(source.is_adult),
    site_type: source.site_type,
    spider: source.spider.clone(),
    searchable: source.searchable,
};
```

- [ ] **Step 2: 在搜索循环中添加 site_type 分流**

在 `video.rs` 第 1141-1296 行的搜索任务生成中，修改每个任务的逻辑：

```rust
// 在 spawn 的 async block 内部
let search_url = if site_clone.site_type.unwrap_or(1) == 3 {
    // Spider 站点: 转发到 API Server
    if site_clone.searchable.unwrap_or(1) != 1 {
        return vec![]; // 不支持搜索的站点跳过
    }
    format!(
        "http://127.0.0.1:3000/api/search?site_key={}&query={}",
        urlencoding::encode(&site_clone.key),
        urlencoding::encode(&query)
    )
} else {
    // CMS 站点: 直接 API 调用
    format!(
        "{}?ac=videolist&wd={}",
        site_clone.api,
        urlencoding::encode(&query)
    )
};
```

- [ ] **Step 3: 验证编译通过**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: no errors

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/commands/video.rs
git commit -m "feat(search): route spider site searches through API Server"
```

---

### Task 7: API Server 新增搜索端点

**Files:**
- Modify: `crates/api-server/src/main.rs:75-87` (routes)
- Modify: `crates/api-server/src/tvbox.rs` (add search handler)

**Interfaces:**
- Consumes: Task 6 的搜索转发逻辑
- Produces: `/api/search` 端点接收请求并执行 spider 搜索

- [ ] **Step 1: 添加 SearchParams 结构体**

在 `crates/api-server/src/tvbox.rs` 文件末尾添加：

```rust
#[derive(Deserialize)]
pub struct SearchParams {
    site_key: String,
    query: String,
}

#[derive(Serialize)]
pub struct SearchResponse {
    results: Vec<SearchResultItem>,
    java_available: bool,
}

#[derive(Serialize)]
pub struct SearchResultItem {
    vod_id: i64,
    vod_name: String,
    vod_pic: String,
    vod_year: String,
    vod_remarks: String,
    vod_play_url: String,
}
```

- [ ] **Step 2: 实现 search_handler**

在 `tvbox.rs` 文件末尾添加搜索 handler：

```rust
pub async fn search_handler(
    Query(params): Query<SearchParams>,
    State(state): State<AppState>,
) -> Json<SearchResponse> {
    // 1. 从配置中查找 site_key 对应的站点
    // 2. 获取 spider JAR URL
    // 3. 下载/缓存 JAR 文件
    // 4. 检查 Java 是否可用
    // 5. 如果可用: java -Dfile.encoding=UTF-8 -jar {jar} {api} search {query}
    // 6. 解析 stdout JSON
    // 7. 返回结果
}
```

- [ ] **Step 3: 实现 Java 检查和执行逻辑**

```rust
fn check_java_available() -> bool {
    std::process::Command::new("java")
        .arg("-version")
        .output()
        .is_ok()
}

fn execute_spider_search(jar_path: &str, site_api: &str, query: &str) -> Result<Vec<SearchResultItem>, String> {
    let output = std::process::Command::new("java")
        .args(["-Dfile.encoding=UTF-8", "-jar", jar_path, site_api, "search", query])
        .timeout(std::time::Duration::from_secs(10))
        .output()
        .map_err(|e| format!("Java execution failed: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    // 解析 TVBox 搜索结果 JSON 格式
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|e| format!("JSON parse failed: {}", e))?;

    // 提取 list 数组并转换为 SearchResultItem
    // ...
}
```

- [ ] **Step 4: 在 main.rs 添加路由**

在 `crates/api-server/src/main.rs` 第 75-87 行的路由定义中添加：

```rust
.route("/api/search", get(search_handler))
```

- [ ] **Step 5: 验证编译通过**

Run: `cargo check --manifest-path crates/api-server/Cargo.toml`
Expected: no errors

- [ ] **Step 6: Commit**

```bash
git add crates/api-server/src/main.rs crates/api-server/src/tvbox.rs
git commit -m "feat(api): add /api/search endpoint for spider site search"
```

---

### Task 8: 管理页面 UI 更新

**Files:**
- Modify: `src/app/admin/page.tsx:525-531` (newSource state)
- Modify: `src/app/admin/page.tsx:750-917` (Add/Edit modal)

**Interfaces:**
- Consumes: Task 5 的 TypeScript 类型
- Produces: 管理页面显示和编辑 TVBox 字段

- [ ] **Step 1: 扩展 newSource 状态**

在 `admin/page.tsx` 第 525-531 行，添加新字段：

```typescript
const [newSource, setNewSource] = useState({
    key: '',
    name: '',
    api: '',
    detail: '',
    is_adult: false,
    site_type: 1,
    spider: '',
    searchable: 1,
});
```

- [ ] **Step 2: 在站点列表中显示 site_type 标签**

在 `SortableSourceItem` 组件中，为站点名称旁添加类型标签：

```tsx
{source.site_type === 3 && (
    <span className="px-1.5 py-0.5 text-[10px] rounded-full bg-purple-500/20 text-purple-300">
        Spider
    </span>
)}
{source.site_type === 1 && (
    <span className="px-1.5 py-0.5 text-[10px] rounded-full bg-blue-500/20 text-blue-300">
        CMS
    </span>
)}
```

- [ ] **Step 3: 在添加站点模态框中添加 site_type 选择**

在 Add 模态框中，添加站点类型下拉选择：

```tsx
<div>
    <label>站点类型</label>
    <select
        value={newSource.site_type}
        onChange={(e) => setNewSource({...newSource, site_type: Number(e.target.value)})}
    >
        <option value={1}>CMS (采集站)</option>
        <option value={3}>Spider (Java)</option>
    </select>
</div>
```

- [ ] **Step 4: 验证前端编译**

Run: `npx next build` 或 `npx tsc --noEmit`
Expected: no errors

- [ ] **Step 5: Commit**

```bash
git add src/app/admin/page.tsx
git commit -m "feat(admin): add site_type display and selection in admin panel"
```

---

### Task 9: 搜索页面 UI 更新

**Files:**
- Modify: `src/app/search/page.tsx:604-683` (search results rendering)

**Interfaces:**
- Consumes: Task 5 的 SearchResult 类型
- Produces: 搜索结果显示 Spider 标记

- [ ] **Step 1: 在搜索结果卡片中添加来源类型标记**

在搜索结果渲染中，为每个结果添加站点类型标记：

```tsx
{result.source_site_type === 3 && (
    <span className="absolute top-1 right-1 px-1 py-0.5 text-[9px] rounded bg-purple-500/30 text-purple-200">
        Spider
    </span>
)}
```

- [ ] **Step 2: 添加搜索来源统计**

在搜索结果区域顶部，显示搜索统计：

```tsx
<div className="text-xs text-gray-400 mb-2">
    搜索了 {cmsCount} 个 CMS 站点 + {spiderCount} 个 Spider 站点
    {!javaAvailable && spiderCount > 0 && (
        <span className="text-yellow-400 ml-2">(Spider 站点需要 Java 环境)</span>
    )}
</div>
```

- [ ] **Step 3: 验证前端编译**

Run: `npx tsc --noEmit`
Expected: no errors

- [ ] **Step 4: Commit**

```bash
git add src/app/search/page.tsx
git commit -m "feat(search): display site type badges and search statistics"
```

---

### Task 10: 端到端验证

**Files:**
- 测试文件: 无（手动验证）

**Interfaces:**
- Consumes: 所有前序 Task
- Produces: 功能正常工作

- [ ] **Step 1: 构建并运行应用**

```bash
cd src-tauri && cargo build
cd .. && npm run build
npm run tauri dev
```

- [ ] **Step 2: 测试订阅导入**

1. 打开管理页面
2. 输入订阅 URL: `https://9280.kstore.space/wex.json`
3. 点击"拉取"按钮
4. 确认站点列表显示，每个站点有正确的 site_type 标签

- [ ] **Step 3: 测试搜索**

1. 在搜索页面输入关键词（如"流浪地球"）
2. 确认 CMS 站点返回结果
3. 确认 Spider 站点有响应（如果 Java 可用）
4. 确认搜索结果聚合正确

- [ ] **Step 4: 测试降级**

1. 如果没有 Java 环境，确认搜索不崩溃
2. 确认 CMS 站点结果正常显示
3. 确认提示信息正确

- [ ] **Step 5: Commit 最终版本**

```bash
git add -A
git commit -m "feat: TVBox subscription search support complete"
```
