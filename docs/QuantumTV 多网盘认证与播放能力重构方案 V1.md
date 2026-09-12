# QuantumTV 多网盘认证与播放能力重构方案

> 目标：解决当前 QuantumTV 中“百度网盘可以播放，但夸克/UC 无法正常登录或登录后无法播放”的问题。
>
> 本方案与上一轮 Bridge 性能优化配套实施。
>
> 核心原则：
>
> **认证、网盘文件访问、播放解析、播放代理四个层次必须解耦。**
>
> 不允许再把“扫码登录成功”直接等价成“可以播放”。

---

# 1. 当前问题判断

目前 QuantumTV 的网盘播放架构：

```text
资源站
   │
   ▼
playerContent
   │
   ▼
Android Spider
   │
   ▼
手机本机代理地址
   │
   ▼
Desktop Bridge
   │
   ▼
还原真实 URL
   │
   ▼
Netdisk Gateway
   │
   ▼
播放器
```

当前表现：

```text
百度网盘
    ↓
登录/认证
    ↓
解析
    ↓
真实 URL
    ↓
播放
    ✅
```

而：

```text
夸克
    ↓
扫码
    ↓
登录态异常
    ↓
无法正常获取可用认证信息
    ↓
播放失败
```

以及：

```text
UC
    ↓
扫码
    ↓
界面显示成功
    ↓
实际 API 请求没有获得有效登录态
    ↓
解析失败
    ↓
播放失败
```

因此不能继续采用：

```text
CloudDrive
    ↓
统一 Login()
    ↓
统一 Token
```

这种抽象。

---

# 2. 新架构

网盘系统拆成四层：

```text
                    CloudDriveManager
                           │
              ┌────────────┼────────────┐
              │            │            │
            Baidu        Quark          UC
              │            │            │
         Provider       Provider      Provider
              │            │            │
              ▼            ▼            ▼
          AuthState     AuthState     AuthState
              │            │            │
              └────────────┼────────────┘
                           │
                    ResourceResolver
                           │
                    MediaResource
                           │
                    Playback Gateway
                           │
                          mpv
```

其中：

```text
Auth
```

只负责：

```text
我是谁？
我是否登录？
我的登录凭证是什么？
凭证什么时候过期？
```

而：

```text
Resolver
```

负责：

```text
这个分享链接对应什么文件？
这个文件如何获得播放地址？
```

最后：

```text
Gateway
```

负责：

```text
如何稳定地把这个媒体 URL 提供给播放器？
```

---

# 3. 不允许统一处理不同网盘

必须定义：

```rust
trait CloudDriveProvider
```

例如：

```rust
pub trait CloudDriveProvider: Send + Sync {

    fn provider_id(&self) -> &'static str;

    async fn login_status(
        &self
    ) -> Result<AuthStatus>;

    async fn start_qr_login(
        &self
    ) -> Result<QrLoginSession>;

    async fn poll_qr_login(
        &self,
        session: &QrLoginSession
    ) -> Result<LoginResult>;

    async fn refresh_auth(
        &self
    ) -> Result<AuthState>;

    async fn get_share(
        &self,
        url: &str
    ) -> Result<ShareResource>;

    async fn get_file(
        &self,
        share: &ShareResource
    ) -> Result<CloudFile>;

    async fn resolve_play_url(
        &self,
        file: &CloudFile
    ) -> Result<PlayResource>;
}
```

---

# 4. 每个网盘独立 Provider

至少：

```text
BaiduProvider
QuarkProvider
UcProvider
```

目录建议：

```text
crates/core/src/clouddrive/

    mod.rs

    provider.rs

    auth/
        mod.rs
        state.rs

    baidu/
        mod.rs
        auth.rs
        share.rs
        file.rs
        playback.rs

    quark/
        mod.rs
        auth.rs
        share.rs
        file.rs
        playback.rs

    uc/
        mod.rs
        auth.rs
        share.rs
        file.rs
        playback.rs
```

不要把所有网盘逻辑塞进：

```text
netdisk_proxy.rs
```

---

# 5. AuthState

统一认证状态，但不同 Provider 自己决定内部字段。

```rust
pub struct AuthState {

    pub provider: CloudDriveType,

    pub status: AuthStatus,

    pub credential: Credential,

    pub created_at: DateTime<Utc>,

    pub expires_at: Option<DateTime<Utc>>,

    pub last_verified_at:
        Option<DateTime<Utc>>,
}
```

