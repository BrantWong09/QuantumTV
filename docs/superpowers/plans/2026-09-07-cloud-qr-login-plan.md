# 网盘扫码登录（桌面端驱动）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 夸克/UC/百度网盘改为桌面端原生扫码 API 登录：桌面大窗口渲染二维码，扫码确认后 cookie 推送给桥接 APK（新增 /setCookie 端点）。

**Architecture:** crates/core 新模块 `qrcodelogin`（纯函数解析 + 薄 HTTP）→ Tauri 命令 `cloud_login_start/cloud_login_poll` → 前端 Modal 用 qrcode 包自绘（夸克/UC）或官方 PNG（百度）→ Confirmed 后桌面端 POST 桥接 `/setCookie`，APK 写 CookieManager + 落盘 + invalidateSpiders。

**Tech Stack:** Rust (reqwest/serde/uuid/base64，全部已有依赖)、Java (Android BridgeService)、React/TS (qrcode@1.5.4 已有)。

**Spec:** docs/superpowers/specs/2026-09-07-cloud-qr-login-design.md

## Global Constraints

- 不新增任何 crate/npm 依赖（base64/uuid/reqwest/qrcode 均已在依赖里）
- reqwest client 一律 `no_proxy()` + 15s 超时 + `redirect(Policy::none())`（换 cookie 要读 302 响应的 Set-Cookie）
- cookie 值绝不写日志，只打长度
- 夸克/UC 二维码内容: `https://su.quark.cn/4_eMHBJ?token=<t>&client_id=<532|381>&ssb=weblogin`；百度 = 官方 PNG `passport.baidu.com/v2/api/qrcode?sign=<sign>&lp=pc` 转 base64
- cas 轮询响应两种口径都要兼容: `data.members.status` 字符串 (CONFIRMED/SCANED/EXPIRED) 与顶层数字 status (2000000/50004001/50004002)
- APK 重打包后必须 `adb install -r`（`ensure_apk_installed` 只查包名不查版本，不会自动升级）
- `docs/` 在 .gitignore 里，只提交代码文件
- 桥接本地端口: 当前开发进程以 `QUANTUMTV_ADB_HOST_PORT=18090` 启动；ADB 上已有旧 forward `127.0.0.1:5555→18081` 可用于冒烟
- 涉及网盘: 本期只做 quark/uc/baidu（ali 留扩展位，tianyi/115/yidong 维持模拟器登录）

---

### Task 1: qrcodelogin 模块骨架 + 纯函数（TDD）

**Files:**
- Create: `crates/core/src/qrcodelogin/mod.rs`
- Modify: `crates/core/src/lib.rs`（模块声明区，`pub mod spider;` 后加一行）

**Interfaces:**
- Produces: `QrKind`（serde tag=kind/content=data, Text|PngBase64）、`QrSession{drive,qr,token,cas_cookies}`、`PollOutcome`（serde tag=status/content=data rename_all=lowercase, Waiting|Scanned|Confirmed{cookie}|Expired）、纯函数 `parse_cas_token` / `parse_cas_poll` / `parse_baidu_poll` / `set_cookie_kv` / `build_cookie`、常量 `SUPPORTED`
- 后续任务消费: Task 2/3 用纯函数写 HTTP 层；Task 4 用 QrSession/PollOutcome 做 Tauri 命令

- [ ] **Step 1: 写失败测试**

创建 `crates/core/src/qrcodelogin/mod.rs`，先只写类型 + 纯函数签名（返回 todo!()）+ 完整测试模块：

