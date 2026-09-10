# TVBox 订阅搜索支持设计文档

**日期**: 2026-09-02
**状态**: 已批准
**范围**: 修复 TVBox 订阅导入后 type:3 spider 站点搜索无结果的问题

---

## 1. 问题描述

### 现状
- TVBox 订阅 URL（如 `https://9280.kstore.space/wex.json`）可以成功导入
- 订阅中的 `sites` 数组被解析并存入 `video_sources` 数据库表
- 但搜索时这些站点**不返回任何结果**

### 根因
1. **搜索格式不兼容**: 搜索代码对所有站点使用 `{api}?ac=videolist&wd={query}`（MacCMS 格式），这只适用于 `type:1` CMS 站点。`type:3` spider 站点（如 `csp_WexzhizhenGuard`）需要通过 spider JAR（Java 程序）执行搜索。
2. **缺少 site_type 字段**: `video_sources` 表没有 `site_type` 列，程序无法区分 CMS 站点和 spider 站点。
3. **spider JAR 未被使用**: API Server 下载并缓存了 spider JAR，但仅用于分发给外部 TVBox 客户端，服务端自身不执行搜索。

### TVBox 配置格式示例
```json
{
  "spider": "http://example.com/spider.jar;md5;abc123",
  "sites": [
    {
      "key": "Wexzhizhen",
      "name": "至臻 4K",
      "type": 3,
      "api": "csp_WexzhizhenGuard",
      "searchable": 1,
      "changeable": 1
    },
    {
      "key": "SomeCMS",
      "name": "CMS 站点",
      "type": 1,
      "api": "https://example.com/api.php/provide/vod",
      "searchable": 1
    }
  ],
  "parses": [...],
  "lives": [...]
}
```

---

## 2. 设计目标

1. 导入 TVBox 订阅后，`type:1` CMS 站点可直接搜索
2. `type:3` spider 站点通过 API Server 代理搜索（需要 Java 环境）
3. 支持多订阅管理（添加/删除/切换订阅）
4. 搜索结果统一聚合、去重、排序
5. Java 不可用时优雅降级，不阻塞其他站点搜索

---

## 3. 方案选择

### 方案 A：API Server 代理搜索（选定方案）
通过本地 Axum API Server 代理 `type:3` 站点的搜索请求，由 spider JAR 执行。

**优点**: 复用已有 API Server 基础设施，改动最小
**缺点**: 需要 Java 运行时

### 方案 B：内置 Spider 执行引擎
在 Rust 端内嵌 Java 执行能力。

**优点**: 完全自包含
**缺点**: 需要嵌入 JRE（~200-300MB），跨平台打包复杂

### 方案 C：仅支持 CMS 搜索
只搜索 `type:1` 站点，spider 站点标记为"仅浏览"。

**优点**: 最简单
**缺点**: 大部分 TVBox 站点是 type:3，搜索覆盖率极低

---

## 4. 详细设计

### 4.1 数据层：扩展站点存储

#### 数据库 Schema 变更

```sql
-- video_sources 表新增列
ALTER TABLE video_sources ADD COLUMN site_type INTEGER NOT NULL DEFAULT 1;
ALTER TABLE video_sources ADD COLUMN spider TEXT NOT NULL DEFAULT '';
ALTER TABLE video_sources ADD COLUMN searchable INTEGER NOT NULL DEFAULT 1;
```

#### Rust 数据结构

```rust
// crates/core/src/types.rs
pub struct SourceConfig {
    pub key: String,
    pub name: String,
    pub api: String,
    pub detail: String,
    pub from: From,
    pub disabled: bool,
    pub is_adult: bool,
    pub site_type: i32,      // 新增: 1=CMS, 3=Spider
    pub spider: String,      // 新增: spider JAR URL
    pub searchable: i32,     // 新增: 是否支持搜索
}
```

#### TypeScript 类型更新

