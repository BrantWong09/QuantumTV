# SpiderBridge APK PoC Plan

## 目标

在 x86_64 Android 模拟器（AVD `wexbridge`）中运行一个轻量桥接 APK，加载 wex 订阅的 spider JAR（内含 classes.dex + ARM .so），暴露 HTTP API 供桌面端 QuantumTV 调用搜索/详情。

## 架构

```
QuantumTV (desktop)
  ├─── core::spider::spider_search/detail
  │         └── 检测 site_type=3 且 class_name 含 Guard
  │               └── HTTP POST http://127.0.0.1:8080/search|detail
  │                     └── 请求体: {"jar": <jar_path>, "class": "WexXXXGuard", "arg": "keyword|id", "action": "search|detail"}
  │
  └─── Android Emulator (headless)
        └─── SpiderBridge APK (com.quantumtv.bridge)
              ├─── HTTP server (lightweight, Kotlin socket/NanoHTTPd)
              ├─── DexClassLoader (从 /data/local/tmp/spider.jar 加载类)
              ├─── System.loadLibrary("wexguard_v8") (ARM64 .so via ndk_translation)
              └─── 反射调用 init/searchContent/detailContent
```

## 理由

wex 类 spider 的 ARM .so 保护（DexNative + wexguard .so + .wexfnw 加密资源）无法在 x86 JVM 上运行。唯一可行路径是 Android 模拟器 + ndk_translation + 桥接 APK。

## 关键风险

1. **ARM 转译是否真的能加载 wexguard .so？** 环境检查显示 `libndk_translation.so` 就位 + `arm64/ld-android.so` 存在 + `ro.enable.native.bridge.exec=1`。但需实际验证：`System.load("/data/local/tmp/libwexguard_v8.so")` 是否成功（JNI_OnLoad 可能做完整性校验/解密，但加载失败 ≠ 转译坏）。
2. **DexClassLoader 加载 JAR 内 classes.dex**：API 30 上 `DexClassLoader` 支持从 JAR 内加载 DEX（不压缩 DEX 即可）。wex JAR 内 classes.dex 是标准存储的（非压缩），应可直接加载。
3. **.wexfnw 加密资源**：`assets/wexshinidie.guard` 是加密配置，spider 在 init() 时通过 native 解密。桥接 APK 需把 JAR assets 目录解压到 APK 私有目录，让 classloader 找到。
4. **性能**：模拟器 + 转译 + 网络桥接，首次搜索延迟可能在 3-10s。桌面端需有超时处理。

## PoC 步骤

### Phase 1: ARM .so 加载验证（可跳过，环境已确认）

从 JAR 提取 wexguard_v8.so，推送到模拟器 `/data/local/tmp/libwexguard_v8.so`，写最小 Java + DEX 测试 `System.load`。

### Phase 2: 最小桥接 APK（核心 PoC）

- Kotlin + 标准 Android Activity（无需 UI，后台 Service）
- 内嵌 HTTP 服务器（用 NanoHTTPd 或 Kotlin 协程 + `ServerSocket`）
- 端点：
  - `POST /search` body: `{"class": "WexXXXGuard", "kw": "keyword"}`
  - `POST /detail` body: `{"class": "WexXXXGuard", "id": "vod_id"}`
- 加载 JAR：`DexClassLoader(jarPath, dexOutputDir, librarySearchPath, parentClassLoader)`
  - `librarySearchPath` = `/data/local/tmp/`（wexguard .so 所在）
- 反射调用：`init(context)` / `searchContent(keyword)` / `detailContent(id)`
- 返回 JSON 序列化结果

### Phase 3: 集成到 QuantumTV

- `crates/core/src/spider/mod.rs` 新增 `spider_bridge_search/detail` 函数
- 检测 class_name 是否含 `Guard`（或通过配置决定是否走桥接）
- HTTP POST 到 `127.0.0.1:8080`
- 超时 15s，失败 fallback 到现有 JVM 路径（非 Guard 类）

### Phase 4: AVD 生命周期管理

- 桌面端启动时检查模拟器是否运行（`adb get-state` 或 `adb devices`）
- 若未运行，自动启动：`emulator -avd wexbridge -no-window -no-audio -no-boot-anim -gpu swiftshader_indirect -read-only`
- 后台轮询 adb 连接状态、bridge 端口可达性

## 文件结构（PoC）

```
android/spider-bridge/
├── app/
│   ├── build.gradle.kts
│   └── src/main/
│       ├── AndroidManifest.xml
│       ├── java/com/quantumtv/bridge/
│       │   ├── BridgeService.kt       # 后台 HTTP 服务
│       │   ├── SpiderLoader.kt         # DexClassLoader + 反射调用
│       │   └── BridgeServer.kt         # 轻量 HTTP server
│       └── res/ (空)
├── build.gradle.kts
├── settings.gradle.kts
└── gradle/wrapper/
```

## 端口约定

| 用途 | 端口 | 协议 |
|------|------|------|
| 桥接 HTTP API | 8080 | HTTP (JSON) |
| adb forward | 8080→8080 | `adb forward tcp:8080 tcp:8080` |

## 之后事项

1. 确认 PoC 方向后：Phase 1 验证（快速，30min）
2. 写 APK 骨架（Phase 2，2-3h）
3. 集成到 core（Phase 3，1h）
4. 生命周期管理（Phase 4，1h）
5. 端到端测试：从桌面端搜索 wex 站点，走桥接返回结果

## 替代方案（已否决）

- **JVM 直接加载 DEX + ARM .so**：x86 上 Android .so 无法加载，dex2jar 死路（已验证）
- **Docker Android 容器**：anbox/waydroid 需要内核模块，Win11 不支持
- **C#/Rust 重写 spider**：wex 加密协议未知，无法逆向