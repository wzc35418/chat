# Prepare a WINDOWS-server deployment bundle for chat-node (run this on the dev machine).
#
# Produces build\node-bundle\ containing:
#   chat-node.exe        release build of the node
#   cert.pem / key.pem   self-signed TLS certificate with SAN = <public ip>
#   seed.hex             node signing seed (KEEP SECRET)
#   run-node.ps1         start script for the server (console host)
#   register-service.ps1 optional: register as a boot-time scheduled task
#   README-SERVER.txt    what to do on the server
#   fingerprint.txt      endpoint + node id + cert SHA-256 (send this back)
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File deploy\prepare-node-bundle.ps1 -PublicIp 106.54.223.171 [-Port 8443]
param(
    [Parameter(Mandatory = $true)][string]$PublicIp,
    [int]$Port = 8443,
    [string]$NodeId = 'node-public',
    [switch]$SkipBuild
)
# 这个脚本主要编排外部命令（cargo/openssl），它们把进度写到 stderr；
# 用 Stop 会把进度当成致命错误，所以这里用 Continue，真正的失败靠退出码判断。
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$bundle = Join-Path $root 'build\node-bundle'
if (Test-Path $bundle) { Remove-Item -Recurse -Force $bundle }
New-Item -ItemType Directory -Path $bundle -Force | Out-Null

Write-Host ('== prepare node bundle for ' + $PublicIp + ':' + $Port + ' ==')

# 1. release build
$exe = Join-Path $root 'target\release\chat-node.exe'
if (-not (Test-Path $exe)) {
    if ($SkipBuild) { Write-Host ('FAIL: ' + $exe + ' missing and -SkipBuild given'); exit 1 }
    Write-Host '== cargo build --release -p chat-node =='
    Push-Location $root
    try {
        # cargo writes progress to stderr; with ErrorActionPreference=Stop that is fatal, so go through cmd /c.
        cmd /c "cargo build --release -p chat-node 2>&1" | Write-Host
        $buildCode = $LASTEXITCODE
    } finally { Pop-Location }
    if ($buildCode -ne 0) { Write-Host ('FAIL: release build failed (exit ' + $buildCode + ')'); exit 1 }
}
Copy-Item $exe (Join-Path $bundle 'chat-node.exe') -Force
Write-Host ('PASS bundle: chat-node.exe (' + [math]::Round((Get-Item $exe).Length / 1MB, 1) + ' MB)')

# 2. certificate (SAN must contain the public IP, otherwise clients reject it)
$openssl = 'C:\Program Files\OpenSSL-Win64\bin\openssl.exe'
if (-not (Test-Path $openssl)) { Write-Host 'FAIL: openssl not found (install Win64 OpenSSL or generate the cert manually)'; exit 1 }
$cnf = Join-Path $bundle 'openssl.cnf'
@"
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $PublicIp
[ext]
subjectAltName = IP:$PublicIp,DNS:localhost
basicConstraints = critical,CA:TRUE
keyUsage = critical,digitalSignature,keyCertSign
extendedKeyUsage = serverAuth
"@ | Set-Content -Path $cnf -Encoding ascii
& $openssl req -x509 -newkey rsa:2048 -nodes -days 3650 `
    -keyout (Join-Path $bundle 'key.pem') -out (Join-Path $bundle 'cert.pem') `
    -config $cnf -extensions ext 2>&1 | Out-Null
if ($LASTEXITCODE -ne 0) { Write-Host 'FAIL: certificate generation failed'; exit 1 }
$fpRaw = (& $openssl x509 -in (Join-Path $bundle 'cert.pem') -noout -fingerprint -sha256 | Out-String)
$fp = ([regex]::Match($fpRaw, 'Fingerprint=([0-9A-Fa-f:]+)').Groups[1].Value -replace ':', '').ToLowerInvariant()
if ($fp -notmatch '^[0-9a-f]{64}$') { Write-Host 'FAIL: could not read the certificate fingerprint'; exit 1 }
Write-Host ('PASS bundle: cert.pem / key.pem (SAN=' + $PublicIp + ')')

