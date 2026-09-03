# QuantumTV

本地优先的开源影视聚合播放器。桌面端(Tauri)为主,可选 api-server 供外部 TVBox 设备使用。播放源由用户自行配置,数据全部本地存储。

## Language

**站点 (Site)**:
用户在订阅或管理页面中配置的单个视频来源。每个站点有唯一 `key`。
_Avoid_: 源(source)、视频源

**CMS 站点 (CMS Site, type:1)**:
通过 MacCMS 标准接口(`ac=videolist`)HTTP 查询的站点。api 字段为 `https://.../api.php/provide/vod` 形式的 URL。
_Avoid_: 普通站点、采集站

**Spider 站点 (Spider Site, type:3)**:
通过 Java JAR 中反射执行的站点。api 字段为 `csp_ClassName` 形式的类名,搜索和详情均需调用 JVM 子进程。
_Avoid_: Java 站点、爬虫站点

**站点类型 (site_type)**:
决定站点搜索/详情走哪条执行路径的数字标识。1 = CMS, 3 = Spider。
_Avoid_: type、分类

**订阅 (Subscription)**:
TVBox 格式的 JSON 配置,包含 `spider`(全局 JAR URL)、`sites`、`parses`、`lives` 等顶层字段。
_Avoid_: 配置、config

**Spider 配置 (Spider Spec)**:
JAR 定位串,格式为 `URL;md5;<hash>`。URL 为 JAR 下载地址,md5 用于完整性校验。
_Avoid_: jar 链接、spider 字段

**Spider JAR**:
CatVodSpider 编译产出的 Java 归档文件。同一订阅下多个 Spider 站点共享同一个 JAR。
_Avoid_: jar 包、爬虫包

**类名 (class_name)**:
Spider 站点 api 字段去掉 `csp_` 前缀后的名称,即 JAR 中实际要反射实例化的类。
_Avoid_: spider 类、类

**搜索 (Search)**:
按关键词在所有已启用、可搜索站点中并行查询,结果聚合去重后返回。
_Avoid_: 检索、查询(query)

**详情 (Detail)**:
根据站点的视频 id 获取该视频的完整播放列表(选集)。
_Avoid_: 视频详情页

**解析器 (Parse)**:
把播放地址转换/解析为可直连 m3u8 的第三方服务。TVBox 标准 parses 数组中的一项。
_Avoid_: 解析、解析源

**可搜索 (Searchable)**:
站点是否参与搜索的标志。`searchable=0` 的站点被跳过,只用于浏览。

**Java 运行时 (JDK)**:
执行 Spider 站点搜索/详情所需的 Java 环境(JDK 17+)。桌面端要求用户自行安装;未安装时 Spider 站点静默跳过。
_Avoid_: JRE、java