```rust
//! 网盘扫码登录 (桌面端驱动, 原生扫码 API)
//!
//! 夸克/UC: uop CAS (token → 前端渲染 su 链接二维码 → 轮询 → service_ticket 换 cookie)
//! 百度: passport getqrcode (官方 PNG 二维码 → unicast 轮询 → v 换 BDUSS)
//! Cookie 由调用方经桥接 /setCookie 写入 APK CookieManager (wex spider 原生读取通道)。
//! 参考: gaozhangmin/boxplayer, nuu987/tvbox-auxiliary, xiaoya-alist, DecryptLogin。

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// 本期支持扫码的网盘
pub const SUPPORTED: &[&str] = &["quark", "uc", "baidu"];

/// 二维码负载: Text = 前端 qrcode 包自绘; PngBase64 = 网盘官方 PNG
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum QrKind {
    Text(String),
    PngBase64(String),
}

/// 扫码会话 (前端 poll 时原样回传)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QrSession {
    pub drive: String,
    pub qr: QrKind,
    /// 夸克/UC: cas token; 百度: getqrcode sign
    pub token: String,
    /// 取码响应携带的会话 cookie (k=v), 换 cookie 时并入请求与最终串
    pub cas_cookies: Vec<String>,
}

/// 轮询结果; Confirmed 表示 cookie 交换已完成
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", content = "data", rename_all = "lowercase")]
pub enum PollOutcome {
    Waiting,
    Scanned,
    Confirmed { cookie: String },
    Expired,
}

fn millis() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

/// cas 取码响应 → token; 兼容 status=200 (nuu987) 与 2000000 (xiaoya) 两种口径
pub(crate) fn parse_cas_token(_json: &serde_json::Value) -> Option<String> {
    todo!()
}

pub(crate) enum CasPoll {
    Waiting,
    Scanned,
    Expired,
    /// service_ticket
    Confirmed(String),
}

/// cas 轮询解析: 优先 data.members.status 字符串, 兜底顶层数字 status
pub(crate) fn parse_cas_poll(_json: &serde_json::Value) -> CasPoll {
    todo!()
}

pub(crate) enum BaiduPoll {
    Waiting,
    Scanned,
    Expired,
    /// 一次性换票 token v
    Confirmed(String),
}

/// 百度 unicast 解析: errno 1=等待 0=有消息(-1/-2 过期); channel_v 是内嵌 JSON 字符串需二次解析
pub(crate) fn parse_baidu_poll(_json: &serde_json::Value) -> BaiduPoll {
    todo!()
}

/// "k=v; Path=/; HttpOnly" → "k=v"; 无 '=' 返回 None
pub(crate) fn set_cookie_kv(_set_cookie: &str) -> Option<String> {
    todo!()
}

/// 合并 cookie 对: 同 key 后者覆盖前者, 输出 "k=v; k2=v2"
pub(crate) fn build_cookie(base: &[String], extra: Vec<String>) -> String {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cas_token_accepts_both_status_shapes() {
        assert_eq!(
            parse_cas_token(&json!({"status":200,"data":{"members":{"token":"t1"}}})).as_deref(),
            Some("t1")
        );
        assert_eq!(
            parse_cas_token(&json!({"status":2000000,"data":{"members":{"token":"t2"}}})).as_deref(),
            Some("t2")
        );
        assert_eq!(parse_cas_token(&json!({"status":500,"message":"err"})), None);
        assert_eq!(parse_cas_token(&json!({"status":200,"data":{"members":{}}})), None);
    }

    #[test]
    fn cas_poll_member_status_string_wins() {
        let j = json!({"status":2000000,"data":{"members":{"status":"SCANED"}}});
        assert!(matches!(parse_cas_poll(&j), CasPoll::Scanned));
        let j = json!({"status":2000000,"data":{"members":{"status":"EXPIRED"}}});
        assert!(matches!(parse_cas_poll(&j), CasPoll::Expired));
        let j = json!({"status":2000000,"data":{"members":{"status":"CONFIRMED","service_ticket":"st1"}}});
        assert!(matches!(parse_cas_poll(&j), CasPoll::Confirmed(ref t) if t == "st1"));
    }

    #[test]
    fn cas_poll_top_level_fallback() {
        // xiaoya 口径: 顶层 2000000 + service_ticket, 无 members.status 字符串
        let j = json!({"status":2000000,"data":{"members":{"service_ticket":"st2"}}});
        assert!(matches!(parse_cas_poll(&j), CasPoll::Confirmed(ref t) if t == "st2"));
        // 等待扫码
        let j = json!({"status":50004001,"message":"query result is empty"});
        assert!(matches!(parse_cas_poll(&j), CasPoll::Waiting));
        // 过期三连
        for code in [50004002i64, 50004003, 50004004] {
            let j = json!({"status":code});
            assert!(matches!(parse_cas_poll(&j), CasPoll::Expired), "code={code}");
        }
    }

    #[test]
    fn baidu_poll_status_mapping() {
        assert!(matches!(parse_baidu_poll(&json!({"errno":1})), BaiduPoll::Waiting));
        assert!(matches!(parse_baidu_poll(&json!({"errno":-1})), BaiduPoll::Expired));
        assert!(matches!(parse_baidu_poll(&json!({"errno":-2})), BaiduPoll::Expired));
        let j = json!({"errno":0,"channel_v":"{\"status\":0,\"v\":\"tok123\"}"});
        assert!(matches!(parse_baidu_poll(&j), BaiduPoll::Confirmed(ref v) if v == "tok123"));
        let j = json!({"errno":0,"channel_v":"{\"status\":1}"});
        assert!(matches!(parse_baidu_poll(&j), BaiduPoll::Scanned));
        // 已扫但 v 为空 → 继续等
        let j = json!({"errno":0,"channel_v":"{\"status\":0,\"v\":\"\"}"});
        assert!(matches!(parse_baidu_poll(&j), BaiduPoll::Waiting));
        // channel_v 非 JSON (中间态) → 等待
        let j = json!({"errno":0,"channel_v":"garbage"});
        assert!(matches!(parse_baidu_poll(&j), BaiduPoll::Waiting));
    }

    #[test]
    fn set_cookie_kv_strips_attributes() {
        assert_eq!(set_cookie_kv("BDUSS=abc123; Path=/; HttpOnly").as_deref(), Some("BDUSS=abc123"));
        assert_eq!(set_cookie_kv("__puus=xyz").as_deref(), Some("__puus=xyz"));
        assert_eq!(set_cookie_kv("invalid"), None);
        assert_eq!(set_cookie_kv(""), None);
    }

    #[test]
    fn build_cookie_later_wins_per_key() {
        let base = vec!["__pus=old".to_string(), "__kp=kp1".to_string()];
        let extra = vec![
            "__pus=new".to_string(),
            "__puus=pu1".to_string(),
            "__puus=pu2".to_string(),
        ];
        assert_eq!(build_cookie(&base, extra), "__kp=kp1; __pus=new; __puus=pu2");
        assert_eq!(build_cookie(&[], vec![]), "");
    }
}
```

修改 `crates/core/src/lib.rs`，在 `pub mod spider;` 之后加：

```rust
pub mod qrcodelogin;
```

（按字母序应放在 playback 之后 source_selection 之前，与现有顺序一致即可）

- [ ] **Step 2: 跑测试确认编译失败/panic**

Run: `cargo test -p quantumtv-core qrcodelogin`
Expected: 编译通过但测试 panic（todo!()），全部测试红

- [ ] **Step 3: 实现纯函数**

把 5 个 todo!() 替换为：

