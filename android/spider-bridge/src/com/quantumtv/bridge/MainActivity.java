package com.quantumtv.bridge;

import android.app.Activity;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.os.Build;
import android.os.Bundle;
import android.util.Log;

/**
 * 桌面图标启动入口: 点击图标 → 拉起桥接前台服务 → 立即退出。
 * 让模拟器重启后用户无需依赖桌面端 adb 指令, 点一下应用图标即可恢复桥接。
 */
public class MainActivity extends Activity {
    private static final String TAG = "BridgeActivity";

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        // Android 13+ 前台服务通知需要通知权限; 未授权时仍启动服务(仅通知不可见)
        if (Build.VERSION.SDK_INT >= 33) {
            if (checkSelfPermission("android.permission.POST_NOTIFICATIONS")
                    != PackageManager.PERMISSION_GRANTED) {
                requestPermissions(new String[]{"android.permission.POST_NOTIFICATIONS"}, 1001);
                return; // 授权回调里再启动
            }
        }
        startBridge();
    }

    @Override
    public void onRequestPermissionsResult(int requestCode, String[] permissions, int[] grantResults) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults);
        startBridge();
    }

    private void startBridge() {
        try {
            Intent svc = new Intent(this, BridgeService.class);
            startForegroundService(svc);
            Log.i(TAG, "BridgeService start requested, finishing");
        } catch (Throwable t) {
            Log.e(TAG, "start BridgeService failed", t);
        } finally {
            finish();
        }
    }
}
