# host JVM 单测: 纯逻辑件 (零 android 依赖) 在 PC 上直接编译运行。
# APK 构建链 (build.ps1) 与此分离; 本脚本存在即编译, 新增逻辑件文件自动纳入。
$ErrorActionPreference = 'Stop'
$jdk = if ($env:JAVA_HOME -and (Test-Path "$env:JAVA_HOME\bin\javac.exe")) { $env:JAVA_HOME } else { "D:\devtools\jdk17" }
if (-not (Test-Path "$jdk\bin\javac.exe")) { throw "找不到构建 JDK: $jdk" }
$root = Join-Path $PSScriptRoot ".."
$out = Join-Path $PSScriptRoot "out"
Remove-Item $out -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $out | Out-Null

$srcs = @(
    "src\com\quantumtv\bridge\ipc\Proto.java",
    "src\com\quantumtv\bridge\ipc\WorkerState.java",
    "src\com\quantumtv\bridge\ipc\TimeoutPolicy.java",
    "src\com\quantumtv\bridge\control\SourceBreaker.java"
) | ForEach-Object { Join-Path $root $_ } | Where-Object { Test-Path $_ }
$tests = Get-ChildItem $PSScriptRoot -Filter *Test.java | ForEach-Object { $_.FullName }

$oldEAP = $ErrorActionPreference
$ErrorActionPreference = 'Continue'
& "$jdk\bin\javac.exe" -encoding UTF-8 -nowarn -d $out $srcs $tests 2>&1 | Tee-Object -FilePath "$out\javac.log" | Out-Host
$ErrorActionPreference = $oldEAP
if ($LASTEXITCODE -ne 0) { throw "hosttest javac failed" }

foreach ($tf in $tests) {
    $t = [System.IO.Path]::GetFileNameWithoutExtension($tf)
    & "$jdk\bin\java.exe" -cp $out $t
    if ($LASTEXITCODE -ne 0) { throw "$t FAILED" }
}
Write-Host "ALL HOSTTESTS OK"
