//! 桥接集成测试。端到端用例需要活的桥接服务（模拟器+APK）：
//! 默认 `http://127.0.0.1:18080`，可用 `QUANTUMTV_BRIDGE_URL` 覆盖。
use quantumtv_core::spider;

#[ignore = "需要活的桥接服务（模拟器+APK）；默认 18080 或 QUANTUMTV_BRIDGE_URL"]
#[tokio::test]
async fn bridge_end_to_end_wexconfig_guard() {
    let class_name = "WexconfigGuard";
    assert!(spider::is_bridge_class(class_name));

    let url = std::env::var("QUANTUMTV_BRIDGE_URL").unwrap_or_else(|_| "http://127.0.0.1:18080".to_string());
    let result = spider::spider_bridge_home(class_name, &url).await;
    match result {
        Ok(home) => {
            assert!(home.contains("\"class\""), "homeContent 应含 class 数组: {}", &home[..home.len().min(200)]);
            println!("homeContent OK: {} bytes", home.len());
        }
        Err(e) => panic!("bridge homeContent 失败(需模拟器+bridge运行): {}", e),
    }
}

#[ignore = "需要活的桥接服务（模拟器+APK）；默认 18080 或 QUANTUMTV_BRIDGE_URL"]
#[tokio::test]
async fn bridge_category_returns_video_list() {
    let class_name = "WexconfigGuard";
    let url = std::env::var("QUANTUMTV_BRIDGE_URL").unwrap_or_else(|_| "http://127.0.0.1:18080".to_string());
    let result = spider::spider_bridge_category(class_name, "1", "1", &url).await;
    match result {
        Ok(cat) => {
            assert!(cat.contains("\"list\""), "categoryContent 应含 list: {}", &cat[..cat.len().min(200)]);
            println!("categoryContent OK: {} bytes", cat.len());
        }
        Err(e) => panic!("bridge categoryContent 失败: {}", e),
    }
}

#[test]
fn is_bridge_class_detection() {
    // wex Guard 类应命中
    assert!(spider::is_bridge_class("WexconfigGuard"));
    assert!(spider::is_bridge_class("WexzhizhenGuard"));
    assert!(spider::is_bridge_class("MyGuard"));
    assert!(spider::is_bridge_class("WexSomething"));
    // 普通 spider 类不命中
    assert!(!spider::is_bridge_class("XYQHiker"));
    assert!(!spider::is_bridge_class("Douban"));
    assert!(!spider::is_bridge_class("AppYs"));
}
