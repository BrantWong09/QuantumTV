$ErrorActionPreference = 'Stop'
$root = "C:\Users\hdec\AppData\Local\Temp\opencode\spider-bridge"
$sdk = "$env:LOCALAPPDATA\Android\Sdk"
$bt = "$sdk\build-tools\30.0.3"
$plat = "$sdk\platforms\android-30\android.jar"

Write-Host "=== 1/6 javac ==="
$srcs = Get-ChildItem "$root\src" -Recurse -Filter *.java | ForEach-Object { $_.FullName }
& javac --release 8 -encoding UTF-8 -classpath $plat -d "$root\out\classes" $srcs
if ($LASTEXITCODE -ne 0) { throw "javac failed" }

Write-Host "=== 2/6 d8 (merge) ==="
New-Item -ItemType Directory -Path "$root\out\dex" -Force | Out-Null
$classes = Get-ChildItem "$root\out\classes" -Recurse -Filter *.class | ForEach-Object { $_.FullName }
$d8Inputs = @($classes)
$libsDir = $root
foreach ($f in @("spider_classes.dex", "gson-2.10.1.jar", "okhttp-4.12.0.jar", "okio-jvm-3.6.0.jar", "kotlin-stdlib-1.9.22.jar", "zxing-core-3.5.2.jar")) {
    $p = Join-Path $libsDir $f
    if (Test-Path $p) {
        if ($p -like "*.dex") {
            # d8 accepts .dex input directly
            $d8Inputs += $p
        } else {
            $d8Inputs += $p
        }
    } elseif (Test-Path (Join-Path "C:\Users\hdec\AppData\Local\Temp\opencode\bridgeprobe" $f)) {
        $d8Inputs += (Join-Path "C:\Users\hdec\AppData\Local\Temp\opencode\bridgeprobe" $f)
    }
}
java -Xmx2048M -cp "$bt\lib\d8.jar" com.android.tools.r8.D8 --release --lib $plat --min-api 30 --output "$root\out\dex" @d8Inputs
if ($LASTEXITCODE -ne 0) { throw "d8 failed" }
Get-ChildItem "$root\out\dex" | Select-Object Name, Length

Write-Host "=== 3/6 aapt2 link ==="
& "$bt\aapt2.exe" link -o "$root\out\unsigned.apk" -I $plat --manifest "$root\AndroidManifest.xml" --min-sdk-version 30 --target-sdk-version 30
if ($LASTEXITCODE -ne 0) { throw "aapt2 link failed" }

Write-Host "=== 4/6 stage dex + lib + assets ==="
& "C:\Users\hdec\miniconda3\python.exe" -c @"
import zipfile, shutil, glob, os
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
src_so = r'C:\Users\hdec\AppData\Local\Temp\opencode\wexlib\wexguard_v8.so'
zout.writestr('lib/arm64-v8a/libwexguard_v8.so', open(src_so,'rb').read())
zout.writestr('assets/wexguard_v8.so', open(src_so,'rb').read())
# wexshinidie.guard asset
import zipfile as zf2
with zf2.ZipFile(r'D:\QuantumTV\.cache\.cache\sites\4996328b1d39e8bce7f241119238f97c\spider.jar') as zj:
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
$ks = "C:\Users\hdec\AppData\Local\Temp\opencode\debug.keystore"
java -Xmx1024M -jar "$bt\lib\apksigner.jar" sign --ks $ks --ks-pass pass:android --key-pass pass:android --out "$root\out\bridge.apk" "$root\out\aligned.apk"
if ($LASTEXITCODE -ne 0) { throw "apksigner failed" }

java -jar "$bt\lib\apksigner.jar" verify "$root\out\bridge.apk"

# 输出到仓库稳定路径，供 core bridge ensure_apk_installed 默认使用
New-Item -ItemType Directory -Path "$PSScriptRoot\out" -Force | Out-Null
Copy-Item "$root\out\bridge.apk" "$PSScriptRoot\out\bridge.apk" -Force
Write-Host "BUILD OK: $PSScriptRoot\out\bridge.apk"
