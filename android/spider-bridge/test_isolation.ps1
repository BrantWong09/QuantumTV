# 隔离验收 (方案 §58/§60/§62/§65): 在 MuMu 上断言 playerContent 永久挂死的爆炸半径。
# 设备无 curl → 全部 HTTP 从宿主机经 adb forward 打 control 的 8080。
# 用法: powershell -File android\spider-bridge\test_isolation.ps1 [-Adb <path>]
param(
    [string]$Adb = "D:\Program Files\Netease\MuMu\nx_main\adb.exe",
    [string]$Dev = "emulator-5554"
)
$ErrorActionPreference = 'Stop'

function Invoke-Bridge([string]$port, [string]$path, [int]$timeoutSec = 5, [string]$Body = '{}') {
    try {
        return (Invoke-WebRequest -UseBasicParsing -Uri "http://127.0.0.1:$port$path" `
            -Method POST -Body $Body -ContentType 'application/json' -TimeoutSec $timeoutSec).Content
    } catch {
        if ($_.Exception.Response) {
            $sr = New-Object System.IO.StreamReader($_.Exception.Response.GetResponseStream())
            return $sr.ReadToEnd()
        }
        return "HTTP_ERROR: $($_.Exception.Message)"
    }
}
function Assert-Log([string]$pattern, [string]$label) {
    $hit = & $adb -s $Dev logcat -d | Select-String $pattern | Select-Object -Last 1
    if (-not $hit) { throw "FAIL[$label]: 未命中 /$pattern/" }
    Write-Host "PASS[$label]: $($hit.Line.Substring([Math]::Max(0,$hit.Line.Length-90)))"
}
function Assert-Match([string]$text, [string]$pattern, [string]$label) {
    if ($text -notmatch $pattern) { throw "FAIL[$label]: '$text' 未命中 /$pattern/" }
    Write-Host "PASS[$label]: $pattern"
}

& $adb -s $Dev install -r "$PSScriptRoot\out\bridge.apk" | Out-Null
& $adb -s $Dev shell am force-stop com.quantumtv.bridge
& $adb logcat -c
& $adb -s $Dev forward tcp:15555 tcp:8080 | Out-Null   # 观察通道 (health)
& $adb -s $Dev forward tcp:15999 tcp:8080 | Out-Null   # 挂死请求通道 (长超时)
& $adb -s $Dev shell am start -n com.quantumtv.bridge/.MainActivity | Out-Null
Start-Sleep 8

Assert-Log "worker=playback .*status=ready" "ready-after-spawn (§22)"

# —— 制造 playback 永久挂死 (§58): 后台长超时请求, 主流程继续观察挂死窗口 ——
$hangReq = Start-Job -ArgumentList $Dev {
    param($d)
    $a = "D:\Program Files\Netease\MuMu\nx_main\adb.exe"
    & $a -s $d forward tcp:15999 tcp:8080 | Out-Null
    try {
        (Invoke-WebRequest -UseBasicParsing -Uri "http://127.0.0.1:15999/__test_hang" `
            -Method POST -Body '{}' -ContentType 'application/json' -TimeoutSec 90).Content
    } catch { "THREW: $($_.Exception.Message)" }
}
Start-Sleep 13   # TEST_HANG 硬超时 10s + kill/restart 余量

# ① §70: 超时 → action=kill
Assert-Log "worker=playback .*action=kill" "hard timeout kill (§70)"
# ② §22/§85②: 自动重启 ready
Assert-Log "worker=playback .*status=ready" "auto restart (§22)"
# ③ §24/§62/§65: 挂死期间 control HTTP 与 general worker 健在, 桌面 TCP 不断
$health = Invoke-Bridge 15555 "/health"
Assert-Match $health '"code":200' "health-decoupled (§24)"
# ④ §35: 挂死请求有明确回包 worker_killed (而非静默 timeout)
Wait-Job $hangReq -Timeout 90 | Out-Null
$resp = (Receive-Job $hangReq | Out-String).Trim()
Remove-Job $hangReq -Force
Write-Host "hang response: $resp"
Assert-Match $resp "worker_killed" "in-flight request answered on kill (§35)"

Write-Host "`nISOLATION TESTS OK"

# ================= Phase B: 类级熔断 §61/§41 =================
# 重启 App 清掉 crash-loop/disabled 状态, 独立验证熔断
& $adb -s $Dev shell am force-stop com.quantumtv.bridge
& $adb logcat -c
& $adb -s $Dev shell am start -n com.quantumtv.bridge/.MainActivity | Out-Null
Start-Sleep 8

