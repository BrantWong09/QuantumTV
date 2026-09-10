# 网盘扫码登录：桌面端驱动原生扫码 API（替代模拟器 WebView）

日期: 2026-09-07
状态: 已批准（方案 A）
关联: ADR 0002（Android 桥接）、docs/superpowers/specs/2026-09-04-bridge-remote-first-design.md

## 背景与问题

当前网盘登录（夸克/UC/百度）在桥接 APK 的 `CloudLoginActivity` 中用 WebView 打开网盘网页版，用户在 320x640 的模拟器窗口里操作。实测网页版二维码在模拟器分辨率下太糊无法扫码。

## 调研结论（详见会话调研，5+ 独立项目交叉印证）

三家网盘都有免浏览器的原生扫码 HTTP API，二维码由客户端自行渲染，与网页/分辨率无关：

| 网盘 | 取码 | 轮询 | 换凭证 | 产物 |
|------|------|------|--------|------|
| 夸克 | `GET uop.quark.cn/cas/ajax/getTokenForQrcodeLogin?client_id=532&v=1.2&request_id=<uuid>` | `GET uop.quark.cn/cas/ajax/getServiceTicketByQrcodeToken?client_id=532&v=1.2&token=<t>&request_id=<uuid>`，2~3s 一次；`2000000`=已确认取 `data.members.service_ticket`，`50004001`=等待，`50004002/3/4`=过期 | `GET pan.quark.cn/account/info?st=<ticket>&lw=scan` 从 Set-Cookie 取；再 `GET drive-pc.quark.cn/1/clouddrive/config?pr=ucpro&fr=pc&uc_param_str=` 补 `__puus` | `__pus`/`__kps`/`__uid`/`__puus` cookie |
| UC | 同夸克，`api.open.uc.cn/cas/ajax/getTokenForQrcodeLogin?client_id=381&v=1.2` | 同夸克同域 | `GET drive.uc.cn/account/info?st=<ticket>&lw=scan` | 同夸克 |
| 百度 | `GET passport.baidu.com/v2/api/getqrcode?lp=pc&qrloginfrom=pc&gid=<uuid>&apiver=v3&tt=<ms>&tpl=netdisk` → 返回 `sign`；二维码图 `passport.baidu.com/v2/api/qrcode?sign=<sign>&lp=pc`（内容为 wappass 确认页 URL） | `GET passport.baidu.com/channel/unicast?channel_id=<sign>&tpl=netdisk&apiver=v3&tt=<ms>&_=<ms>`，1~3s 一次；errno=1 等待；errno=0 且 channel_v.status=1 已扫；status=0 且 v 非空=确认，v 为换票 token；errno=-1/-2 过期 | `GET passport.baidu.com/v3/login/main/qrbdusslogin?v=<ms>&bduss=<v>&loginVersion=v4&qrcode=1&tpl=netdisk&apiver=v3` 从 Set-Cookie 取 | `BDUSS`/`STOKEN`/`PTOKEN`/`BAIDUID` 等 cookie |
| 阿里 | `GET passport.aliyundrive.com/newlogin/qrcode/generate.do?appName=aliyun_drive&fromSite=52&appEntrance=web&isMobile=false&lang=zh_CN&_bx-v=2.2.3` | `POST .../qrcode/query.do`（form: t/ck），`qrCodeStatus`: NEW→SCANED→CONFIRMED/EXPIRED | CONFIRMED 响应 `bizExt`（base64 JSON）→ `pds_login_result.refreshToken`；但完整播放链路还需 OAuth 层（spider 经 xhofe 中转），**本期不做** | refresh_token（非 cookie） |

二维码内容：夸克/UC = `https://su.quark.cn/4_eMHBJ?token=<token>&client_id=532&ssb=weblogin&uc_param_str=&uc_biz_str=S%3Acustom%7COPT%3ASAREA%400%7COPT%3AIMMERSIVE%401%7COPT%3ABACK_BTN_STYLE%400`（UC 域名相应替换），前端用 `qrcode` npm 包自绘；百度 = 直接用官方 `qrcode?sign=` PNG（转 base64 展示，最稳）。

