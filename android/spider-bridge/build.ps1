param(
    [string]$SpiderJar = "D:\QuantumTV\.cache\.cache\sites\4996328b1d39e8bce7f241119238f97c\spider.jar",
    [string]$WorkRoot = "$env:TEMP\spider-bridge"
)
$ErrorActionPreference = 'Stop'
# 构建参数: -SpiderJar 指向订阅缓存中的 spider.jar (提供 wexshinidie.guard/classes.dex)
#           -WorkRoot 构建工作目录 (默认 %TEMP%\spider-bridge)
$root = $WorkRoot
$sdk = "$env:LOCALAPPDATA\Android\Sdk"
$bt = "$sdk\build-tools\30.0.3"
$plat = "$sdk\platforms\android-30\android.jar"
# python 仅用于 staging 步骤的 zip 操作, 优先用 PATH 上的 python
$python = (Get-Command python -ErrorAction SilentlyContinue).Source
if (-not $python) { $python = "python" }
# 依赖目录: 与 build.ps1 同级的 deps 目录 (okhttp/gson/kotlin-stdlib/zxing + wexguard so + debug keystore)
$deps = Join-Path $PSScriptRoot "deps"

Write-Host "=== 1/6 javac ==="
New-Item -ItemType Directory -Path "$root\src" -Force | Out-Null
# 同步源码到工作目录 (保持既有构建布局)
Copy-Item "$PSScriptRoot\src\*" "$root\src\" -Recurse -Force
Copy-Item "$PSScriptRoot\AndroidManifest.xml" "$root\AndroidManifest.xml" -Force
$srcs = Get-ChildItem "$root\src" -Recurse -Filter *.java | ForEach-Object { $_.FullName }
& javac --release 8 -encoding UTF-8 -classpath $plat -d "$root\out\classes" $srcs
if ($LASTEXITCODE -ne 0) { throw "javac failed" }

Write-Host "=== 2/6 d8 (merge) ==="
New-Item -ItemType Directory -Path "$root\out\dex" -Force | Out-Null
# 先从订阅 spider.jar 提取 classes.dex (d8 需要在这一步合并 spider 类)
& $python -c @"
import zipfile as zf2
with zf2.ZipFile(r'$SpiderJar') as zj:
    if 'classes.dex' in zj.namelist():
        open(r'$root\spider_classes.dex','wb').write(zj.read('classes.dex'))
        print('extracted spider_classes.dex')
    else:
        print('no classes.dex in spider.jar')
"@
if ($LASTEXITCODE -ne 0) { throw "extract spider_classes.dex failed" }
$classes = Get-ChildItem "$root\out\classes" -Recurse -Filter *.class | ForEach-Object { $_.FullName }
$d8Inputs = @($classes)
# 依赖 jar 缺一不可: spider 运行时需要 okhttp(okio)/gson/kotlin-stdlib/zxing
foreach ($f in @("spider_classes.dex", "gson-2.10.1.jar", "okhttp-4.12.0.jar", "okio-3.6.0.jar", "kotlin-stdlib-1.9.22.jar", "zxing-core-3.5.2.jar")) {
    $p = Join-Path $root $f
    if (Test-Path $p) {
        $d8Inputs += $p
    } elseif (Test-Path (Join-Path $deps $f)) {
        $d8Inputs += (Join-Path $deps $f)
    } else {
        Write-Warning "缺失依赖: $f (d8 将跳过, 运行时会 ClassNotFound)"
    }
}
# 校验关键依赖确实进入了 d8 输入
$required = @("okio-3.6.0.jar", "okhttp-4.12.0.jar", "kotlin-stdlib-1.9.22.jar")
foreach ($r in $required) {
    if (-not ($d8Inputs | Where-Object { (Split-Path $_ -Leaf) -eq $r })) {
        throw "关键依赖 $r 未找到, 禁止构建 (okhttp 运行时强制依赖 okio)"
    }
}
java -Xmx2048M -cp "$bt\lib\d8.jar" com.android.tools.r8.D8 --release --lib $plat --min-api 30 --output "$root\out\dex" @d8Inputs
if ($LASTEXITCODE -ne 0) { throw "d8 failed" }
Get-ChildItem "$root\out\dex" | Select-Object Name, Length

Write-Host "=== 3/6 aapt2 link ==="
& "$bt\aapt2.exe" link -o "$root\out\unsigned.apk" -I $plat --manifest "$root\AndroidManifest.xml" --min-sdk-version 30 --target-sdk-version 30
if ($LASTEXITCODE -ne 0) { throw "aapt2 link failed" }

Write-Host "=== 4/6 stage dex + lib + assets ==="
$so = Join-Path $deps "wexguard_v8.so"
& $python -c @"
import zipfile, shutil
apk = r'$root\out\unsigned.apk'
tmp = r'$root\out\staged.apk'
zin = zipfile.ZipFile(apk, 'r')
zout = zipfile.ZipFile(tmp, 'w', zipfile.ZIP_DEFLATED)
for item in zin.infolist():
    zout.writestr(item, zin.read(item.filename))
zin.close()
# main dex
zout.writestr('classes.dex', open(r'$root\out\dex\classes.dex','rb').read())
# libwexguard_v8.so into both lib/arm64-v8a and assets
src_so = r'$so'
zout.writestr('lib/arm64-v8a/libwexguard_v8.so', open(src_so,'rb').read())
zout.writestr('assets/wexguard_v8.so', open(src_so,'rb').read())
# wexshinidie.guard asset (从订阅 spider jar 提取)
import zipfile as zf2
with zf2.ZipFile(r'$SpiderJar') as zj:
    if 'assets/wexshinidie.guard' in zj.namelist():
        zout.writestr('assets/wexshinidie.guard', zj.read('assets/wexshinidie.guard'))
        print('added wexshinidie.guard')
zout.close()
shutil.move(tmp, apk)
print('staged')
"@
if ($LASTEXITCODE -ne 0) { throw "staging failed" }

Write-Host "=== 5/6 zipalign ==="
& "$bt\zipalign.exe" -f 4 "$root\out\unsigned.apk" "$root\out\aligned.apk"
if ($LASTEXITCODE -ne 0) { throw "zipalign failed" }

Write-Host "=== 6/6 sign ==="
$ks = Join-Path $deps "debug.keystore"
if (-not (Test-Path $ks)) {
    # 首次构建生成 debug keystore (密码固定 android)
    & keytool -genkeypair -keystore $ks -storepass android -keypass android -alias androiddebugkey -dname "CN=Android Debug,O=Android,C=US" -keyalg RSA -keysize 2048 -validity 10000
    if ($LASTEXITCODE -ne 0) { throw "keytool failed" }
}
java -Xmx1024M -jar "$bt\lib\apksigner.jar" sign --ks $ks --ks-pass pass:android --key-pass pass:android --out "$root\out\bridge.apk" "$root\out\aligned.apk"
if ($LASTEXITCODE -ne 0) { throw "apksigner failed" }

java -jar "$bt\lib\apksigner.jar" verify "$root\out\bridge.apk"

# 输出到仓库稳定路径，供 core bridge ensure_apk_installed 默认使用
New-Item -ItemType Directory -Path "$PSScriptRoot\out" -Force | Out-Null
Copy-Item "$root\out\bridge.apk" "$PSScriptRoot\out\bridge.apk" -Force
Write-Host "BUILD OK: $PSScriptRoot\out\bridge.apk"