```rust
pub(crate) fn parse_cas_token(json: &serde_json::Value) -> Option<String> {
    let st = json.get("status").and_then(|v| v.as_i64());
    if !matches!(st, Some(200) | Some(2_000_000)) {
        return None;
    }
    json.pointer("/data/members/token").and_then(|v| v.as_str()).map(str::to_string)
}

pub(crate) fn parse_cas_poll(json: &serde_json::Value) -> CasPoll {
    let members = json.pointer("/data/members");
    if let Some(s) = members.and_then(|m| m.get("status")).and_then(|v| v.as_str()) {
        match s {
            "CONFIRMED" => {
                if let Some(t) = members
                    .and_then(|m| m.get("service_ticket"))
                    .and_then(|v| v.as_str())
                {
                    return CasPoll::Confirmed(t.to_string());
                }
            }
            "SCANED" => return CasPoll::Scanned,
            "EXPIRED" => return CasPoll::Expired,
            _ => {}
        }
    }
    match json.get("status").and_then(|v| v.as_i64()) {
        Some(2_000_000) => match json.pointer("/data/members/service_ticket").and_then(|v| v.as_str()) {
            Some(t) => CasPoll::Confirmed(t.to_string()),
            None => CasPoll::Waiting,
        },
        Some(50004002) | Some(50004003) | Some(50004004) => CasPoll::Expired,
        _ => CasPoll::Waiting,
    }
}

pub(crate) fn parse_baidu_poll(json: &serde_json::Value) -> BaiduPoll {
    match json.get("errno").and_then(|v| v.as_i64()) {
        Some(1) => BaiduPoll::Waiting,
        Some(-1) | Some(-2) => BaiduPoll::Expired,
        Some(0) => {
            let cv = json.get("channel_v").and_then(|v| v.as_str()).unwrap_or("");
            let cv: serde_json::Value = serde_json::from_str(cv).unwrap_or(serde_json::Value::Null);
            match cv.get("status").and_then(|v| v.as_i64()) {
                Some(0) => match cv.get("v").and_then(|v| v.as_str()) {
                    Some(v) if !v.is_empty() => BaiduPoll::Confirmed(v.to_string()),
                    _ => BaiduPoll::Waiting,
                },
                Some(1) => BaiduPoll::Scanned,
                _ => BaiduPoll::Waiting,
            }
        }
        _ => BaiduPoll::Waiting,
    }
}

pub(crate) fn set_cookie_kv(set_cookie: &str) -> Option<String> {
    let first = set_cookie.split(';').next()?.trim();
    if first.is_empty() || !first.contains('=') {
        return None;
    }
    Some(first.to_string())
}

pub(crate) fn build_cookie(base: &[String], extra: Vec<String>) -> String {
    let mut out: Vec<String> = Vec::new();
    for p in base.iter().chain(extra.iter()) {
        let key = p.split('=').next().unwrap_or("");
        let prefix = format!("{key}=");
        out.retain(|x: &String| !x.starts_with(&prefix));
        out.push(p.clone());
    }
    out.join("; ")
}
```

- [ ] **Step 4: 跑测试确认全绿**

Run: `cargo test -p quantumtv-core qrcodelogin`
Expected: 全部 PASS

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/qrcodelogin/mod.rs crates/core/src/lib.rs
git commit -m "feat(cloud): qrcodelogin 模块骨架与扫码响应纯函数解析"
```

---

### Task 2: 夸克/UC 的 start/poll HTTP 层

**Files:**
- Modify: `crates/core/src/qrcodelogin/mod.rs`（追加 HTTP 部分）

**Interfaces:**
- Consumes: Task 1 的 `parse_cas_token`/`parse_cas_poll`/`set_cookie_kv`/`build_cookie`、`QrSession`/`PollOutcome`/`QrKind`
- Produces: `pub async fn start(drive: &str) -> Result<QrSession, String>`、`pub async fn poll(session: &QrSession) -> Result<PollOutcome, String>`（Task 3 百度接入同一入口，Task 4 Tauri 命令直接调用）

- [ ] **Step 1: 加 HTTP 客户端构造 + start 入口**

在 `SUPPORTED` 常量后追加：

```rust
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";
const QUARK_CLIENT_UA: &str = "quark-cloud-drive/2.5.20 Chrome/100.0.4896.160 Electron/18.3.5.4 Safari/537.36 Channel/pckk_other_ch";

/// 换 cookie 阶段可能 302, 必须禁跟随才能读到首响 Set-Cookie
fn http() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(15))
        .no_proxy()
        .build()
        .map_err(|e| format!("http client: {e}"))
}

/// 取二维码 (scan 入口)
pub async fn start(drive: &str) -> Result<QrSession, String> {
    match drive {
        "quark" => start_cas("quark", "532").await,
        "uc" => start_cas("uc", "381").await,
        "baidu" => crate::qrcodelogin::baidu::start().await,
        other => Err(format!("网盘 {other} 暂不支持扫码登录 (支持: quark/uc/baidu)")),
    }
}
```

- [ ] **Step 2: 实现 cas 取码 + 轮询 + 换 cookie**

追加：

```rust
async fn start_cas(drive: &str, client_id: &str) -> Result<QrSession, String> {
    let rid = uuid::Uuid::new_v4();
    let host = if drive == "quark" { "uop.quark.cn" } else { "api.open.uc.cn" };
    let url = format!("https://{host}/cas/ajax/getTokenForQrcodeLogin?client_id={client_id}&v=1.2&request_id={rid}");
    let resp = http()?
        .get(&url)
        .header(reqwest::header::USER_AGENT, UA)
        .send()
        .await
        .map_err(|e| format!("取码失败: {e}"))?;
    let cas_cookies: Vec<String> = resp
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(set_cookie_kv)
        .collect();
    let json: serde_json::Value = resp.json().await.map_err(|e| format!("取码解析失败: {e}"))?;
    let token = parse_cas_token(&json).ok_or_else(|| format!("取码失败: {json}"))?;
    Ok(QrSession {
        drive: drive.to_string(),
        qr: QrKind::Text(format!("https://su.quark.cn/4_eMHBJ?token={token}&client_id={client_id}&ssb=weblogin")),
        token,
        cas_cookies,
    })
}

pub async fn poll(session: &QrSession) -> Result<PollOutcome, String> {
    match session.drive.as_str() {
        "quark" | "uc" => poll_cas(session).await,
        "baidu" => crate::qrcodelogin::baidu::poll(session).await,
        other => Err(format!("网盘 {other} 暂不支持扫码登录")),
    }
}