TVBox 生态同构实现参考：gaozhangmin/boxplayer（quark auth.ts）、riowang88/tvbox-source-aggregator（baidu cloud-login.ts）、nuu987/tvbox-auxiliary、xiaoya-alist（quark_cookie.py）、CharlesPikachu/DecryptLogin（baidupan.py）。

## 方案（A：桌面端驱动）

扫码逻辑全部在桌面端 Rust（crates/core 新模块）实现，二维码在桌面端窗口大图渲染，成功后把 cookie 推送给桥接 APK。Java 侧仅新增一个 `/setCookie` 端点。

```
管理页(CloudAccountSettings)          桌面端 Rust                      桥接 APK
  点「扫码登录」 ──cloud_login_start──▶ crates/core/qrcodelogin
                                        └─ HTTP 取 token/sign ◀──▶ 夸克/UC/百度
  弹窗大二维码 ◀── {qr_kind, qr_data}
  每 2s ──cloud_login_poll──▶ 轮询状态 ◀──▶ 网盘
  状态: waiting / scanned / confirmed / expired
  confirmed 时 Rust 收 Set-Cookie → cookie 串
                                ──POST /setCookie──▶ BridgeService 新端点:
                                  CookieManager.setCookie(各域) + 写 files/TV/.<drive>cookie
                                  + invalidateSpiders()
  显示「登录成功」 ◀── ok
```

## 组件设计

### 1. crates/core/src/qrcodelogin/mod.rs（新模块）

```rust
pub enum QrKind { Text(String), PngBase64(String) }  // 夸克/UC=文本自绘; 百度=官方PNG
pub struct QrSession { pub drive: String, pub qr: QrKind, pub state: QrStateToken }
pub enum PollOutcome { Waiting, Scanned, Confirmed { cookie: String }, Expired }

pub async fn start(drive: &str) -> Result<QrSession, String>;
pub async fn poll(session: &QrSession) -> Result<PollOutcome, String>;
```

- `start`: 按网盘调取码接口；夸克/UC 需先 GET token（响应里的 Set-Cookie 会话值要与后续请求共用 cookie jar，reqwest Client 共享实例解决）；百度直接拿 sign
- `poll`: 轮询对应接口；Confirmed 时完成"换 cookie"最后一步（含夸克补 `__puus`），拼出完整 cookie 串（按域聚合，`k=v; k2=v2` 格式，与 CloudLoginActivity 落盘格式一致）
- 无状态函数式设计（session 由调用方持有），便于单测；HTTP 用 reqwest（core 已依赖）
- 常量: 各接口 URL/参数/状态码集中为 `const`，超时 10s/请求

### 2. 桥接 APK /setCookie 端点（BridgeService.java + ~40 行）

- 请求: `POST /setCookie` body `{"drive":"quark","cookie":"__pus=...; __kps=..."}` 
- 处理:
  1. 按 drive 映射域名列表（复用 CloudLoginActivity 的 DRIVE_HOSTS 表，提为静态常量）
  2. `CookieManager.getInstance().setCookie("https://<host>/", cookie)` 逐域写入
  3. `CookieManager.getInstance().flush()`
  4. 写 `files/TV/.<drive>cookie`（复用 CloudLoginActivity.persistCookieFile 逻辑，提为 BridgeService 静态方法）
  5. `BridgeService.invalidateSpiders()`（已存在，触发下次 /init 重建 spider）
- 响应: `{"code":200,"err":"ok"}`

### 3. APK 重打包

- BridgeService.java 修改后跑 `android/spider-bridge/build.ps1`（现有流程，含 dex 合并 spider JAR）
- 桌面端 ensure_apk_installed 检测版本不变则跳装；开发期手动 `adb install -r`

### 4. Tauri 命令（src-tauri/src/commands/netdisk.rs 扩展）

```rust
#[tauri::command] async fn cloud_login_start(drive: String) -> Result<QrSessionDto, String>
#[tauri::command] async fn cloud_login_poll(session: QrSessionDto) -> Result<PollOutcomeDto, String>
```