```typescript
// src/lib/admin.types.ts
interface SourceConfig {
  key: string;
  name: string;
  api: string;
  detail?: string;
  from: 'config' | 'custom';
  disabled?: boolean;
  is_adult?: boolean;
  site_type?: number;    // 新增: 1=CMS, 3=Spider
  spider?: string;       // 新增: spider JAR URL
  searchable?: number;   // 新增: 是否支持搜索
}
```

#### 订阅解析变更

`crates/core/src/admin_config.rs` 中的 `normalize_source_config_item()` 需要：
1. 保留 TVBox 原始 `type` 字段，映射为 `site_type`
2. **spider JAR URL 来源**: TVBox 订阅 JSON 的顶层 `spider` 字段是所有站点共享的 JAR URL。解析时需将此 URL 传递给每个 `type:3` 站点，存入 `spider` 列。
3. 保留 `searchable` 字段

**注意**: 个别站点可能有自己的 `jar` 字段覆盖全局 spider，需优先使用站点级 `jar`，其次使用全局 `spider`。

#### 数据迁移

对于已有的 `video_sources` 表数据（无 `site_type` 列）：
- `ALTER TABLE` 新增列时使用 `DEFAULT 1`，已有数据自动归类为 CMS 站点
- 这是安全的，因为已有的站点都是通过 MacCMS 格式添加的

### 4.2 搜索路由：分流 CMS 和 Spider 站点

#### Tauri 搜索命令变更

`src-tauri/src/commands/video.rs` 中的 `search_with_cache_hit()`:

```rust
// 伪代码
for source in enabled_sources {
    match source.site_type {
        1 => {
            // CMS 站点: 直接 HTTP GET
            let url = format!("{}?ac=videolist&wd={}", source.api, query);
            let response = http_get(&url, 6).await;
            if let Ok(data) = response {
                if let Ok(parsed) = serde_json::from_str::<ApiSearchResponse>(&data) {
                    results.extend(parsed.list);
                }
            }
        }
        3 => {
            // Spider 站点: 转发到 API Server
            if source.searchable == 1 {
                let url = format!(
                    "http://127.0.0.1:{}/api/search?site_key={}&query={}",
                    API_PORT, source.key, urlencoding::encode(&query)
                );
                let response = http_get(&url, 10).await;
                if let Ok(data) = response {
                    if let Ok(parsed) = serde_json::from_str::<SearchResponse>(&data) {
                        results.extend(parsed.results);
                    }
                }
            }
        }
        _ => {}
    }
}
```

#### API Server 新增搜索端点

`crates/api-server/src/main.rs` 新增路由：

```rust
.route("/api/search", get(search_handler))
```

**API Server 端口**: 复用现有端口 3000（与当前 Axum 服务器一致）。搜索请求从 Tauri 搜索命令转发到 `http://127.0.0.1:3000/api/search`。

`crates/api-server/src/tvbox.rs` 新增 handler：

```rust
async fn search_handler(
    Query(params): Query<SearchParams>,
    State(state): State<ApiState>,
) -> Json<SearchResponse> {
    // 1. 根据 site_key 查找站点配置
    // 2. 获取对应的 spider JAR
    // 3. 检查 Java 运行时是否可用
    // 4. 如果可用: 通过子进程执行 Java 搜索
    // 5. 如果不可用: 返回空结果
}
```

### 4.3 Spider 执行：Java 子进程桥接

#### 执行流程

```
1. 接收搜索请求 (site_key, query)
2. 查找站点配置 (从 sites 列表)
3. 获取 spider JAR URL → 下载/缓存 JAR 文件
4. 检查 Java: 执行 `java -version`
5. 如果有 Java:
   a. 构建 Java 命令（详见下方"Java 搜索协议"）
   b. 执行子进程 (超时 10 秒)
   c. 解析 stdout JSON 输出
6. 如果没有 Java:
   a. 返回空结果 + 错误提示
7. 返回 SearchResponse
```

