package com.quantumtv.bridge;

import android.app.Activity;
import android.content.Context;
import android.os.Build;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.util.Log;
import android.view.View;
import android.webkit.CookieManager;
import android.webkit.WebChromeClient;
import android.webkit.WebSettings;
import android.webkit.WebView;
import android.webkit.WebViewClient;
import android.widget.FrameLayout;
import android.widget.TextView;

import java.io.File;
import java.io.FileOutputStream;
import java.nio.charset.StandardCharsets;

/**
 * 网盘账号登录页 (WebView):
 * 直接在桥接 APK 内用系统 WebView 打开网盘登录页, 用户在模拟器窗口完成登录。
 * 登录成功后 cookie 自动进入系统 CookieManager —— 这正是混淆 spider (wex 系)
 * 原生读取网盘登录态的通道 (TVBox 生态即如此), 不再需要桌面端扫码 + ext 透传。
 *
 * 用法: adb shell am start -n com.quantumtv.bridge/.CloudLoginActivity --es drive quark
 */
public class CloudLoginActivity extends Activity {
    private static final String TAG = "CloudLogin";

    /** 登录成功判据: 对应网盘的关键 cookie 出现即视为已登录 */
    private static final String[][] DRIVE_KEYS = {
            {"quark", "__kp=", "__pus="},
            {"uc", "__kp=", "__pus="},
            {"baidu", "BDUSS=", "BDUSS_BFESS="},
    };
    /** 登录页 URL 前缀, 用于判定已进入网盘主域 */
    private static final String[][] DRIVE_HOSTS = {
            {"quark", "quark.cn", "pan.quark.cn", "uop.quark.cn"},
            {"uc", "uc.cn", "pc.uc.cn"},
            {"baidu", "pan.baidu.com", "pan.baidu.com"},
    };

    private WebView webView;
    private TextView statusBar;
    private String drive;
    private final Handler handler = new Handler(Looper.getMainLooper());
    private final Runnable poller = new Runnable() {
        @Override
        public void run() {
            if (isLoggedIn(drive)) {
                onLoggedIn();
                return;
            }
            handler.postDelayed(this, 1500);
        }
    };

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        drive = getIntent().getStringExtra("drive");
        if (drive == null) drive = "quark";

        String title;
        switch (drive) {
            case "baidu": title = "百度网盘登录"; break;
            case "uc": title = "UC网盘登录"; break;
            default: title = "夸克网盘登录"; break;
        }

        // 根布局: 状态条 + WebView
        FrameLayout root = new FrameLayout(this);
        statusBar = new TextView(this);
        statusBar.setText(title + " — 登录完成后自动返回");
        statusBar.setBackgroundColor(0xFF222222);
        statusBar.setTextColor(0xFFFFFFFF);
        statusBar.setPadding(24, 24, 24, 16);
        statusBar.setTextSize(14);

