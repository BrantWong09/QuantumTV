package com.quantumtv.bridge.worker;

/** 播放解析专用 Worker 进程 (方案 §10/§16): 夸克 playerContent 挂死只杀本进程。 */
public class PlaybackWorkerService extends BaseSpiderWorker {
    @Override protected String role() { return "playback"; }
}
