//! 桥接生命周期集成测试。
//! 运行条件: 本机已安装 Android SDK + wexbridge AVD（或 QUANTUMTV_BRIDGE_AVD 指定的 AVD）。
//! 运行: cargo test -p quantumtv-core --test bridge_lifecycle -- --ignored --test-threads=1

use quantumtv_core::bridge::{self, BridgeConfig, BridgeStatus};

#[tokio::test(flavor = "multi_thread")]
#[ignore = "需要本机 Android SDK + AVD，真拉起模拟器（约 2 分钟）"]
async fn ensure_ready_then_shutdown_lifecycle() {
    let cfg = BridgeConfig::from_env();
    if !cfg.enabled {
        eprintln!("QUANTUMTV_BRIDGE_ENABLED=0，跳过");
        return;
    }

    bridge::ensure_ready_with(cfg.clone()).await.expect("ensure_ready 应成功");
    assert_eq!(bridge::status(), BridgeStatus::Ready);

    // 健康端点真实可达（生效 URL 可能是远程直连，不一定是本地 forward 地址）
    let url = bridge::effective_url().expect("桥接就绪后应有生效 URL");
    let client = reqwest::Client::new();
    let body = client
        .get(format!("{}/health", url))
        .send()
        .await
        .expect("health 请求应成功")
        .text()
        .await
        .expect("health 响应体");
    assert!(bridge::parse_health_body(&body), "health 应为 code=200: {}", body);

    let was_ours = bridge::we_started();
    bridge::shutdown().await;

    if was_ours {
        assert_eq!(bridge::status(), BridgeStatus::Idle);
        // 自己拉起的模拟器应已消失
        let adb = bridge::adb_path(&cfg.sdk_root);
        let out = std::process::Command::new(adb)
            .arg("devices")
            .output()
            .expect("adb devices 应可执行");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            !bridge::has_emulator_device(&text),
            "shutdown 后模拟器应消失: {}",
            text
        );
    } else {
        // 复用外部模拟器的场景：不允许关别人的设备
        eprintln!("复用外部模拟器，跳过 shutdown 断言");
    }
}