        FrameLayout.LayoutParams barLp = new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.WRAP_CONTENT);
        root.addView(statusBar, barLp);

        webView = new WebView(this);
        FrameLayout.LayoutParams wvLp = new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT);
        wvLp.topMargin = (int) (64 * getResources().getDisplayMetrics().density);
        root.addView(webView, wvLp);

        setContentView(root);

        initWebView();
        webView.loadUrl(loginUrl(drive));
        handler.postDelayed(poller, 1500);
    }

    private void initWebView() {
        WebSettings s = webView.getSettings();
        s.setJavaScriptEnabled(true);
        s.setDomStorageEnabled(true);
        s.setDatabaseEnabled(true);
        s.setLoadWithOverviewMode(true);
        s.setUseWideViewPort(true);
        s.setUserAgentString("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36");

        CookieManager cm = CookieManager.getInstance();
        cm.setAcceptCookie(true);
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.LOLLIPOP) {
            cm.setAcceptThirdPartyCookies(webView, true);
        }

        webView.setWebViewClient(new WebViewClient() {
            @Override
            public boolean shouldOverrideUrlLoading(WebView view, String url) {
                // 网盘登录跳转 App 深链时留在 WebView 内
                if (url != null && (url.startsWith("https://") || url.startsWith("http://"))) {
                    view.loadUrl(url);
                    return true;
                }
                return false;
            }

            @Override
            public void onPageFinished(WebView view, String url) {
                Log.i(TAG, "page: " + url);
            }
        });
        webView.setWebChromeClient(new WebChromeClient());
        // 底部返回按钮 = 退出登录页
        webView.setLongClickable(true);
    }

    private String loginUrl(String drive) {
        switch (drive) {
            case "baidu":
                return "https://pan.baidu.com/";
            case "uc":
                return "https://pc.uc.cn/";
            case "quark":
            default:
                return "https://pan.quark.cn/";
        }
    }

    private boolean isLoggedIn(String drive) {
        String cookie = getCookiesFor(drive);
        if (cookie == null) return false;
        String[][] keys = DRIVE_KEYS;
        for (String[] kv : keys) {
            if (!kv[0].equals(drive)) continue;
            boolean needAll = true;
            for (int i = 1; i < kv.length; i++) {
                if (!cookie.contains(kv[i])) { needAll = false; break; }
            }
            if (needAll) return true;
        }
        return false;
    }

    /** 收集该网盘登录所需域名下的全部 cookie */
    private String getCookiesFor(String drive) {
        CookieManager cm = CookieManager.getInstance();
        StringBuilder sb = new StringBuilder();
        String[][] hosts = DRIVE_HOSTS;
        for (String[] h : hosts) {
            if (!h[0].equals(drive)) continue;
            for (int i = 1; i < h.length; i++) {
                String c = cm.getCookie("https://" + h[i] + "/");
                if (c != null && !c.isEmpty()) {
                    if (sb.length() > 0 && !sb.toString().endsWith("; ")) sb.append("; ");
                    sb.append(c);
                }
            }
        }
        return sb.toString();
    }

    private void onLoggedIn() {
        String cookies = getCookiesFor(drive);
        Log.i(TAG, "登录成功 (" + drive + "), cookie 长度: " + (cookies == null ? 0 : cookies.length()));
        persistCookieFile(drive, cookies);
        // 通知桥接服务清空已缓存的 spider 实例, 下次调用按新 CookieManager 重建
        BridgeService.invalidateSpiders();
        handler.removeCallbacks(poller);
        runOnUiThread(() -> {
            ToastUtil.show(this, "登录成功, 网盘账号已生效");
            finish();
        });
    }

    /** 把登录 cookie 落盘到 files/TV/ 下 (部分 spider 读文件兜底) */
    private void persistCookieFile(String drive, String cookies) {
        try {
            File dir = new File(getFilesDir(), "TV");
            if (!dir.exists()) dir.mkdirs();
            File f = new File(dir, "." + drive + "cookie");
            FileOutputStream fos = new FileOutputStream(f, false);
            if (cookies != null) {
                fos.write(cookies.getBytes(StandardCharsets.UTF_8));
            }
            fos.flush();
            fos.close();
            f.setReadable(true, false);
            Log.i(TAG, "cookie 已写入: " + f.getAbsolutePath());
        } catch (Exception e) {
            Log.e(TAG, "persist cookie file failed", e);
        }
    }

    @Override
    public void onBackPressed() {
        if (webView != null && webView.canGoBack()) {
            webView.goBack();
        } else {
            super.onBackPressed();
        }
    }

    @Override
    protected void onDestroy() {
        handler.removeCallbacks(poller);
        if (webView != null) {
            webView.loadUrl("about:blank");
            webView.stopLoading();
            webView.destroy();
            webView = null;
        }
        super.onDestroy();
    }

    /** 极简 Toast 封装 (避免依赖外部库) */
    static class ToastUtil {
        static void show(Context c, String msg) {
            android.widget.Toast.makeText(c, msg, android.widget.Toast.LENGTH_LONG).show();
        }
    }
}