# 3. seed
$seedBytes = New-Object byte[] 32
[System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($seedBytes)
([System.BitConverter]::ToString($seedBytes) -replace '-', '').ToLowerInvariant() | Set-Content -Path (Join-Path $bundle 'seed.hex') -NoNewline -Encoding ascii
Write-Host 'PASS bundle: seed.hex'

# 4. run script for the server
# 注意：这里必须用**单引号** here-string。双引号 here-string 会把行尾的反引号当转义吃掉，
# 导致生成的 run-node.ps1 里续行消失，服务器上报「一元运算符 -- 后面缺少表达式」。
$run = @'
# Start chat-node on this Windows server (console host).
# Keep the window open; for boot persistence run register-service.ps1 once as admin.
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
Write-Host 'chat-node starting...'
& (Join-Path $here 'chat-node.exe') `
  --bind 0.0.0.0:PORT_PLACEHOLDER `
  --allow-public-bind `
  --data-dir (Join-Path $here 'data') `
  --seed-file (Join-Path $here 'seed.hex') `
  --node-id NODEID_PLACEHOLDER `
  --tls-cert (Join-Path $here 'cert.pem') `
  --tls-key (Join-Path $here 'key.pem')
'@
# 端口/节点 ID 由脚本参数决定：生成后再替换占位符（单引号 here-string 内不插值）。
$run = $run.Replace('PORT_PLACEHOLDER', $Port.ToString()).Replace('NODEID_PLACEHOLDER', $NodeId)
Set-Content -Path (Join-Path $bundle 'run-node.ps1') -Value $run -Encoding utf8

# 4b. 服务器侧自检脚本（拷进 bundle，服务器上先跑它再启动）
$verifier = Join-Path $PSScriptRoot 'verify-bundle.ps1'
if (Test-Path $verifier) { Copy-Item $verifier (Join-Path $bundle 'verify-bundle.ps1') -Force }

$svc = @'
# Register a boot-time scheduled task so the node survives reboots (run as admin on the server).
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$ps = (Get-Command powershell.exe).Source
$action = New-ScheduledTaskAction -Execute $ps -Argument ('-NoProfile -ExecutionPolicy Bypass -File "' + (Join-Path $here 'run-node.ps1') + '"')
$trigger = New-ScheduledTaskTrigger -AtStartup
$principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
Register-ScheduledTask -TaskName 'chat-node' -Action $action -Trigger $trigger -Principal $principal -Force | Out-Null
Write-Host 'registered scheduled task: chat-node (starts at boot)'
'@
Set-Content -Path (Join-Path $bundle 'register-service.ps1') -Value $svc -Encoding utf8

# 5. server README + fingerprint file
$readme = @"
=== chat-node deployment (Windows Server) ===

1) Copy this whole folder to the server, e.g. C:\chat-node
2) Allow inbound TCP $Port (Windows Firewall + cloud security group):
     New-NetFirewallRule -DisplayName 'chat-node' -Direction Inbound -Protocol TCP -LocalPort $Port -Action Allow
3) Start it (keep the window open):
     powershell -ExecutionPolicy Bypass -File .\run-node.ps1
   or register it to start at boot (admin):
     powershell -ExecutionPolicy Bypass -File .\register-service.ps1
4) Self-check from the server:
     curl.exe --cacert cert.pem https://${PublicIp}:$Port/v1/health
     curl.exe --cacert cert.pem https://${PublicIp}:$Port/v1/tls/info
5) Send these three lines back to the client side:
     endpoint    : https://${PublicIp}:$Port
     node_id     : $NodeId
     fingerprint : $fp

KEEP SECRET: seed.hex (node identity) and key.pem (TLS private key).
"@
Set-Content -Path (Join-Path $bundle 'README-SERVER.txt') -Value $readme -Encoding utf8
@"
endpoint    : https://$PublicIp`:$Port
node_id     : $NodeId
fingerprint : $fp
"@ | Set-Content -Path (Join-Path $bundle 'fingerprint.txt') -Encoding utf8

Write-Host ''
Write-Host '================ ready ================'
Write-Host ('bundle      : ' + $bundle)
Write-Host ('endpoint    : https://' + $PublicIp + ':' + $Port)
Write-Host ('node_id     : ' + $NodeId)
Write-Host ('fingerprint : ' + $fp)
Write-Host '======================================='
Write-Host 'Next: copy the folder to the server, run run-node.ps1 (or register-service.ps1), then send the three lines back.'