状态：

```text
Unknown
Unauthenticated
QrPending
Authenticated
Expired
Invalid
Refreshing
```

---

# 6. 最重要：LoginSuccess 不等于 Authenticated

当前很可能存在：

```text
扫码成功
    ↓
前端显示成功
```

但实际上：

```text
服务端 Cookie/Token
没有正确保存
```

或者：

```text
扫码接口成功
≠
后续文件 API 可以使用
```

因此：

```text
QR Login
```

必须增加：

```text
verify_login()
```

完整流程：

```text
扫码
 ↓
登录接口成功
 ↓
保存 credential
 ↓
调用账号信息接口
 ↓
调用一个最轻量资源 API
 ↓
验证成功
 ↓
Authenticated
```

只有最后一步成功：

```text
AuthState = Authenticated
```

---

# 7. 夸克扫码流程

夸克单独实现：

```text
QuarkQrLoginSession
```

至少包含：

```rust
pub struct QuarkQrLoginSession {

    pub session_id: String,

    pub qr_data: String,

    pub qr_image: Vec<u8>,

    pub created_at: DateTime<Utc>,

    pub expires_at: DateTime<Utc>,
}
```

不要让前端自己猜二维码生命周期。

---

# 8. 二维码必须有生命周期

状态：

```text
Created
 ↓
Waiting
 ↓
Scanned
 ↓
Confirmed
 ↓
Authenticated
```

异常：

```text
Expired
Cancelled
Rejected
Failed
```

前端不能只看到：

```text
success
```

然后直接认为登录成功。

---

# 9. 夸克扫码轮询

不要：

```text
setInterval(100ms)
```

建议：

```text
500ms ~ 1500ms
```

并且后端控制状态。

例如：

```text
0~10s
1s polling

10~60s
2s polling

>60s
expired
```

---

# 10. QR Login 必须由 Rust 保存状态

不要：

```text
React
 ↓
保存 token
```

应该：

```text
React
 ↓
Tauri command
 ↓
Rust
 ↓
QuarkProvider
 ↓
保存 credential
```

原因：

Token/Cookie 不应该依赖前端 React state 生命周期。

---

# 11. Credential 必须持久化

建议：

```text
SQLite
```

或者：

```text
系统 Keyring
```

如果当前项目已经使用 SQLite 配置系统，则优先：

```text
SQLite
```

但敏感 credential：

```text
Cookie
AccessToken
RefreshToken
```

必须加密。

至少：

```text
AES-GCM
```

密钥由本机生成。

---

# 12. 不要直接存明文 Cookie

禁止：

```json
{
    "cookie": "xxx=xxx; xxx=xxx"
}
```

直接进入 SQLite。

建议：

```text
credential
    ↓
encrypt
    ↓
SQLite
```

读取：

```text
SQLite
    ↓
decrypt
    ↓
Provider
```

---

# 13. UC 登录必须做二次验证

UC 当前问题：

> 扫码了，但是不能用。

重点检查：

```text
扫码结果
    ↓
是否真正拿到了 session
    ↓
是否保存 Cookie
    ↓
是否保存 device/session 信息
    ↓
后续 API 是否携带完整认证信息
```

不要只检查：

```text
HTTP 200
```

必须检查业务层：

```json
{
    "code": 0
}
```

以及：

```text
user_id
session
token
```

是否真实存在。

---

# 14. Auth Credential 不应该只有 Token

不同网盘可能需要：

```text
Cookie
AccessToken
RefreshToken
UserId
DeviceId
SessionId
Headers
```

因此：

```rust
pub enum Credential {
    Cookie {
        cookie: String,
    },

    Token {
        access_token: String,
        refresh_token: Option<String>,
    },

    Session {
        session_id: String,
        cookie: Option<String>,
        headers: HashMap<String, String>,
    },
}
```

---

# 15. Provider 可以自己定义 Credential

更推荐：

```rust
pub struct ProviderCredential {
    pub provider: CloudDriveType,
    pub data: serde_json::Value,
}
```

然后：

```text
Baidu
    ↓
AccessToken

Quark
    ↓
Cookie + session

UC
    ↓
Cookie + session/device
```

这样不会强迫所有网盘适配成：

```text
token
```

---

