package com.quantumtv.bridge.ipc;

/** Worker 生命周期状态 (方案 §18)。只有 IDLE 接受新请求; SUSPECT 起停发 (§31)。 */
public enum WorkerState {
    STARTING, IDLE, BUSY, SUSPECT, KILLING, DEAD, RESTARTING, DISABLED;

    public boolean canAcceptRequest() { return this == IDLE; }
}
