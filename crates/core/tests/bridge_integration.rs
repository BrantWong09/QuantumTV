use quantumtv_core::spider;

#[tokio::test]
async fn bridge_end_to_end_wexconfig_guard() {
    let class_name = "WexconfigGuard";
    assert!(spider::is_bridge_class(class_name));

    let url = "http://127.0.0.1:8080";
    let result = spider::spider_bridge_home(class_name, url).await;
    match result {
        Ok(home) => {
            assert!(home.contains("\"class\""), "homeContent 应含 class 数组: {}", &home[..home.len().min(200)]);
            println!("homeContent OK: {} bytes", home.len());
        }
        Err(e) => panic!("bridge homeContent 失败(需模拟器+bridge运行): {}", e),
    }
}

#[tokio::test]
async fn bridge_category_returns_video_list() {
    let class_name = "WexconfigGuard";
    let url = "http://127.0.0.1:8080";
    let result = spider::spider_bridge_category(class_name, "1", "1", url).await;
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
