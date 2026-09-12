package com.quantumtv.bridge.worker;

/** 搜索/详情/浏览 Worker 进程 (方案 §16/§44): playerContent 挂死与本进程无关。 */
public class GeneralWorkerService extends BaseSpiderWorker {
    @Override protected String role() { return "general"; }
}
