# 把当前仓库发布到 GitHub（Windows PowerShell 5.1 / pwsh 7 均可用）
#
# 用法：
#   # 1) 用细粒度令牌创建私有仓库并推送（令牌只用于本次调用，不写入任何配置）
#   $env:GITHUB_TOKEN = '<fine-grained PAT：Administration 读写 + Contents 读写>'
#   $env:GITHUB_USER  = '<你的 GitHub 用户名>'
#   .\tools\publish-github.ps1 -Repo chat
#
#   # 2) 仓库已经存在，只推送
#   $env:GITHUB_TOKEN = '<PAT>'
#   .\tools\publish-github.ps1 -Repo chat -Owner my-name -SkipCreate
#
#   # 3) 走 SSH（先把 tools/publish-github.ps1 同目录输出的公钥加到 GitHub 账号）
#   .\tools\publish-github.ps1 -Repo chat -Owner my-name -SkipCreate -Method ssh
#
# 安全说明：
# - 令牌只通过 http.extraheader 传给 git，不写进 .git/config、不打印、不落盘；
# - 推送完成后本地 remote 只保留不含令牌的干净 https 地址；
# - 任何失败都以非零退出码结束并说明原因。

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Repo,

    [string]$Owner = '',

    [ValidateSet('https', 'ssh')]
    [string]$Method = 'https',

    [switch]$Public,

    [switch]$SkipCreate,

    [string]$Description = '端到端加密聊天应用：Rust 内核（OpenMLS + iroh）与 Kotlin/Compose 三端界面'
)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Push-Location $root
try {
    if (-not (Test-Path '.git')) { throw '当前目录不是 git 仓库，请先在项目根目录执行 git init。' }
    $branch = (git rev-parse --abbrev-ref HEAD).Trim()
    if ($branch -ne 'main') { git branch -M main; $branch = 'main' }

    if (-not $Owner) {
        if (-not $env:GITHUB_USER) { throw '缺少 -Owner 或环境变量 GITHUB_USER（GitHub 用户名）。' }
        $Owner = $env:GITHUB_USER
    }

    $token = $env:GITHUB_TOKEN
    if ($Method -eq 'https' -and -not $token) {
        throw '缺少环境变量 GITHUB_TOKEN（fine-grained PAT：Administration 读写 + Contents 读写）。'
    }

    if (-not $SkipCreate) {
        if ($Method -eq 'ssh') { throw '创建仓库需要令牌：请先在网页新建空仓库，再用 -SkipCreate -Method ssh 推送。' }
        $visibility = if ($Public) { $false } else { $true }
        $body = @{
            name        = $Repo
            description = $Description
            private     = $visibility
            has_issues  = $true
            has_wiki    = $false
        } | ConvertTo-Json
        $headers = @{
            Authorization = "Bearer $token"
            'User-Agent'  = 'chat-publish-script'
            Accept        = 'application/vnd.github+json'
        }

        Write-Host "[1/3] 检查/创建仓库 $Owner/$Repo（private=$visibility）..."
        $existing = $null
        try {
            $existing = Invoke-RestMethod -Method Get -Uri "https://api.github.com/repos/$Owner/$Repo" -Headers $headers -ErrorAction Stop
        } catch {
            $existing = $null
        }
        if ($existing) {
            Write-Host "      仓库已存在，直接使用：$($existing.html_url)"
        } else {
            $created = Invoke-RestMethod -Method Post -Uri 'https://api.github.com/user/repos' -Headers $headers -Body $body -ContentType 'application/json'
            Write-Host "      已创建：$($created.html_url)"
        }
    } else {
        Write-Host '[1/3] 跳过创建，使用已存在的远端仓库。'
    }

    Write-Host '[2/3] 配置远端...'
    if ($Method -eq 'ssh') {
        $remoteUrl = "git@github.com:$Owner/$Repo.git"
    } else {
        $remoteUrl = "https://github.com/$Owner/$Repo.git"
    }
    $hasOrigin = (git remote) -contains 'origin'
    if ($hasOrigin) { git remote set-url origin $remoteUrl } else { git remote add origin $remoteUrl }
    Write-Host "      origin -> $remoteUrl"

    Write-Host '[3/3] 推送 main...'
    if ($Method -eq 'ssh') {
        git push -u origin main
    } else {
        $pair = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes("x-access-token:$token"))
        git -c http.extraheader="AUTHORIZATION: basic $pair" push -u origin main
    }
    if ($LASTEXITCODE -ne 0) { throw "推送失败（exit=$LASTEXITCODE）。" }

    Write-Host ''
    Write-Host "完成：https://github.com/$Owner/$Repo"
    Write-Host '后续推送：git push（凭据交给系统凭据管理器；不要再把令牌写进 remote）。'
} finally {
    Pop-Location
}
