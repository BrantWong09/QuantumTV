# [SpiderPerf] 采样与汇总 (方案 §70-§72): 抓 logcat → 按 method+class 输出 P50/P95/max,
# 供 TimeoutPolicy 定值 (硬超时 = P95×2, 下限 30s)。
param(
    [string]$Adb = "D:\Program Files\Netease\MuMu\nx_main\adb.exe",
    [string]$Dev = "emulator-5554",
    [int]$Seconds = 120,
    [string]$Out = "worker_metrics.log"
)
$ErrorActionPreference = 'Stop'
& $adb -s $Dev logcat -c | Out-Null
Write-Host "采集中 ($Seconds s) — 请正常播放/搜索若干轮 (夸克/百度/UC 各几集)..."
Start-Sleep $Seconds
& $adb -s $Dev logcat -d | Select-String "\[SpiderPerf\]" | ForEach-Object { $_.Line } | Set-Content $Out -Encoding UTF8
Write-Host "原始日志 → $Out"

$rows = Get-Content $Out | ForEach-Object {
    if ($_ -match 'side=control role=(\w+) method=(\S+) class=(\S+) duration=(\d+)ms') {
        [pscustomobject]@{ Side='control'; Method=$Matches[2]; Class=$Matches[3]; Ms=[int]$Matches[4] }
    } elseif ($_ -match 'role=(\w+) pid=\d+ method=(\S+) class=(\S+) duration=(\d+)ms') {
        [pscustomobject]@{ Side='worker'; Method=$Matches[2]; Class=$Matches[3]; Ms=[int]$Matches[4] }
    }
}
if (-not @($rows).Count) { Write-Host "窗口内没有 [SpiderPerf] 样本"; exit 0 }

function Get-Percentile([int[]]$sorted, [double]$q) {
    $idx = [Math]::Min([int]([Math]::Ceiling($sorted.Count * $q)) - 1, $sorted.Count - 1)
    $sorted[[Math]::Max(0, $idx)]
}

$rows | Group-Object { "$($_.Side)/$($_.Method)/$($_.Class)" } | ForEach-Object {
    $ms = @($_.Group | ForEach-Object Ms | Sort-Object)
    $p95 = Get-Percentile $ms 0.95
    [pscustomobject]@{
        Group            = $_.Name
        N                = $ms.Count
        P50ms            = Get-Percentile $ms 0.50
        P95ms            = $p95
        Maxms            = $ms[-1]
        TimeoutCandidate = [Math]::Max(30000, $p95 * 2)
    }
} | Sort-Object Group | Format-Table -AutoSize