# 16. 登录成功之后立即验证播放能力

不是只验证：

```text
get_user_info()
```

最好验证：

```text
get_share()
```

例如：

```text
用户配置一个测试分享
```

或者：

```text
调用一个轻量 share/file API
```

如果：

```text
账号 API 成功
分享 API 失败
```

仍然不能认为：

```text
Playback Ready
```

---

# 17. 增加 Auth Capability

增加：

```rust
pub struct CloudDriveCapabilities {

    pub login: bool,

    pub qr_login: bool,

    pub share_access: bool,

    pub file_list: bool,

    pub direct_url: bool,

    pub streaming: bool,

    pub range: bool,

    pub requires_cookie: bool,

    pub requires_refresh: bool,
}
```

例如：

```text
百度
    login
    share
    direct_url
    streaming
    range
```

夸克：

```text
login
share
file
direct_url
streaming
range
```

UC：

```text
login
share
file
direct_url
streaming
range
```

---

# 18. 播放不能依赖“原始分享 URL”

这是当前架构非常容易踩坑的地方。

错误：

```text
playerContent
 ↓
https://pan.quark.cn/s/xxx
 ↓
Gateway
 ↓
播放器
```

正确：

```text
分享 URL
 ↓
CloudDriveProvider
 ↓
ShareResource
 ↓
CloudFile
 ↓
PlayResource
 ↓
Gateway
 ↓
播放器
```

---

# 19. CloudFile

统一模型：

```rust
pub struct CloudFile {

    pub provider: CloudDriveType,

    pub file_id: String,

    pub name: String,

    pub size: Option<u64>,

    pub mime_type: Option<String>,

    pub is_video: bool,

    pub parent_id: Option<String>,
}
```

---

# 20. PlayResource

不要让播放器直接理解网盘。

```rust
pub struct PlayResource {

    pub url: String,

    pub headers: HeaderMap,

    pub range_supported: bool,

    pub expires_at: Option<DateTime<Utc>>,

    pub referer: Option<String>,

    pub user_agent: Option<String>,
}
```

这样：

```text
百度
夸克
UC
阿里
115
```

最终都变成：

```text
PlayResource
```

---

# 21. PlayResource 必须携带 Headers

这是非百度网盘特别容易失败的地方。

不能只返回：

```json
{
    "url": "https://xxx/video.mp4"
}
```

而应该：

```json
{
    "url": "https://xxx/video.mp4",
    "headers": {
        "User-Agent": "...",
        "Referer": "...",
        "Cookie": "...",
        "Authorization": "..."
    }
}
```

哪些 Header 必须带，由对应 Provider 决定。

---

# 22. Gateway 必须接收 PlayResource

当前：

```text
/netdisk/file.mp4?url=xxx
```

需要升级成：

```text
/netdisk/file
    ?resource_id=xxx
```

或者：

```text
/netdisk/{playback_id}
```

不要把：

```text
Cookie
Token
Headers
```

直接放在 URL query 中。

---

# 23. PlaybackResourceStore

新增：

```rust
PlaybackResourceStore
```

结构：

```text
playback_id
    ↓
PlayResource
```

例如：

```text
playback_id = abc123

{
    url,
    headers,
    expires_at,
    provider
}
```

播放器：

```text
http://127.0.0.1:xxxxx/netdisk/abc123
```

Gateway 根据：

```text
abc123
```

取得真正的：

```text
PlayResource
```

---

# 24. 好处

这样：

```text
夸克 URL
```

里面的：

```text
Cookie
Token
签名
```

不会暴露给：

```text
Frontend
mpv
日志
浏览器地址栏
```

安全性也更好。

---

# 25. PlayResource 生命周期

播放 URL 往往是临时的。

因此：

```text
PlayResource
```

必须支持：

```text
expires_at
```

播放时：

```text
expires_at > now
```

继续使用。

否则：

```text
重新 resolve
```

但注意：

```text
重新 resolve
≠
重新 search
```

必须：

```text
Episode
 ↓
Provider
 ↓
重新获取播放 URL
```

---

# 26. 网盘播放失败自动恢复

例如：

```text
HTTP 401
HTTP 403
```

不要直接：

```text
播放失败
```

应该：

```text
Gateway
 ↓
401/403
 ↓
检查 PlayResource 是否过期
 ↓
过期
 ↓
Resolver refresh
 ↓
新的 PlayResource
 ↓
继续播放
```

