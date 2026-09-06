//! 网盘播放解析: spider 集数 id → playerContent 二次解析 → 真实直链
//!
//! wex 全系站点是网盘资源模式: detailContent 返回的"集数"是网盘分享 id(非 http),
//! 播放前必须经 playerContent(flag, id) 解析。未登录网盘时解析结果为空 → 前端给出登录提示。

use serde_json::Value;

use super::bridge_post_with;

/// 解析单集: 返回 (直链, header 对象)
/// Err 携带用户可读原因(如"需要登录夸克网盘")
pub async fn resolve_spider_episode(
    class_name: &str,
    flag: &str,
    id: &str,
    bridge_url: &str,
) -> Result<(String, Value), String> {
    let body = serde_json::json!({
        "class": class_name,
        "flag": flag,
        "id": id,
    });
    let data = match bridge_post_with(bridge_url, "/playerContent", &body, false, 120).await {
        Ok(d) => d,
        Err(e) => {
            // 桥接透传的 Java 异常(如 InvocationTargetException)几乎都是
            // spider 内部调网盘 API 失败(未登录对应网盘/cookie 失效) → 给可操作的提示
            if e.contains("InvocationTargetException")
                || e.contains("JSONException")
                || e.contains("bridge error")
            {
                return Err(format!("{} (原因: {})", netdisk_login_hint_for(class_name, flag), short_err(&e)));
            }
            return Err(e);
        }
    };
    let obj: Value = serde_json::from_str(&data)
        .map_err(|e| format!("playerContent 响应解析失败: {e}, body: {}", &data[..data.len().min(120)]))?;
    let url = obj["url"].as_str().unwrap_or("").trim().to_string();
    if url.is_empty() {
        return Err(netdisk_login_hint(class_name));
    }
    Ok((url, obj["header"].clone()))
}

/// 依据 spider 类名推断需要登录的网盘, 生成用户提示
pub fn netdisk_login_hint(class_name: &str) -> String {
    let lower = class_name.to_lowercase();
    if lower.contains("quark") || lower.contains("kuake") {
        "该源需要登录夸克网盘: 请到 管理 → 网盘账号 扫码登录".into()
    } else if lower.contains("ali") {
        "该源需要登录阿里云盘: 请到 管理 → 网盘账号 粘贴 Cookie".into()
    } else if lower.contains("baidu") {
        "该源需要登录百度网盘: 请到 管理 → 网盘账号 粘贴 Cookie".into()
    } else if lower.contains("uc") {
        "该源需要登录 UC 网盘: 请到 管理 → 网盘账号 粘贴 Cookie".into()
    } else {
        // wex 全系默认是夸克/UC 系网盘资源
        "该源为网盘资源, 需要登录网盘账号: 请到 管理 → 网盘账号 扫码/粘贴登录".into()
    }
}

/// flag(线路名)比类名更准确: 它直接就是网盘名(如 "夸克网盘"/"UC网盘"/"百度原画")
pub fn netdisk_login_hint_for(class_name: &str, flag: &str) -> String {
    let flag_lower = flag.to_lowercase();
    let where_to = "请到 管理 → 网盘账号 扫码/粘贴登录";
    if flag_lower.contains("quark") || flag.contains("夸克") {
        format!("该线路需要登录夸克网盘: {where_to}")
    } else if flag_lower.contains("uc") || flag.contains("UC网盘") {
        format!("该线路需要登录 UC 网盘: {where_to}")
    } else if flag.contains("百度") {
        format!("该线路需要登录百度网盘: {where_to}")
    } else if flag.contains("阿里") {
        format!("该线路需要登录阿里云盘: {where_to}")
    } else if flag.contains("天翼") {
        format!("该线路需要登录天翼云盘: {where_to}")
    } else if flag.contains("115") {
        format!("该线路需要登录 115 网盘: {where_to}")
    } else if flag.contains("迅雷") {
        format!("该线路需要登录迅雷云盘: {where_to}")
    }
    // flag 无信息时退回按类名推断
    else {
        netdisk_login_hint(class_name)
    }
}

/// 把桥接透传的长异常串压成一行要点
fn short_err(e: &str) -> String {
    let trimmed = e.trim();
    let end = trimmed.char_indices().nth(120).map(|(i, _)| i).unwrap_or(trimmed.len());
    trimmed[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_hint_by_class_name() {
        assert!(netdisk_login_hint("WexquarkGuard").contains("夸克"));
        assert!(netdisk_login_hint("WexzhizhenGuard").contains("网盘"));
        assert!(netdisk_login_hint("WexAliSomethingGuard").contains("阿里"));
    }

    #[test]
    fn login_hint_prefers_flag_over_class_name() {
        // flag 是线路名(网盘名), 比类名更准确
        assert!(netdisk_login_hint_for("WexmuouggGuard", "夸克网盘").contains("夸克"));
        assert!(netdisk_login_hint_for("WexmuouggGuard", "UC网盘").contains("UC"));
        assert!(netdisk_login_hint_for("WexWoBaiduPanGuard", "百度原画#01").contains("百度"));
        assert!(netdisk_login_hint_for("WexWoXunLeiPanGuard", "迅雷原画").contains("迅雷"));
        // flag 无信息时退回按类名推断
        assert!(netdisk_login_hint_for("WexquarkGuard", "").contains("夸克"));
    }
}