function Wait-PlaybackReady([int]$maxSec = 30) {
    for ($i = 0; $i -lt $maxSec; $i++) {
        $h = Invoke-Bridge 15555 "/health" 5
        if ($h -match '"playback":"ready"') { return }
        Start-Sleep 1
    }
    throw "FAIL: playback 未在 ${maxSec}s 内 ready"
}

# 3× playerContent 挂死超时 (经 /__test_hang; 每次等 restart 完成再打下一个)
foreach ($i in 1..3) {
    Wait-PlaybackReady
    $r = Invoke-Bridge 15999 "/__test_hang" 60
    Assert-Match $r "worker_killed" "hang#$i → killed"
}
# 第 4 次: 同 class 的 playerContent 必须被类级熔断秒拒 (§41, 不再烧 90s)
$blocked = Invoke-Bridge 15555 "/playerContent" 5 -Body '{"class":"WextestHang","id":"x"}'
Assert-Match $blocked "source_circuit_open" "class breaker OPEN (§42)"
# 其他 class 不受影响 → 走到派发层; playback 因 §40 crash-loop 已 DISABLED (证明熔断是 class 维度而非全局)
$other = Invoke-Bridge 15555 "/playerContent" 5 -Body '{"class":"Wexother","id":"x"}'
Assert-Match $other "worker_disabled" "other class NOT circuit-blocked (§41)"
# health: general 健在 + sources_open 可见
$health2 = Invoke-Bridge 15555 "/health"
Assert-Match $health2 '"general":"ready"' "general alive through storm (§61)"
Assert-Match $health2 'WextestHang' "sources_open in health (§48)"
Write-Host "BREAKER TESTS OK"

# ============ Phase C: 在途保护 + 并行排队 + disabled 自愈 (真机故障回归) ============
# 此时 playback 已 DISABLED (Phase B 遗留), general 正常
# C1: 真实 spider 首调会冻结 HB (native 反调试/houdini dlopen), 在途 8s 不得被 hbStale 误杀
$frz = Invoke-Bridge 15555 "/__test_freeze" 60 -Body '{"role":"general","ms":8000}'
Assert-Match $frz '"code":200' "in-flight HB gap must NOT kill (legit slow call > 5s)"
# C2: 并发 5 个 2s 调用应串行排队全部应答, 不得 worker_restarting 秒拒
$jobs = 1..5 | ForEach-Object {
    Start-Job -ArgumentList $_ {
        param($n)
        $a = "D:\Program Files\Netease\MuMu\nx_main\adb.exe"
        & $a -s emulator-5554 forward "tcp:170$n" tcp:8080 | Out-Null
        try {
            (Invoke-WebRequest -UseBasicParsing -Uri "http://127.0.0.1:170$n/__test_freeze" `
                -Method POST -Body ('{"role":"general","ms":2000}') -TimeoutSec 60).Content
        } catch { "ERR:$_" }
        finally { & $a -s emulator-5554 forward --remove "tcp:170$n" | Out-Null }
    }
}
Wait-Job $jobs -Timeout 150 | Out-Null
$outs = ($jobs | Receive-Job | Out-String)
Remove-Job $jobs -Force
Write-Host "parallel results: $outs"
if ($outs -notmatch "worker_restarting") {
    Write-Host "PASS[parallel queued (no BUSY-refuse)]"
} else {
    Write-Host "FAIL[parallel queued (no BUSY-refuse)]"; throw "parallel requests were refused"
}
# C3: 短冷却配置生效后, DISABLED worker 必须半开自愈
$cfg = Invoke-Bridge 15555 "/__test_config" 5 -Body '{"disable_recovery_ms":3000}'
Assert-Match $cfg '"code":200' "test config accepted"
Start-Sleep 8
$probe = Invoke-Bridge 15555 "/playerContent" 60 -Body '{"class":"Wexprobe","id":"x"}'
Write-Host "probe after cooldown: $probe"
if ($probe -notmatch "worker_disabled") {
    Write-Host "PASS[disabled auto-recovery half-open (§40)]"
} else {
    Write-Host "FAIL[disabled auto-recovery half-open (§40)]"; throw "no recovery from DISABLED"
}
Wait-PlaybackReady 40 | Out-Null
Write-Host "PHASE C TESTS OK"