如果：

```text
认证状态失效
```

再：

```text
AuthProvider.refresh()
```

---

# 27. 不允许播放失败触发 Search

严格禁止：

```text
Playback Error
 ↓
Search
```

应该：

```text
Playback Error
 ↓
Check Auth
 ↓
Refresh Credential
 ↓
Resolve
 ↓
Play
```

只有：

```text
Episode 本身不存在
```

才允许返回：

```text
ResourceUnavailable
```

而不是重新搜索。

---

# 28. 网盘播放状态机

建议：

```text
Idle
 ↓
Resolving
 ↓
Resolved
 ↓
Starting
 ↓
Playing
 ↓
Refreshing
 ↓
Playing
```

异常：

```text
Resolving
 ↓
ResolveFailed

Playing
 ↓
Expired
 ↓
Refreshing

Refreshing
 ↓
AuthExpired
 ↓
NeedLogin
```

---

# 29. 登录状态机

```text
Unauthenticated
       │
       ▼
QrCreating
       │
       ▼
QrWaiting
       │
       ├── Expired
       │
       ├── Cancelled
       │
       ▼
QrScanned
       │
       ▼
LoginConfirmed
       │
       ▼
Verifying
       │
       ▼
Authenticated
```

这里必须明确：

```text
QrScanned
```

不是：

```text
Authenticated
```

---

# 30. 前端 UI

网盘设置页面应该显示：

```text
百度网盘
● 已登录
播放能力：正常

夸克网盘
● 已登录
播放能力：正常

UC 网盘
● 登录
播放能力：需要重新验证
```

而不是只有：

```text
已登录
```

---

# 31. 增加“测试连接”

每个网盘增加：

```text
测试登录
```

点击之后：

```text
Auth
 ↓
verify
 ↓
Share API
 ↓
File API
```

最终：

```text
✓ 登录有效
✓ 分享访问有效
✓ 文件访问有效
✓ 播放解析有效
```

或者明确：

```text
✗ 登录有效
✓ 分享访问
✗ 播放解析

原因：
xxx
```

---

# 32. 错误码必须结构化

不要：

```text
"播放失败"
```

改成：

```rust
pub enum CloudDriveError {

    NotLoggedIn,

    CredentialExpired,

    CredentialInvalid,

    QrExpired,

    QrCancelled,

    QrLoginFailed,

    ShareNotFound,

    FileNotFound,

    PermissionDenied,

    PlaybackUrlExpired,

    PlaybackResolveFailed,

    RateLimited,

    NetworkError,
}
```

---

# 33. 错误信息

前端显示：

```text
夸克网盘登录已过期，请重新扫码
```

而不是：

```text
请求失败
```

UC：

```text
UC 登录状态无效，请重新登录
```

播放：

```text
UC 文件解析成功，但播放地址已失效，正在重新获取
```

---

# 34. Bridge 与 CloudDrive 的关系

上一轮 Bridge 优化完成后：

```text
BridgeSession
```

只负责：

```text
RPC
```

不要让 Bridge 自己知道：

```text
百度
夸克
UC
```

应该：

```text
QuarkProvider
     ↓
BridgeClient
     ↓
BridgeSession
```

UC：

```text
UcProvider
     ↓
BridgeClient
     ↓
BridgeSession
```

---

# 35. Android Bridge API

建议把 Bridge API 标准化：

```text
bridge.login.start
bridge.login.poll
bridge.login.cancel

bridge.cloud.status
bridge.cloud.verify

bridge.cloud.share
bridge.cloud.file
bridge.cloud.resolve
```

不要：

```text
bridge.request("/xxx")
```

让上层自己拼各种 URL。

---

# 36. Android 端 Adapter

Android：

```text
spider-bridge
```

新增：

```text
CloudDriveAdapter
```

例如：

```text
BaiduAdapter
QuarkAdapter
UcAdapter
```

如果实际账号能力依赖某个 Android App/WebView：

```text
Adapter
```

负责：

```text
Cookie
Session
Login
API
```

Desktop 不应该知道这些细节。

---

# 37. 为什么百度能用而夸克/UC不能

重点排查以下 6 层：

```text
① QR Code
       ↓
② Login callback
       ↓
③ Credential extraction
       ↓
④ Credential persistence
       ↓
⑤ API request headers
       ↓
⑥ Playback URL generation
```