async fn poll_cas(session: &QrSession) -> Result<PollOutcome, String> {
    let client_id = if session.drive == "quark" { "532" } else { "381" };
    let host = if session.drive == "quark" { "uop.quark.cn" } else { "api.open.uc.cn" };
    let rid = uuid::Uuid::new_v4();
    let url = format!(
        "https://{host}/cas/ajax/getServiceTicketByQrcodeToken?client_id={client_id}&v=1.2&token={}&request_id={rid}",
        session.token
    );
    let json: serde_json::Value = http()?
        .get(&url)
        .header(reqwest::header::USER_AGENT, UA)
        .send()
        .await
        .map_err(|e| format!("轮询失败: {e}"))?
        .json()
        .await
        .map_err(|e| format!("轮询解析失败: {e}"))?;
    match parse_cas_poll(&json) {
        CasPoll::Waiting => Ok(PollOutcome::Waiting),
        CasPoll::Scanned => Ok(PollOutcome::Scanned),
        CasPoll::Expired => Ok(PollOutcome::Expired),
        CasPoll::Confirmed(ticket) => {
            let cookie = exchange_cas_cookie(&session.drive, &session.cas_cookies, &ticket).await?;
            Ok(PollOutcome::Confirmed { cookie })
        }
    }
}

/// service_ticket → cookie; 夸克再补 __puus (spider 播放链路轮换所需)
async fn exchange_cas_cookie(drive: &str, cas_cookies: &[String], ticket: &str) -> Result<String, String> {
    let client = http()?;
    let info_host = if drive == "quark" { "pan.quark.cn" } else { "drive.uc.cn" };
    let url = format!("https://{info_host}/account/info?st={ticket}&lw=scan");
    let resp = client
        .get(&url)
        .header(reqwest::header::USER_AGENT, UA)
        .header(reqwest::header::COOKIE, &cas_cookies.join("; "))
        .send()
        .await
        .map_err(|e| format!("换 cookie 失败: {e}"))?;
    let mut pairs: Vec<String> = resp
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(set_cookie_kv)
        .collect();

    if drive == "quark" {
        let merged = build_cookie(cas_cookies, pairs.clone());
        let resp2 = client
            .get("https://drive-pc.quark.cn/1/clouddrive/config?pr=ucpro&fr=pc&uc_param_str=")
            .header(reqwest::header::USER_AGENT, QUARK_CLIENT_UA)
            .header(reqwest::header::REFERER, "https://pan.quark.cn/")
            .header(reqwest::header::COOKIE, &merged)
            .send()
            .await
            .map_err(|e| format!("补 __puus 失败: {e}"))?;
        let puus: Vec<String> = resp2
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .filter_map(set_cookie_kv)
            .filter(|p| p.starts_with("__puus="))
            .collect();
        pairs.extend(puus);
    }

    let cookie = build_cookie(cas_cookies, pairs);
    if cookie.is_empty() {
        return Err("登录确认成功但未取到 cookie (Set-Cookie 为空), 请重试".to_string());
    }
    Ok(cookie)
}
```

- [ ] **Step 3: 编译 + 全量测试**

Run: `cargo test -p quantumtv-core qrcodelogin`
Expected: 编译错误——`crate::qrcodelogin::baidu` 尚不存在。先建占位子模块 `crates/core/src/qrcodelogin/baidu.rs`：

```rust
//! 百度 passport 扫码 (Task 3 实现)
use super::{PollOutcome, QrSession};

pub(super) async fn start() -> Result<QrSession, String> {
    Err("baidu 扫码待实现".to_string())
}

pub(super) async fn poll(_session: &QrSession) -> Result<PollOutcome, String> {
    Err("baidu 扫码待实现".to_string())
}
```

并在 `crates/core/src/qrcodelogin/mod.rs` 顶部 use 区后加：

```rust
pub(crate) mod baidu;
```

再跑: `cargo test -p quantumtv-core qrcodelogin`
Expected: PASS（既有纯函数测试全绿）

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/qrcodelogin/
git commit -m "feat(cloud): 夸克/UC 扫码取码/轮询/换cookie HTTP 层"
```

---

### Task 3: 百度 start/poll HTTP 层

**Files:**
- Modify: `crates/core/src/qrcodelogin/baidu.rs`

**Interfaces:**
- Consumes: Task 1 的 `parse_baidu_poll`/`set_cookie_kv`/`QrKind::PngBase64`/`QrSession`/`PollOutcome`；Task 2 的 `http()`（需把 `fn http()` 可见性改为 `pub(super)`）
- Produces: `pub(super) async fn start()` / `poll()`（已在 Task 2 入口接好）

- [ ] **Step 1: 开放 http() 可见性**

`crates/core/src/qrcodelogin/mod.rs` 中 `fn http()` 改为：

```rust
pub(super) fn http() -> Result<reqwest::Client, String> {
```

- [ ] **Step 2: 实现 baidu.rs**

替换 `crates/core/src/qrcodelogin/baidu.rs` 全部内容：

