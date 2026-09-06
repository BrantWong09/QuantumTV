package com.quantumtv.bridge;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.util.Log;

/**
 * 开机自启: 模拟器重启后收到 BOOT_COMPLETED, 自动拉起桥接前台服务,
 * 不再需要手动点击图标或桌面端重新触发。
 */
public class BootReceiver extends BroadcastReceiver {
    private static final String TAG = "BridgeBoot";

    @Override
    public void onReceive(Context context, Intent intent) {
        if (intent == null) return;
        String action = intent.getAction();
        if (!Intent.ACTION_BOOT_COMPLETED.equals(action)) return;
        try {
            Intent svc = new Intent(context, BridgeService.class);
            context.startForegroundService(svc);
            Log.i(TAG, "BOOT_COMPLETED: BridgeService start requested");
        } catch (Throwable t) {
            Log.e(TAG, "BOOT_COMPLETED start failed", t);
        }
    }
}
