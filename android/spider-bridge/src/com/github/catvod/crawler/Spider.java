package com.github.catvod.crawler;

import android.content.Context;

import java.util.HashMap;
import java.util.List;
import java.util.Map;

public abstract class Spider {
    public void init(Context context) throws Exception {
    }

    public void init(Context context, String api) throws Exception {
        init(context);
    }

    public String homeContent(boolean filter) throws Exception {
        return "";
    }

    public String homeVideoContent() throws Exception {
        return "";
    }

    public String categoryContent(String tid, String pg, boolean filter, HashMap<String, String> extend) throws Exception {
        return "";
    }

    public String detailContent(List<String> ids) throws Exception {
        return "";
    }

    public String searchContent(String key, boolean quick) throws Exception {
        return "";
    }

    public String searchContent(String key, boolean quick, String suffix) throws Exception {
        return "";
    }

    public String playerContent(String flag, String id, List<String> vipFlags) throws Exception {
        return "";
    }

    public String liveContent() throws Exception {
        return "";
    }

    public String action(String key) throws Exception {
        return "";
    }

    public void destroy() {
    }

    public boolean manualVideoCheck() throws Exception {
        return false;
    }

    public boolean isVideoFormat(String url) {
        return false;
    }

    public Object[] proxy(Map<String, String> param) throws Exception {
        return null;
    }

    public Object[] proxyInvoke(Map<String, String> param) throws Exception {
        return null;
    }
}