```rust
//! 百度网盘扫码登录: passport getqrcode → unicast 轮询 → qrbdusslogin 换 BDUSS
//! 参考: riowang88/tvbox-source-aggregator, CharlesPikachu/DecryptLogin, BaiduPCS-Rust

use base64::Engine;
use std::time::Duration;

use super::{parse_baidu_poll, set_cookie_kv, PollOutcome, QrKind, QrSession};

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";

fn gid() -> String {
    uuid::Uuid::new_v4().simple().to_string().to_uppercase()
}

fn millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

pub(super) async fn start() -> Result<QrSession, String> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .no_proxy()
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let url = format!(
        "https://passport.baidu.com/v2/api/getqrcode?lp=pc&qrloginfrom=pc&gid={}&apiver=v3&tt={}&tpl=netdisk",
        gid(),
        millis()
    );
    let json: serde_json::Value = client
        .get(&url)
        .header(reqwest::header::USER_AGENT, UA)
        .send()
        .await
        .map_err(|e| format!("取码失败: {e}"))?
        .json()
        .await
        .map_err(|e| format!("取码解析失败: {e}"))?;
    let sign = json
        .get("sign")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("取码失败: {json}"))?
        .to_string();

    // 官方 PNG (内容为 wappass 确认页 URL), 转 base64 给前端 <img>
    let img_url = format!("https://passport.baidu.com/v2/api/qrcode?sign={sign}&lp=pc");
    let png = client
        .get(&img_url)
        .header(reqwest::header::USER_AGENT, UA)
        .send()
        .await
        .map_err(|e| format!("二维码图获取失败: {e}"))?
        .bytes()
        .await
        .map_err(|e| format!("二维码图读取失败: {e}"))?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png);

    Ok(QrSession {
        drive: "baidu".to_string(),
        qr: QrKind::PngBase64(b64),
        token: sign,
        cas_cookies: Vec::new(),
    })
}

pub(super) async fn poll(session: &QrSession) -> Result<PollOutcome, String> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .no_proxy()
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let tt = millis();
    let url = format!(
        "https://passport.baidu.com/channel/unicast?channel_id={}&tpl=netdisk&gid={}&apiver=v3&tt={tt}&_={tt}",
        session.token,
        gid()
    );
    let json: serde_json::Value = client
        .get(&url)
        .header(reqwest::header::USER_AGENT, UA)
        .send()
        .await
        .map_err(|e| format!("轮询失败: {e}"))?
        .json()
        .await
        .map_err(|e| format!("轮询解析失败: {e}"))?;

    match parse_baidu_poll(&json) {
        super::BaiduPoll::Waiting => Ok(PollOutcome::Waiting),
        super::BaiduPoll::Scanned => Ok(PollOutcome::Scanned),
        super::BaiduPoll::Expired => Ok(PollOutcome::Expired),
        super::BaiduPoll::Confirmed(v) => {
            let login_url = format!(
                "https://passport.baidu.com/v3/login/main/qrbdusslogin?v={tt}&bduss={v}&loginVersion=v4&qrcode=1&tpl=netdisk&apiver=v3&tt={tt}"
            );
            let resp = client
                .get(&login_url)
                .header(reqwest::header::USER_AGENT, UA)
                .send()
                .await
                .map_err(|e| format!("换 cookie 失败: {e}"))?;
            let cookie: Vec<String> = resp
                .headers()
                .get_all(reqwest::header::SET_COOKIE)
                .iter()
                .filter_map(|h| h.to_str().ok())
                .filter_map(set_cookie_kv)
                .filter(|p| {
                    p.starts_with("BDUSS=") || p.starts_with("STOKEN=") || p.starts_with("PTOKEN=")
                })
                .collect();
            if cookie.is_empty() {
                return Err("登录确认成功但未取到 BDUSS (Set-Cookie 为空), 请重试".to_string());
            }
            Ok(PollOutcome::Confirmed { cookie: cookie.join("; ") })
        }
    }
}
```

- [ ] **Step 3: 编译 + 测试**

Run: `cargo test -p quantumtv-core`
Expected: 全部 PASS（含既有模块测试）

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/qrcodelogin/
git commit -m "feat(cloud): 百度 passport 扫码取码/轮询/换BDUSS"
```

---

### Task 4: Tauri 命令 + /setCookie 推送

**Files:**
- Modify: `src-tauri/src/commands/netdisk.rs`
- Modify: `src-tauri/src/lib.rs:202` 附近（generate_handler 列表）

**Interfaces:**
- Consumes: `quantumtv_core::qrcodelogin::{start, poll, QrSession, PollOutcome, SUPPORTED}`、`quantumtv_core::bridge::effective_url()`
- Produces: Tauri 命令 `cloud_login_start(drive) -> QrSession`、`cloud_login_poll(session) -> PollOutcome`（前端 Task 6 invoke 名称即此）

- [ ] **Step 1: netdisk.rs 追加命令与推送函数**

在 `netdisk_launch_login` 命令之后追加：

```rust
use quantumtv_core::qrcodelogin::{self, PollOutcome, QrSession};

/// 取扫码二维码 (桌面端渲染)
#[tauri::command]
pub async fn cloud_login_start(drive: String) -> Result<QrSession, String> {
    qrcodelogin::start(&drive).await
}

/// 轮询扫码状态; Confirmed 时已完成 cookie 交换并推送给桥接
#[tauri::command]
pub async fn cloud_login_poll(session: QrSession) -> Result<PollOutcome, String> {
    match qrcodelogin::poll(&session).await? {
        PollOutcome::Confirmed { cookie } => {
            push_cookie_to_bridge(&session.drive, &cookie).await?;
            Ok(PollOutcome::Confirmed { cookie })
        }
        other => Ok(other),
    }
}

/// cookie → 桥接 APK: CookieManager + 落盘 + 重建 spider
async fn push_cookie_to_bridge(drive: &str, cookie: &str) -> Result<(), String> {
    let Some(url) = quantumtv_core::bridge::effective_url() else {
        return Err("桥接未就绪, 请稍后重试".to_string());
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .no_proxy()
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let resp = client
        .post(format!("{url}/setCookie"))
        .json(&serde_json::json!({ "drive": drive, "cookie": cookie }))
        .send()
        .await
        .map_err(|e| format!("推送桥接失败: {e}"))?;
    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("推送桥接响应异常: {e}"))?;
    if json.get("code").and_then(|v| v.as_i64()) == Some(200) {
        Ok(())
    } else {
        Err(format!("桥接 /setCookie 失败: {json}"))
    }
}
```

注意：文件顶部已有 `use tauri::State; use crate::storage::StorageManager;`，新增的 use 并入顶部。

- [ ] **Step 2: 注册命令**

`src-tauri/src/lib.rs` 的 `generate_handler!` 列表中，`commands::netdisk::netdisk_launch_login,`（约 202 行）之后加：

```rust
            commands::netdisk::cloud_login_start,
            commands::netdisk::cloud_login_poll,
```

- [ ] **Step 3: 编译验证**

Run: `cargo build -p quantumtv`
Expected: 编译成功（警告可接受）

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/commands/netdisk.rs src-tauri/src/lib.rs
git commit -m "feat(cloud): cloud_login_start/poll 命令与桥接 /setCookie 推送"
```

---

### Task 5: 桥接 APK /setCookie 端点 + 重打包 + 冒烟

