param(
    [Parameter(Mandatory)][string]$Setup,
    [Parameter(Mandatory)][string]$Stub
)
$ErrorActionPreference = "Stop"
. "$PSScriptRoot/windows-process.ps1"
$registry = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\HIVECAST"
if (Test-Path $registry) { throw "Upgrade test requires a user without an existing HIVECAST installation" }

function Assert-InstallerRejects([string]$Program, [string[]]$Arguments) {
    try { Invoke-Installer $Program $Arguments } catch {
        if ($_.Exception.Message -match "exited with code 1$") { return }
        throw
    }
    throw "Expected installer rejection from $Program"
}

$work = New-Item -ItemType Directory -Path (Join-Path $env:RUNNER_TEMP ("upgrade-" + [guid]::NewGuid()))
$originalAppData = $env:APPDATA
try {
    $env:APPDATA = Join-Path $work "Roaming"
    $foreign = New-Item -ItemType Directory -Path (Join-Path $work "other app")
    $otherUninstaller = Join-Path $foreign "uninstall.exe"
    Copy-Item -LiteralPath $Stub -Destination $otherUninstaller
    $stubHash = (Get-FileHash $Stub).Hash
    Assert-InstallerRejects $Setup @("--silent", "--dir", $foreign.FullName)
    if ((Get-FileHash $otherUninstaller).Hash -ne $stubHash) { throw "Unrelated uninstaller changed" }

    $dir = Join-Path $work "旧 安装"
    $seed = Join-Path $work "old setup.exe"
    $oldVersion = "0.0.0-fixture.1"
    Invoke-Installer $Stub @("--pack", $Stub, $Stub, $oldVersion, $seed)
    Invoke-Installer $seed @("--silent", "--dir", $dir, "--no-shortcut")
    # Legacy REG_SZ can combine Explorer direction marks, NSIS quotes and bytes after NUL.
    $legacyLocation = [string][char]0x202A + '"' + $dir + '"' + [char]0x202C + [char]0 + "trailing"
    Set-ItemProperty $registry InstallLocation $legacyLocation
    $app = Join-Path $dir "HIVECAST.exe"
    $uninstall = Join-Path $dir "uninstall.exe"
    $shortcut = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\HIVECAST.lnk"
    foreach ($path in @($uninstall, $shortcut)) {
        $lock = [IO.File]::Open($path, 'Open', 'Read', 'Read')
        try { Assert-InstallerRejects $Setup @("--silent", "--no-shortcut") } finally { $lock.Dispose() }
        if ((Get-FileHash $app).Hash -ne $stubHash -or (Get-FileHash $uninstall).Hash -ne $stubHash -or
            (Get-ItemProperty $registry).DisplayVersion -ne $oldVersion) { throw "Incomplete upgrade rollback" }
    }
    Invoke-Installer $Setup @("--silent", "--no-shortcut")
    $entry = Get-ItemProperty $registry
    if ($entry.InstallLocation -ne $dir -or $entry.DisplayVersion -ne $env:VERSION) { throw "Lost original install location or version" }
    Assert-InstallerRejects $otherUninstaller @("--uninstall", "--silent")
    Assert-InstallerRejects $otherUninstaller @("--uninstall", "--silent", "--dir", $dir)
    $keep = Join-Path $dir "keep.txt"
    Set-Content -LiteralPath $keep -Value "user file" -NoNewline
    Invoke-Installer $uninstall @("--uninstall", "--silent")
    $deadline = [DateTime]::UtcNow.AddSeconds(30)
    while ((Test-Path $uninstall) -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 200 }
    if ((Test-Path $registry) -or (Test-Path $shortcut) -or @(Get-ChildItem -Force $dir).Count -ne 1 -or
        (Get-Content -LiteralPath $keep -Raw) -ne "user file") { throw "Uninstall did not preserve only the user file" }

    $emptyDir = Join-Path $work "空 目录"
    Invoke-Installer $Setup @("--silent", "--dir", $emptyDir, "--no-shortcut")
    New-Item -ItemType Directory -Force -Path (Join-Path $emptyDir "cache"), (Join-Path $emptyDir "webview"), (Join-Path $emptyDir "logs") | Out-Null
    Set-Content -LiteralPath (Join-Path $emptyDir "logs\app.log") -Value "log" -NoNewline
    $emptyUninst = Join-Path $emptyDir "uninstall.exe"
    # Verify running uninstall.exe directly without --uninstall defaults to uninstall mode.
    Invoke-Installer $emptyUninst @("--silent")
    $deadline = [DateTime]::UtcNow.AddSeconds(30)
    while ((Test-Path -LiteralPath $emptyDir) -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 200 }
    if ((Test-Path -LiteralPath $emptyDir) -or (Test-Path $registry) -or (Test-Path $shortcut)) {
        throw "Empty install directory (with ephemeral cache/webview/logs) was not removed after uninstall"
    }

    # Verify default uninstall preserves data/ + Recording/, reinstall reuses that directory, and --purge-data clears data/ while keeping Recording/.
    $reuseDir = Join-Path $work "复用 目录"
    Invoke-Installer $Setup @("--silent", "--dir", $reuseDir, "--no-shortcut")
    New-Item -ItemType Directory -Force -Path (Join-Path $reuseDir "data"), (Join-Path $reuseDir "Recording"), (Join-Path $reuseDir "cache") | Out-Null
    Set-Content -LiteralPath (Join-Path $reuseDir "data\hivecast.db") -Value "db" -NoNewline
    Set-Content -LiteralPath (Join-Path $reuseDir "Recording\clip.mp4") -Value "vid" -NoNewline
    $reuseUninst = Join-Path $reuseDir "uninstall.exe"
    Invoke-Installer $reuseUninst @("--uninstall", "--silent")
    $deadline = [DateTime]::UtcNow.AddSeconds(30)
    while ((Test-Path -LiteralPath $reuseUninst) -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 200 }
    if ((Test-Path -LiteralPath (Join-Path $reuseDir "cache")) -or -not (Test-Path -LiteralPath (Join-Path $reuseDir "data\hivecast.db")) -or -not (Test-Path -LiteralPath (Join-Path $reuseDir "Recording\clip.mp4"))) {
        throw "Default uninstall did not clean cache/ while preserving data/ and Recording/"
    }
    Invoke-Installer $Setup @("--silent", "--dir", $reuseDir, "--no-shortcut")
    Invoke-Installer $reuseUninst @("--uninstall", "--silent", "--purge-data")
    $deadline = [DateTime]::UtcNow.AddSeconds(30)
    while ((Test-Path -LiteralPath $reuseUninst) -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 200 }
    if ((Test-Path -LiteralPath (Join-Path $reuseDir "data")) -or -not (Test-Path -LiteralPath (Join-Path $reuseDir "Recording\clip.mp4"))) {
        throw "--purge-data uninstall did not remove data/ while keeping Recording/"
    }

    # Verify registry-lost / moved HIVECAST directory can be upgraded/repaired in place without losing data/ or Recording/.
    $movedDir = Join-Path $work "迁移 目录"
    New-Item -ItemType Directory -Force -Path (Join-Path $movedDir "data"), (Join-Path $movedDir "Recording") | Out-Null
    Copy-Item -LiteralPath $Stub -Destination (Join-Path $movedDir "HIVECAST.exe")
    Set-Content -LiteralPath (Join-Path $movedDir "data\hivecast.db") -Value "moved-db" -NoNewline
    Set-Content -LiteralPath (Join-Path $movedDir "Recording\clip.mp4") -Value "moved-vid" -NoNewline
    Invoke-Installer $Setup @("--silent", "--dir", $movedDir, "--no-shortcut")
    if ((Get-ItemProperty $registry).InstallLocation -ne $movedDir -or
        (Get-Content -LiteralPath (Join-Path $movedDir "data\hivecast.db") -Raw) -ne "moved-db" -or
        (Get-Content -LiteralPath (Join-Path $movedDir "Recording\clip.mp4") -Raw) -ne "moved-vid") {
        throw "Registry-lost in-place repair did not preserve data/Recording or re-register InstallLocation"
    }
    $movedUninst = Join-Path $movedDir "uninstall.exe"
    Invoke-Installer $movedUninst @("--silent", "--purge-data")
    $deadline = [DateTime]::UtcNow.AddSeconds(30)
    while ((Test-Path -LiteralPath $movedUninst) -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 200 }
    if ((Test-Path $registry) -or -not (Test-Path -LiteralPath (Join-Path $movedDir "Recording\clip.mp4"))) {
        throw "Cleanup after registry-lost repair test failed"
    }
    Write-Host "upgrade smoke ok (ownership, rollback, quoted path, retry, preserve data/recording, reinstall, purge-data, moved-dir repair and empty-dir cleanup)"
} finally {
    $env:APPDATA = $originalAppData
}
