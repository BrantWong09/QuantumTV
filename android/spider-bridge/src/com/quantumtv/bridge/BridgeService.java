package com.quantumtv.bridge;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.os.Build;
import android.os.IBinder;
import android.util.Log;

import java.io.BufferedReader;
import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.InputStreamReader;
import java.io.OutputStream;
import java.lang.reflect.Method;
import java.net.ServerSocket;
import java.net.Socket;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.concurrent.Executors;
import java.util.concurrent.ExecutorService;

public class BridgeService extends Service {
    private static final String TAG = "Bridge";
    private static final int PORT = 8080;
    private ServerSocket server;
    private ExecutorService pool;
    private boolean initialized = false;

    @Override
    public IBinder onBind(Intent intent) { return null; }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        startForeground();
        if (server != null) return START_STICKY;
        try {
            server = new ServerSocket(PORT);
            Log.i(TAG, "HTTP server listening on " + PORT);
            pool = Executors.newFixedThreadPool(4);
            new Thread(this::acceptLoop, "BridgeAccept").start();
        } catch (Exception e) {
            Log.e(TAG, "server start failed: " + e);
        }
        return START_STICKY;
    }

    private void startForeground() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            NotificationChannel ch = new NotificationChannel("bridge", "Bridge", NotificationManager.IMPORTANCE_LOW);
            getSystemService(NotificationManager.class).createNotificationChannel(ch);
            Notification n = new Notification.Builder(this, "bridge")
                .setContentTitle("QuantumTV Bridge")
                .setContentText("running")
                .setSmallIcon(android.R.drawable.stat_notify_sync)
                .build();
            startForeground(1, n);
        }
    }

    private void acceptLoop() {
        while (!server.isClosed()) {
            try {
                Socket s = server.accept();
                pool.submit(() -> handle(s));
            } catch (Exception e) {
                Log.e(TAG, "accept: " + e);
            }
        }
    }

    private void handle(Socket s) {
        try {
            BufferedReader r = new BufferedReader(new InputStreamReader(s.getInputStream(), java.nio.charset.StandardCharsets.UTF_8));
            String line = r.readLine();
            if (line == null) { s.close(); return; }
            String[] parts = line.split(" ");
            String method = parts[0];
            String path = parts[1];
            int contentLength = 0;
            while ((line = r.readLine()) != null && !line.isEmpty()) {
                if (line.toLowerCase().startsWith("content-length:")) {
                    contentLength = Integer.parseInt(line.split(":", 2)[1].trim());
                }
            }
            char[] body = new char[contentLength];
            int read = 0;
            while (read < contentLength) {
                int n = r.read(body, read, contentLength - read);
                if (n < 0) break;
                read += n;
            }
            String bodyStr = new String(body, 0, read);
            Log.i(TAG, method + " " + path + " body=" + bodyStr);

            String resp;
            if ("/health".equals(path)) {
                resp = json(200, "ok", "{\"initialized\":" + initialized + "}");
            } else if ("/init".equals(path) && "POST".equalsIgnoreCase(method)) {
                resp = doInit(bodyStr);
            } else if ("/search".equals(path) && "POST".equalsIgnoreCase(method)) {
                resp = doSearch(bodyStr);
            } else if ("/detail".equals(path) && "POST".equalsIgnoreCase(method)) {
                resp = doDetail(bodyStr);
            } else if ("/home".equals(path) && "POST".equalsIgnoreCase(method)) {
                resp = doHome(bodyStr);
            } else if ("/category".equals(path) && "POST".equalsIgnoreCase(method)) {
                resp = doCategory(bodyStr);
            } else {
                resp = json(404, "not_found", null);
            }
            writeHttp(s, resp);
            s.close();
        } catch (Exception e) {
            Log.e(TAG, "handle: " + e);
            try { s.close(); } catch (Exception ex) {}
        }
    }

    private void writeHttp(Socket s, String body) throws Exception {
        byte[] bodyBytes = body.getBytes(java.nio.charset.StandardCharsets.UTF_8);
        OutputStream out = s.getOutputStream();
        String headers = "HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: " + bodyBytes.length + "\r\nConnection: close\r\n\r\n";
        out.write(headers.getBytes(java.nio.charset.StandardCharsets.US_ASCII));
        out.write(bodyBytes);
        out.flush();
    }

    private String json(int code, String err, String data) {
        StringBuilder sb = new StringBuilder("{\"code\":");
        sb.append(code);
        if (err != null) sb.append(",\"err\":\"").append(esc(err)).append("\"");
        if (data != null) sb.append(",\"data\":").append(data);
        sb.append("}");
        return sb.toString();
    }

    private String esc(String s) { return s.replace("\\", "\\\\").replace("\"", "\\\""); }

    private String doInit(String body) {
        try {
            Class<?> initCls = Class.forName("com.github.catvod.spider.Init");
            initCls.getMethod("init", Context.class).invoke(null, getApplication());
            initialized = true;
            return json(200, null, "{\"ok\":true}");
        } catch (Throwable t) {
            Log.e(TAG, "init failed", t);
            return json(500, t.toString(), null);
        }
    }

    private String doSearch(String body) {
        try {
            String className = parseField(body, "class");
            String keyword = parseField(body, "keyword");
            if (className == null || keyword == null) return json(400, "missing class/keyword", null);
            return invokeSpider(className, "searchContent", new Class[]{String.class, boolean.class}, new Object[]{keyword, true});
        } catch (Throwable t) {
            return json(500, t.toString(), null);
        }
    }

    private String doDetail(String body) {
        try {
            String className = parseField(body, "class");
            String ids = parseField(body, "ids");
            if (className == null || ids == null) return json(400, "missing class/ids", null);
            return invokeSpider(className, "detailContent", new Class[]{List.class}, new Object[]{java.util.Arrays.asList(ids.split(","))});
        } catch (Throwable t) {
            return json(500, t.toString(), null);
        }
    }

    private String doHome(String body) {
        try {
            String className = parseField(body, "class");
            if (className == null) return json(400, "missing class", null);
            return invokeSpider(className, "homeContent", new Class[]{boolean.class}, new Object[]{true});
        } catch (Throwable t) {
            return json(500, t.toString(), null);
        }
    }

    private String doCategory(String body) {
        try {
            String className = parseField(body, "class");
            String tid = parseField(body, "tid");
            String pg = parseField(body, "pg");
            if (className == null) return json(400, "missing class", null);
            if (tid == null) tid = "1";
            if (pg == null) pg = "1";
            return invokeSpider(className, "categoryContent",
                new Class[]{String.class, String.class, boolean.class, java.util.HashMap.class},
                new Object[]{tid, pg, true, new java.util.HashMap<String, String>()});
        } catch (Throwable t) {
            return json(500, t.toString(), null);
        }
    }

    private String invokeSpider(String className, String method, Class<?>[] paramTypes, Object[] args) {
        try {
            // 短名自动补全包名 (桌面端传 csp_ 剥离后的短名)
            String fqn = className.indexOf('.') >= 0 ? className : "com.github.catvod.spider." + className;
            Class<?> initCls = Class.forName("com.github.catvod.spider.Init");
            if (!initialized) initCls.getMethod("init", Context.class).invoke(null, getApplication());
            Method getSpider = initCls.getMethod("getSpider", String.class);
            Object spider = getSpider.invoke(null, fqn);
            if (spider == null) return json(404, "spider not found: " + fqn, null);
            Method m = spider.getClass().getMethod(method, paramTypes);
            Object result = m.invoke(spider, args);
            String data = result == null ? "null" : result.toString();
            return json(200, null, "\"" + esc(data) + "\"");
        } catch (Throwable t) {
            Log.e(TAG, "invokeSpider failed", t);
            return json(500, t.toString(), null);
        }
    }

    private String parseField(String body, String key) {
        // very minimal JSON parse for string fields
        String pat = "\"" + key + "\":\"";
        int i = body.indexOf(pat);
        if (i < 0) return null;
        int s = i + pat.length();
        int e = body.indexOf("\"", s);
        if (e < 0) return null;
        return body.substring(s, e).replace("\\\\", "\\").replace("\\\"", "\"");
    }

    @Override
    public void onDestroy() {
        try { if (server != null) server.close(); } catch (Exception e) {}
        super.onDestroy();
    }
}
