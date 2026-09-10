# ADR-0001: Spider 搜索合并到桌面端(crates/core),api-server 降级为可选层

桌面端(Tauri)原本通过 `http://127.0.0.1:3000/api/search` 依赖独立运行的 api-server 进程执行 Spider 站点搜索,且 Spider 详情(detailContent)从未实现。用户必须手动启动 api-server 才能使用 Spider 站点,而 TVBox 订阅中绝大多数站点都是 type:3,导致核心功能不可用。我们决定把 Spider 搜索 + 详情执行能力搬进 `crates/core`,桌面端直接函数调用,api-server/Docker 保留为可选的外部 TVBox 后端层。

**理由**: 用户实际使用场景是 PC 桌面 App 自包含使用。HTTP 进程间依赖最脆弱(端口、生命周期、订阅同步),而 Java 子进程执行逻辑无论如何都要写,放 core 可同时被桌面端和 api-server 复用。Android 构建环境已暂停,不再作为交付目标。

**考虑过的替代方案**:
- 方案 B: 桌面端自动 spawn api-server 子进程(打包时捆绑两个二进制,复杂度高)
- 方案 C: api-server 在 Docker 中加 JDK,桌面端继续走 HTTP 代理(仍依赖常驻进程,detail 与播放链路照样要写)
- 方案 D: 完全删除 api-server 与 Docker(放弃外部 TVBox 设备接入能力)

**后果**:
- `crates/core` 新增 Spider 执行相关依赖(md5、base64、Java 子进程),保持 reqwest/tokio 已有依赖
- api-server 仍可作为可选 bin 存在(从 core 复用执行逻辑),Docker 部署继续有效
- 桌面端不再需要 127.0.0.1:3000,但需检测用户是否安装 JDK,未安装时 Spider 站点静默跳过