- session DTO 序列化后在前后端之间往返（前端每 2s 调一次 poll 传入）
- confirmed 分支内：Rust 取到 cookie 后立即 `bridge_post("/setCookie")`（复用 spider::bridge_post_with 或独立小函数），成功才算 Confirmed 返回；桥接未就绪时报"桥接未就绪"错误
- 现有 `netdisk_launch_login` 保留（WebView 兜底入口）

### 5. 前端（CloudAccountSettings.tsx + 新组件 CloudQrModal.tsx）

- quark/uc/baidu 三行按钮改为「扫码登录」（主）+「模拟器登录」（次链接）
- 点扫码登录 → `cloud_login_start` → Modal 弹窗：
  - 夸克/UC: `QRCode.toDataURL(qr_data)` 渲染 320px 白底黑码
  - 百度: `<img src="data:image/png;base64,...">`
  - 状态文案: 等待扫码… / 已扫码，请在手机上确认 / 二维码已过期（按钮「刷新」重调 start）/ 登录成功✓（1.5s 后自动关闭）
  - 轮询 setInterval 2s 调 `cloud_login_poll`；组件卸载清理
- ali/tianyi/115/yidong 维持现状（模拟器登录）
- `qrcode` npm 包已在依赖中（1.5.4）

## 数据流与错误处理

- 二维码过期: poll 返回 Expired → 前端停轮询、显示刷新按钮（不自动重启，避免无限循环）
- 轮询网络错误: 单次失败静默跳过，连续 5 次失败报错停轮询
- 换 cookie 失败（如 ticket 换 cookie 网络错误）: 返回 Err，前端提示"确认成功但换取登录态失败，请重试"
- /setCookie 推送失败: 命令返回 Err，前端提示；cookie 已在桌面端但未生效
- 桥接未就绪: start 前置检查 `bridge::effective_url()`，None 直接报错
- cookie 串格式: `k=v; k2=v2`（分号+空格分隔），与 persistCookieFile/DRIVE_KEYS 检测逻辑兼容
- 安全: cookie 仅经 adb forward 本地回环传输（127.0.0.1:<host_port>），不出本机；日志只打 cookie 长度不打内容

## 不做的事（YAGNI）

- 阿里云盘扫码（refresh_token→OAuth 链路长，订阅中无阿里站点；留扩展位：qrcodelogin 模块按 drive 分发，将来加 `ali` 分支）
- WebView 登录入口删除（保留兜底）
- ext 注入通道改造（现有 EXT_PAYLOAD/EXT_DIRTY 机制不动，setCookie 走 CookieManager 是 wex spider 原生读取路径）

## 测试

- Rust 单测（qrcodelogin）:
  - 响应解析纯函数（夸克状态码映射、百度 channel_v 二次解析、Set-Cookie 提取 k=v）
  - cookie 串拼装与 DRIVE_KEYS 兼容性
- 集成冒烟（手动，#[ignore] 或脚本）:
  - 起 app → 扫码登录夸克 → /health initialized → 搜网盘站点 → playerContent 出直链
- APK: build.ps1 重打包 → adb install -r → /setCookie 后 `adb shell dumpsys account`/logcat 验证

## 涉及文件

| 文件 | 动作 |
|------|------|
| crates/core/src/qrcodelogin/mod.rs | 新增（含单测） |
| crates/core/src/lib.rs | 挂模块 |
| android/spider-bridge/src/com/quantumtv/bridge/BridgeService.java | 新增 /setCookie |
| android/spider-bridge/src/com/quantumtv/bridge/CloudLoginActivity.java | DRIVE_HOSTS 提为共享常量 |
| src-tauri/src/commands/netdisk.rs | 新增 2 命令 |
| src-tauri/src/lib.rs | 注册命令 |
| src/components/CloudAccountSettings.tsx | 按钮改双入口 |
| src/components/CloudQrModal.tsx | 新增 |
| android/spider-bridge/out/bridge.apk | build.ps1 重打包 |