任何一层出错都会表现成：

```text
“扫码了但是不能播放”
```

因此必须逐层打日志。

---

# 38. Debug Log

开发模式：

```text
[CloudAuth]
provider=quark
event=qr_created

[CloudAuth]
provider=quark
event=qr_scanned

[CloudAuth]
provider=quark
event=login_confirmed

[CloudAuth]
provider=quark
event=credential_saved

[CloudAuth]
provider=quark
event=verify_success
```

播放：

```text
[CloudPlayback]
provider=quark
file_id=xxx
event=resolve_start

[CloudPlayback]
provider=quark
event=resolve_success
expires_at=xxx

[Gateway]
provider=quark
event=upstream_start
status=206
```

---

# 39. 严禁打印敏感信息

日志中：

```text
❌ Cookie
❌ AccessToken
❌ RefreshToken
❌ Authorization
❌ 完整播放 URL
```

只能：

```text
cookie=present
token=present
url_host=pan.xxx.com
```

---

# 40. 播放测试矩阵

建立：

```text
CloudDrive Test Matrix
```

| 平台 | 扫码 | 登录验证 | 分享 | 文件 | Resolve | Range | 播放 |
|---|---|---|---|---|---|---|---|
| 百度 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 夸克 | ❗ | ❗ | ❗ | ❗ | ❗ | ❗ | ❗ |
| UC | ❗ | ❗ | ❗ | ❗ | ❗ | ❗ | ❗ |

完成后必须达到：

```text
百度    7/7
夸克    7/7
UC      7/7
```

---

# 41. 必须准备真实测试资源

不要只测试：

```text
登录成功
```

必须准备每个平台至少：

```text
一个公开视频
一个大文件
一个需要 Range 的视频
一个多集电视剧
```

测试：

```text
首播
暂停
继续
拖动
切集
重新播放
重新启动应用
Token 过期
```

---

# 42. 重点测试场景

## 场景 A

```text
扫码
 ↓
退出 QuantumTV
 ↓
重新打开
 ↓
直接播放
```

要求：

```text
不需要再次扫码
```

---

## 场景 B

```text
登录
 ↓
播放
 ↓
等待 URL 过期
 ↓
继续播放
```

要求：

```text
自动 refresh
```

---

## 场景 C

```text
夸克登录
 ↓
播放
 ↓
拖动
```

要求：

```text
Range 正常
```

---

## 场景 D

```text
UC 登录
 ↓
播放第 1 集
 ↓
第 2 集
 ↓
第 3 集
```

要求：

```text
不重新登录
不重新搜索
不重复初始化 Bridge
```

---

# 43. 与上一轮 Bridge 优化结合

最终架构：

```text
                         CloudDrive
                              │
                 ┌────────────┼────────────┐
                 │            │            │
               Baidu        Quark          UC
                 │            │            │
              Provider     Provider      Provider
                 │            │            │
                 └────────────┼────────────┘
                              │
                       Auth / Resolve
                              │
                       BridgeSession
                              │
                      Android Spider
                              │
                       PlayResource
                              │
                    PlaybackResourceStore
                              │
                       Netdisk Gateway
                              │
                             mpv
```

---

# 44. 与上一轮性能优化的结合点

必须同时满足：

```text
Bridge Session
+
Request Multiplex
+
Search Cache
+
Resolve SingleFlight
+
Auth Session
+
PlayResource Cache
```

于是：

```text
搜索
 ↓
SearchCache
 ↓
BridgeSession
```

播放：

```text
Episode
 ↓
ResolveCache
 ↓
SingleFlight
 ↓
Provider
 ↓
BridgeSession
 ↓
PlayResource
 ↓
Gateway
 ↓
mpv
```

---

# 45. 不允许的实现方式

禁止：

```text
❌ 通过重新 Search 修复播放

❌ 播放失败重新扫码

❌ 每次播放重新登录

❌ 前端保存 Cookie

❌ URL 中携带完整 Cookie

❌ 把 Token 打进日志

❌ 百度的逻辑复制后仅修改域名作为夸克/UC

❌ 三个平台共用一个 Credential 模型

❌ HTTP 200 就认为登录成功

❌ QR confirmed 就认为登录成功

❌ Gateway 自己负责登录

❌ Gateway 自己调用 Search
```

