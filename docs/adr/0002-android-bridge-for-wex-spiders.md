# ADR 0002: wex 类 Spider 通过 Android 桥接 APK 执行

状态: 已接受
日期: 2026-09-03

## 背景

用户订阅的 wex 类站点（api=csp_Wex*Guard，site_type=3）使用深度保护的 spider JAR：
- classes.dex 仅为薄壳（150+ 个 Guard 转发类，61KB）
- 真实逻辑在 ARM64/ARM32 native 库（wexguard_v8.so/v7.so，OLLVM 混淆）
- 加密资源 .wexfnw/.guard 由 native 解密
- DexNative 框架需要宿主 Application context 注入

三层硬墙使桌面端 JVM/Rust 原生执行不可行：
1. DEX 字节码：JVM 无法直接加载 Android DEX
2. ARM native：x86 JVM 无法加载/执行 ARM .so
3. DexNative 故意锁定 native（宿主校验）

## 决策

在 Android 模拟器（AVD wexbridge，API 30 x86_64 + WHPX 加速）内运行桥接 APK，
通过 HTTP 协议向桌面端暴露 spider 执行能力。

### 验证结论（Phase 1，2026-09-03）

关键机制链已完整打通并在真机（模拟器）验证：

1. **ARM64 转译真实可用**：x86_64 镜像自带 ndk_translation，
   APK 仅含 lib/arm64-v8a 时 Zygote 以 ARM64 ABI 启动进程，native bridge 激活。
2. **wexguard_v8.so 执行**：JNI_OnLoad（ARM64 代码）真实执行并注册 DexNative 的 native 方法。
   前提是 com.github.catvod.spider.DexNative 类必须存在于调用方 classloader。
3. **DexNative.<clinit> 流程**（经 dexdump 逆向确认）：
   - 调 Init.context() 取 Application
   - 从 classloader 读 assets/wexguard_v8.so（按 CPU_ABI 选 v7/v8）复制到 cache
   - System.load(绝对路径) 加载
   - 失败场景：context 为 null → NPE；进程为 x86_64 ABI → EM_AARCH64 vs EM_X86_64 错误
4. **Init.init(Context)**：注入 context 后调用 native getLoader()，native 解密
   assets/wexshinidie.guard 生成 code_cache/sharedb/config.db，创建 DexClassLoader。
5. **Init.getSpider(fqn)**：完整类名（含包名）调用，native 从 config.db 中查真实类
   （如 WexconfigGuard → 真实类 Wexconfig，去掉 Guard 后缀），实例化返回。
   短名会返回 null。
6. **真实数据返回**：配置中心 homeContent 返回 13 个分类 JSON，
   categoryContent 返回真实视频列表。
7. **依赖库需求**：真实 spider 需 Gson、okhttp3、okio、kotlin-stdlib、zxing 在 classpath。

### 桥接协议

桌面端（crates/core/src/spider/mod.rs）→ HTTP POST → adb forward tcp:8080 → 桥接 APK：

| 端点 | 参数 | 返回 |
|------|------|------|
| POST /init | {} | 初始化 Init.init（幂等） |
| POST /search | {class, keyword} | CatVod 标准搜索 JSON |
| POST /detail | {class, ids} | CatVod 标准详情 JSON |
| POST /home | {class} | homeContent JSON |
| POST /category | {class, tid, pg} | categoryContent JSON |
| GET /health | - | 状态 |

响应封装：{"code":200,"data":"<spider json>"} 或 {"code":4xx/5xx,"err":"..."}。

### core 集成

- `is_bridge_class(class_name)`: 含 "Wex" 或 "Guard" → 桥接；否则 JVM 路径
- `spider_bridge_search/detail/home/category(bridge_url)`
- bridge_url 由环境变量 QUANTUMTV_BRIDGE_URL 配置，默认 http://127.0.0.1:8080
- video.rs 搜索/详情分流已接入

## 桥接 APK 构建要素

无 Gradle 手工构建（build-tools 30.0.3 直连）：
1. javac --release 8 -encoding UTF-8 -classpath android.jar
2. d8 合并：BridgeService.class + Spider.java 产物 + spider_classes.dex（jar 内提取）
   + gson/okhttp/okio/kotlin-stdlib/zxing jar → 单 classes.dex
3. aapt2 link 生成未签名 APK
4. zip 打包：classes.dex + lib/arm64-v8a/libwexguard_v8.so（触发 ARM64 ABI）
   + assets/wexguard_v8.so + assets/wexshinidie.guard（DexNative.clinit 读取）
5. zipalign + apksigner（debug.keystore）

关键点：lib/arm64-v8a 必须非空（否则 Zygote 以 x86_64 启动进程，native bridge 不激活）。

## 备选方案（已否决）

- **dex2jar + x86 JVM**：ARM .so 无法在 x86 加载，死路
- **直接逆向 .wexfnw 解密协议**：OLLVM + 字符串加密 + 控制流平坦化，成本不可接受
- **Docker Android 容器**：需 KVM 嵌套，Win11 WHPX 不支持
- **Waydroid/Anbox**：需 Linux 内核模块

## 影响

- wex 类站点首次调用需模拟器运行（启动约 55s）
- 桌面端需 adb（platform-tools）可用
- 桥接 APK 需随订阅 spider JAR 更新重新打包（dex 合并）
- 模拟器常驻约 2GB RAM

## 后续工作

- Phase 4: 桌面端自动拉起 AVD + adb forward + 桥接健康检查
- 内容 spider（Wexzhizhen 等）返回 null 的业务配置初始化（配置中心 ext 链路）
- bridge_url 配置进管理界面而非环境变量