#### Java 搜索协议

CatVodSpider 的标准调用方式：

```bash
java -Dfile.encoding=UTF-8 -jar {spider.jar} {site_api} search {keyword}
```

- `{spider.jar}`: 下载缓存的 spider JAR 文件路径
- `{site_api}`: 站点的 api 字段（如 `csp_WexzhizhenGuard`）
- `{keyword}`: 搜索关键词

spider JAR 的搜索输出格式（标准 TVBox 格式）：

```json
{
  "list": [
    {
      "vod_id": 123,
      "vod_name": "流浪地球",
      "vod_pic": "https://...",
      "vod_year": "2019",
      "vod_remarks": "HD",
      "vod_play_url": "播放地址#Episode1$播放地址2#Episode2"
    }
  ]
}
```

#### 降级策略

- Java 不可用时，type:3 站点搜索静默返回空结果
- 搜索结果中不显示该站点（而非显示错误）
- 管理页面显示 Java 状态检测结果
- 前端提示："部分站点需要 Java 环境"

### 4.4 前端 UI 改进

#### 管理页面

- 站点列表新增 `site_type` 标签列（CMS / Spider）
- 显示 `searchable` 状态图标
- 站点详情面板显示 spider JAR 状态
- 订阅管理区域支持多订阅 URL

#### 搜索页面

- 搜索进度显示："正在搜索 X 个 CMS 站点 + Y 个 Spider 站点"
- Spider 站点结果带 `[Spider]` 标记
- Java 不可用时显示全局提示

#### 搜索结果类型

```typescript
// src/lib/types.ts
interface SearchResult {
  // 现有字段...
  source_site_type?: number;  // 新增: 来源站点类型
}
```

---

## 5. 改动文件清单

| 文件 | 改动类型 | 说明 |
|------|----------|------|
| `src-tauri/src/db/db_init.rs` | 修改 | video_sources 表新增列 |
| `crates/core/src/admin_config.rs` | 修改 | 解析保留 site_type, spider, searchable |
| `crates/core/src/search_aggregation.rs` | 修改 | 聚合结果包含 site_type 信息 |
| `src-tauri/src/commands/config.rs` | 修改 | SourceConfig 结构体扩展字段 |
| `src-tauri/src/commands/video.rs` | 修改 | 搜索分流逻辑 |
| `src-tauri/src/commands/search.rs` | 修改 | 搜索状态包含 site_type |
| `crates/api-server/src/main.rs` | 修改 | 新增 /api/search 路由 |
| `crates/api-server/src/tvbox.rs` | 修改 | 新增 search_handler |
| `src/lib/admin.types.ts` | 修改 | TypeScript 类型更新 |
| `src/lib/types.ts` | 修改 | SearchResult 类型更新 |
| `src/app/admin/page.tsx` | 修改 | 管理页面 UI 改进 |
| `src/app/search/page.tsx` | 修改 | 搜索页面 UI 改进 |

---

## 6. 测试策略

1. **单元测试**: admin_config 解析、site_type 识别
2. **集成测试**: 订阅导入 → 站点存储 → 搜索路由
3. **端到端测试**: 导入 wex.json 订阅 → 搜索 "流浪地球" → 验证 CMS 和 Spider 站点都返回结果
4. **降级测试**: 无 Java 环境下搜索不崩溃

---

## 7. 风险与注意事项

1. **Java 依赖**: type:3 站点搜索需要 Java 运行时，需在文档和 UI 中明确说明
2. **Spider JAR 兼容性**: 不同订阅的 spider JAR 可能有不同版本，需要版本兼容性处理
3. **性能**: Java 子进程启动有冷启动开销（~1-2 秒），需考虑缓存或复用
4. **安全性**: spider JAR 是外部下载的可执行文件，需要 MD5 校验和沙箱隔离
5. **跨平台**: Android 平台由系统提供 Java，桌面端需要用户安装 JRE