**Files:**
- Modify: `android/spider-bridge/src/com/quantumtv/bridge/BridgeService.java`（handle 路由 ~L197、doSetCookie/cookieHosts/writeCookieFile 追加在 parseField 之前）
- Modify: `android/spider-bridge/src/com/quantumtv/bridge/CloudLoginActivity.java`（persistCookieFile 委托）

**Interfaces:**
- Produces: HTTP 端点 `POST /setCookie` body `{"drive":"quark|uc|baidu","cookie":"k=v; ..."}` → `{"code":200,"err":"ok"}`；`static void BridgeService.writeCookieFile(Context, drive, cookies)`（Task 4 的推送在 Task 7 E2E 时真正打通）

- [ ] **Step 1: 加路由**

`BridgeService.java` handle 方法中 `} else if ("/category".equals(path) ...` 分支之后、`} else {` 之前加：

```java
            } else if ("/setCookie".equals(path) && "POST".equalsIgnoreCase(method)) {
                resp = doSetCookie(bodyStr);
```

- [ ] **Step 2: 实现处理逻辑**

在 `parseField` 方法之前追加：

```java
    /** 桌面端扫码登录: cookie 写入 CookieManager + 落盘, 并重建 spider 实例 */
    private String doSetCookie(String body) {
        try {
            String drive = parseField(body, "drive");
            String cookie = parseField(body, "cookie");
            if (drive == null || cookie == null || cookie.isEmpty()) {
                return json(400, "missing drive/cookie", null);
            }
            String[] hosts = cookieHosts(drive);
            if (hosts == null) {
                return json(400, "unsupported drive: " + drive, null);
            }
            android.webkit.CookieManager cm = android.webkit.CookieManager.getInstance();
            cm.setAcceptCookie(true);
            for (String h : hosts) {
                cm.setCookie("https://" + h + "/", cookie);
            }
            cm.flush();
            writeCookieFile(this, drive, cookie);
            invalidateSpiders();
            Log.i(TAG, "setCookie: drive=" + drive + " len=" + cookie.length());
            return json(200, "ok", null);
        } catch (Exception e) {
            Log.e(TAG, "setCookie", e);
            return json(500, "set_cookie_failed", null);
        }
    }

    /** 各网盘登录态所在域 (与 spider 读取通道一致) */
    private static String[] cookieHosts(String drive) {
        switch (drive) {
            case "quark": return new String[]{"pan.quark.cn", "quark.cn", "uop.quark.cn", "drive-pc.quark.cn"};
            case "uc":    return new String[]{"drive.uc.cn", "uc.cn", "pc.uc.cn"};
            case "baidu": return new String[]{"pan.baidu.com", "passport.baidu.com", "wappass.baidu.com"};
            default: return null;
        }
    }

    /** 登录 cookie 落盘到 files/TV/.<drive>cookie (部分 spider 读文件兜底) */
    static void writeCookieFile(android.content.Context ctx, String drive, String cookies) {
        try {
            java.io.File dir = new java.io.File(ctx.getFilesDir(), "TV");
            if (!dir.exists()) dir.mkdirs();
            java.io.File f = new java.io.File(dir, "." + drive + "cookie");
            java.io.FileOutputStream fos = new java.io.FileOutputStream(f, false);
            if (cookies != null) fos.write(cookies.getBytes(java.nio.charset.StandardCharsets.UTF_8));
            fos.flush();
            fos.close();
            f.setReadable(true, false);
            Log.i("CloudLogin", "cookie 已写入: " + f.getAbsolutePath());
        } catch (Exception e) {
            Log.e("CloudLogin", "persist cookie file failed", e);
        }
    }
```

- [ ] **Step 3: CloudLoginActivity 去重**

`CloudLoginActivity.java` 的 `persistCookieFile` 方法体替换为委托：

```java
    /** 把登录 cookie 落盘到 files/TV/ 下 (部分 spider 读文件兜底) */
    private void persistCookieFile(String drive, String cookies) {
        BridgeService.writeCookieFile(this, drive, cookies);
    }
```

- [ ] **Step 4: 重打包 APK**

Run: `pwsh -NoProfile -File android/spider-bridge/build.ps1`
Expected: 末尾列出 dex/apk 文件，`android/spider-bridge/out/bridge.apk` 更新（时间戳为当前）

- [ ] **Step 5: 安装 + 重启桥接服务**

```bash
& "$env:LOCALAPPDATA\Android\Sdk\platform-tools\adb.exe" -s emulator-5554 install -r android\spider-bridge\out\bridge.apk
& "$env:LOCALAPPDATA\Android\Sdk\platform-tools\adb.exe" -s emulator-5554 shell am start-foreground-service -n com.quantumtv.bridge/.BridgeService
```

Expected: install 报 Success；服务启动

- [ ] **Step 6: 路由冒烟（不污染真实登录态）**

```powershell
# 经已有 forward 18081 → 设备 8080
$r = Invoke-WebRequest -Uri "http://127.0.0.1:18081/setCookie" -Method POST -ContentType "application/json" -Body '{"drive":"foo","cookie":"a=b"}' -TimeoutSec 10 -NoProxy
$r.Content
```

Expected: `{"code":400,"err":"unsupported drive: foo"}`（证明路由与参数解析通；用非法 drive 避免污染 CookieManager）
再验健康: `Invoke-WebRequest http://127.0.0.1:18081/health` → `{"code":200,...}`

- [ ] **Step 7: Commit**

```bash
git add android/spider-bridge/src/
git commit -m "feat(bridge): /setCookie 端点 (桌面扫码登录 cookie 写入 CookieManager/落盘)"
```

---

### Task 6: 前端二维码 Modal + 双入口

**Files:**
- Create: `src/components/CloudQrModal.tsx`
- Modify: `src/components/CloudAccountSettings.tsx`

**Interfaces:**
- Consumes: Task 4 命令 `cloud_login_start(drive)` / `cloud_login_poll(session)`；npm `qrcode`（`QRCode.toDataURL(content, { width, margin })`）
- Produces: `CloudQrModal` 组件 props `{ drive: string; driveName: string; onClose: () => void; showAlert: (type, title, message?) => void }`

- [ ] **Step 1: 创建 CloudQrModal.tsx**