---

# 46. 推荐实施顺序

不要同时改三个平台。

## Phase 1：认证框架

先实现：

```text
CloudDriveProvider
AuthState
Credential
QrLoginSession
verify_login
```

暂时只接：

```text
Baidu
```

保证现有百度播放不回归。

---

## Phase 2：夸克

实现：

```text
QuarkProvider
QuarkQrLogin
QuarkCredential
QuarkShare
QuarkFile
QuarkPlayback
```

目标：

```text
扫码
 ↓
验证
 ↓
分享
 ↓
文件
 ↓
播放
```

全部打通。

---

## Phase 3：UC

实现：

```text
UcProvider
UcQrLogin
UcCredential
UcShare
UcFile
UcPlayback
```

同样完整走：

```text
Auth
→ Verify
→ Share
→ File
→ Resolve
→ Gateway
→ Playback
```

---

## Phase 4：PlaybackResourceStore

将：

```text
URL
Headers
Cookie
Expires
```

从播放器调用链中抽离。

---

## Phase 5：Gateway

支持：

```text
PlayResource
Range
Headers
Refresh
Retry
```

---

# 47. 最终验收标准

## 百度

必须保持：

```text
登录        ✅
分享        ✅
播放        ✅
Range       ✅
切集        ✅
重启恢复    ✅
```

---

## 夸克

必须：

```text
扫码        ✅
登录验证    ✅
持久化      ✅
分享        ✅
文件        ✅
Resolve     ✅
播放        ✅
Range       ✅
切集        ✅
```

---

## UC

必须：

```text
扫码        ✅
登录验证    ✅
持久化      ✅
分享        ✅
文件        ✅
Resolve     ✅
播放        ✅
Range       ✅
切集        ✅
```

---

# 48. 本轮真正要解决的问题

最终不是：

```text
“让夸克能扫码”
“让 UC 能扫码”
```

而是：

```text
让 QuantumTV 建立真正的 CloudDrive Provider 层。
```

以后增加：

```text
阿里云盘
115
123
天翼
迅雷
PikPak
```

只需要：

```text
NewProvider
     ↓
Auth
     ↓
Share
     ↓
File
     ↓
Resolve
```

而：

```text
Bridge
Gateway
PlaybackManager
播放器
```

全部不用修改。

---

# 49. 最终架构原则

QuantumTV 的网盘系统最终应该遵守：

```text
认证 ≠ 分享解析

分享解析 ≠ 播放解析

播放解析 ≠ 播放代理

播放代理 ≠ 播放器
```

完整链路：

```text
CloudDrive Account
        ↓
     AuthState
        ↓
   ShareResource
        ↓
      CloudFile
        ↓
    PlayResource
        ↓
PlaybackResourceStore
        ↓
   Netdisk Gateway
        ↓
        mpv
```

这样以后某个网盘突然改登录接口：

```text
只修改 Provider/Auth
```

某个网盘修改播放 URL：

```text
只修改 Provider/Playback
```

Gateway 不需要跟着网盘规则一起变。

---

# 50. AI Coding Agent 执行要求

开始修改之前必须先完成：

```text
1. 找到当前百度网盘登录入口

2. 找到当前夸克扫码入口

3. 找到当前 UC 扫码入口

4. 找到三者 Credential 保存位置

5. 找到 playerContent 对网盘的调用路径

6. 找到 Android Spider 中实际处理扫码的代码

7. 找到 Android Spider 中实际处理 playerContent 的代码

8. 找到 Desktop 如何还原 127.0.0.1:8096/kaiser URL

9. 找到 netdisk_proxy.rs 如何构造 upstream request

10. 画出百度、夸克、UC 三条真实调用链
```

在完成以上分析之前：

```text
禁止直接重写认证代码。
```

---

# 51. 最终输出给开发 Agent 的一句话

本次任务不是“修复夸克和 UC 播放 Bug”。

而是：

> **在不破坏现有百度网盘播放的前提下，将 QuantumTV 网盘能力重构为 Provider + Auth + Share + File + Resolve + PlayResource 六层架构，并让百度、夸克、UC 分别拥有独立的认证与播放适配器；Bridge 只负责 RPC，Gateway 只负责媒体传输，PlaybackManager 不得参与网盘认证，也不得通过 Search 恢复播放。**