```tsx
'use client';

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import QRCode from 'qrcode';

type QrKind = { kind: 'Text'; data: string } | { kind: 'PngBase64'; data: string };
type SessionDto = { drive: string; qr: QrKind; token: string; cas_cookies: string[] };
type PollDto =
  | { status: 'waiting' }
  | { status: 'scanned' }
  | { status: 'confirmed'; data: { cookie: string } }
  | { status: 'expired' };

const STATUS_TEXT: Record<string, string> = {
  waiting: '等待扫码…（请用对应网盘 App 扫码）',
  scanned: '已扫码, 请在手机上确认',
  confirmed: '登录成功',
  expired: '二维码已过期',
};

export default function CloudQrModal({
  drive,
  driveName,
  onClose,
  showAlert,
}: {
  drive: string;
  driveName: string;
  onClose: () => void;
  showAlert: (
    type: 'success' | 'error' | 'warning',
    title: string,
    message?: string,
  ) => void;
}) {
  const [qrSrc, setQrSrc] = useState<string | null>(null);
  const [status, setStatus] = useState<
    'loading' | 'waiting' | 'scanned' | 'confirmed' | 'expired'
  >('loading');
  const [runId, setRunId] = useState(0);

  useEffect(() => {
    let alive = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let failCount = 0;

    (async () => {
      try {
        const session: SessionDto = await invoke('cloud_login_start', { drive });
        if (!alive) return;
        if (session.qr.kind === 'Text') {
          const url = await QRCode.toDataURL(session.qr.data, {
            width: 320,
            margin: 1,
          });
          if (alive) setQrSrc(url);
        } else {
          if (alive) setQrSrc(`data:image/png;base64,${session.qr.data}`);
        }
        setStatus('waiting');

        const loop = async () => {
          if (!alive) return;
          try {
            const r: PollDto = await invoke('cloud_login_poll', { session });
            if (!alive) return;
            failCount = 0;
            if (r.status === 'confirmed') {
              setStatus('confirmed');
              showAlert('success', `${driveName} 登录成功`, '账号 cookie 已写入模拟器桥接');
              timer = setTimeout(onClose, 1500);
              return;
            }
            if (r.status === 'expired') {
              setStatus('expired');
              return;
            }
            setStatus(r.status);
          } catch (e) {
            if (!alive) return;
            const msg = e instanceof Error ? e.message : String(e);
            if (msg.includes('桥接未就绪') || msg.includes('推送桥接失败')) {
              showAlert('error', '写入模拟器失败', msg);
              return;
            }
            failCount += 1;
            if (failCount >= 5) {
              showAlert('error', '轮询失败', msg);
              return;
            }
            setStatus('waiting');
          }
          timer = setTimeout(loop, 2000);
        };
        timer = setTimeout(loop, 2000);
      } catch (e) {
        if (!alive) return;
        showAlert('error', '获取二维码失败', e instanceof Error ? e.message : String(e));
      }
    })();

    return () => {
      alive = false;
      if (timer) clearTimeout(timer);
    };
    // showAlert/onClose 由父组件保证稳定; drive/runId 变化即重启流程
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [drive, runId]);

  const expired = status === 'expired';

  return (
    <div
      className='fixed inset-0 z-50 flex items-center justify-center bg-black/60'
      onClick={onClose}
    >
      <div
        className='w-96 rounded-xl bg-white p-5 shadow-2xl dark:bg-gray-900'
        onClick={(e) => e.stopPropagation()}
      >
        <div className='mb-3 flex items-center justify-between'>
          <h3 className='text-base font-medium text-gray-900 dark:text-gray-100'>
            {driveName} 扫码登录
          </h3>
          <button
            onClick={onClose}
            className='text-gray-400 hover:text-gray-600 dark:hover:text-gray-200'
          >
            ✕
          </button>
        </div>
        <div className='flex h-80 items-center justify-center rounded-lg bg-white'>
          {qrSrc ? (
            // eslint-disable-next-line @next/next/no-img-element
            <img src={qrSrc} alt='登录二维码' className='h-72 w-72 object-contain' />
          ) : (
            <span className='text-sm text-gray-400'>正在获取二维码…</span>
          )}
        </div>
        <div className='mt-3 flex items-center justify-between'>
          <span className='text-xs text-gray-500 dark:text-gray-400'>
            {STATUS_TEXT[status]}
          </span>
          {expired && (
            <button
              onClick={() => {
                setQrSrc(null);
                setStatus('loading');
                setRunId((n) => n + 1);
              }}
              className='rounded-lg bg-blue-600 px-3 py-1 text-xs text-white hover:bg-blue-700'
            >
              刷新二维码
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
```

- [ ] **Step 2: 改造 CloudAccountSettings.tsx**

全部内容替换为：

```tsx
'use client';

import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import CloudQrModal from './CloudQrModal';

const DRIVE_META: Record<
  string,
  { name: string; desc: string }
> = {
  quark: { name: '夸克网盘', desc: '桌面端扫码登录（推荐），或备用模拟器 WebView 登录' },
  uc: { name: 'UC 网盘', desc: '桌面端扫码登录（推荐），或备用模拟器 WebView 登录' },
  baidu: { name: '百度网盘', desc: '桌面端扫码登录（推荐），或备用模拟器 WebView 登录' },
  ali: { name: '阿里云盘', desc: '模拟器中登录阿里云盘' },
  tianyi: { name: '天翼云盘', desc: '模拟器中登录天翼云盘' },
  '115': { name: '115 网盘', desc: '模拟器中登录 115 网盘' },
  yidong: { name: '移动云盘', desc: '模拟器中登录移动云盘' },
};

const QR_DRIVES: Record<string, string> = {
  quark: '夸克网盘',
  uc: 'UC 网盘',
  baidu: '百度网盘',
};

export default function CloudAccountSettings({
  showAlert,
}: {
  showAlert: (
    type: 'success' | 'error' | 'warning',
    title: string,
    message?: string,
  ) => void;
}) {
  const [launchingDrive, setLaunchingDrive] = useState<string | null>(null);
  const [qrDrive, setQrDrive] = useState<string | null>(null);

  const drives = Object.keys(DRIVE_META);

  const launchLogin = async (drive: string) => {
    setLaunchingDrive(drive);
    try {
      await invoke('netdisk_launch_login', { drive });
      showAlert(
        'success',
        '已在模拟器中打开登录页',
        '请在模拟器窗口完成登录，登录成功后 cookie 自动生效',
      );
    } catch (e) {
      showAlert(
        'error',
        '打开登录页失败',
        e instanceof Error ? e.message : String(e),
      );
    } finally {
      setLaunchingDrive(null);
    }
  };

  return (
    <div className='space-y-4'>
      <p className='text-xs text-gray-500 dark:text-gray-400'>
        网盘资源播放需要对应网盘账号登录。夸克/UC/百度推荐直接在桌面端扫码
        （手机网盘 App 扫码确认即可，无需操作模拟器）；其余网盘在模拟器窗口内完成登录。
        登录态由模拟器内的桥接保存，播放网盘源时自动使用。
      </p>

      {drives.map((drive) => {
        const meta = DRIVE_META[drive];
        const qrName = QR_DRIVES[drive];
        return (
          <div
            key={drive}
            className='flex flex-wrap items-center gap-3 rounded-lg border border-gray-200 p-3 dark:border-gray-700'
          >
            <div className='min-w-0 flex-1'>
              <div className='text-sm font-medium text-gray-900 dark:text-gray-100'>
                {meta.name}
              </div>
              <div className='mt-0.5 text-xs text-gray-500 dark:text-gray-400'>
                {meta.desc}
              </div>
            </div>
            {qrName ? (
              <div className='flex gap-2'>
                <button
                  onClick={() => setQrDrive(drive)}
                  className='rounded-lg bg-blue-600 px-3 py-1.5 text-xs text-white hover:bg-blue-700'
                >
                  扫码登录
                </button>
                <button
                  onClick={() => launchLogin(drive)}
                  disabled={launchingDrive === drive}
                  className='rounded-lg border border-gray-300 px-3 py-1.5 text-xs text-gray-600 hover:bg-gray-50 disabled:opacity-60 dark:border-gray-600 dark:text-gray-300 dark:hover:bg-gray-800'
                >
                  {launchingDrive === drive ? '正在打开...' : '模拟器登录'}
                </button>
              </div>
            ) : (
              <button
                onClick={() => launchLogin(drive)}
                disabled={launchingDrive === drive}
                className='rounded-lg bg-blue-600 px-3 py-1.5 text-xs text-white hover:bg-blue-700 disabled:opacity-60'
              >
                {launchingDrive === drive ? '正在打开...' : '在模拟器中登录'}
              </button>
            )}
          </div>
        );
      })}

      {qrDrive && (
        <CloudQrModal
          drive={qrDrive}
          driveName={QR_DRIVES[qrDrive]}
          onClose={() => setQrDrive(null)}
          showAlert={showAlert}
        />
      )}
    </div>
  );
}
```

- [ ] **Step 3: 类型检查 + lint**

Run: `npm run typecheck && npm run lint`
Expected: 两者通过（存量警告不算失败；新增文件零警告）

- [ ] **Step 4: Commit**

```bash
git add src/components/CloudQrModal.tsx src/components/CloudAccountSettings.tsx
git commit -m "feat(cloud): 桌面端扫码登录弹窗与网盘双入口"
```

---

### Task 7: 端到端冒烟（HITL）+ 收尾

**Files:**
- 无新增（验证 + 清理）

**Interfaces:**
- Consumes: Task 4 命令、Task 5 APK 端点、Task 6 UI

- [ ] **Step 1: 确认桥接就绪**

```powershell
Invoke-WebRequest -Uri "http://127.0.0.1:18081/health" -TimeoutSec 5 -NoProxy
& "$env:LOCALAPPDATA\Android\Sdk\platform-tools\adb.exe" forward --list
```

Expected: health 200；forward 存在（18081 旧 forward 或 app 自建 18090）

- [ ] **Step 2: 用户扫码实测夸克**

提示用户：管理页 → 网盘账号 → 夸克网盘「扫码登录」→ 手机夸克 App 扫码确认。
观察：二维码大图清晰；状态从 等待扫码 → 已扫码 → 登录成功；Toast 弹出。

- [ ] **Step 3: 验证 cookie 已写入**

```powershell
& "$env:LOCALAPPDATA\Android\Sdk\platform-tools\adb.exe" -s emulator-5554 logcat -d | Select-String "setCookie" | Select-Object -Last 3
```

Expected: `setCookie: drive=quark len=<N>`（N>0；日志只打长度，不打内容）

- [ ] **Step 4: 播放链路验证**

用户搜索网盘资源（如 Wogg 站点）→ 点播夸克线路 → playerContent 出直链 → 播放成功。
失败排查入口: logcat 过滤 `Bridge`，重点看 invokeSpider failed。

- [ ] **Step 5: 清理与收尾**

```powershell
grep -rn "todo!()" crates/core/src/qrcodelogin/   # 应无输出
```

确认无 `[DEBUG-]` 类临时代码残留；本计划无临时插桩。

---

## Self-Review 记录

1. **Spec 覆盖**: 夸克/UC/百度扫码（Task 2/3）、__puus 补齐（Task 2 exchange）、/setCookie + CookieManager/落盘/invalidateSpiders（Task 5）、Tauri 命令 + 桥接未就绪报错（Task 4）、Modal + 过期刷新 + 5 次失败停轮询（Task 6）、WebView 兜底保留（Task 6 次按钮）、阿里留扩展位（start() match 分支）、不涉及 ext 通道改造 ✓
2. **占位符**: 无 TBD/TODO；所有步骤含完整代码 ✓
3. **类型一致性**: QrSession{drive,qr,token,cas_cookies} 前后端一致；PollOutcome serde 小写 tag 与 PollDto 一致；`http()` 可见性 pub(super)（Task 3 Step 1）✓
