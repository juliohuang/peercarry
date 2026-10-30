param([switch]$NoStart)
$ErrorActionPreference = 'Stop'
$destination = Join-Path $env:LOCALAPPDATA 'peercarry'
$source = $PSScriptRoot
$names = @('peercarry-tray.exe', 'peercarry.exe')
foreach ($name in $names) {
    if (-not (Test-Path -LiteralPath (Join-Path $source $name) -PathType Leaf)) { throw "Missing $name" }
}
New-Item -ItemType Directory -Force -Path $destination | Out-Null
if ((Get-Item -LiteralPath $destination).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Installation directory must not be a link' }
$tray = Join-Path $destination 'peercarry-tray.exe'
$processes = @(Get-Process peercarry-tray -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $tray })
# Stop only this installed tray; preserve the data directory and other programs.
foreach ($process in $processes) { Stop-Process -Id $process.Id; Wait-Process -Id $process.Id -ErrorAction SilentlyContinue }
$copied = @()
try {
    foreach ($name in $names) {
        $target = Join-Path $destination $name
        if (Test-Path -LiteralPath $target) {
            if ((Get-Item -LiteralPath $target).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Installed executable must not be a link' }
            Copy-Item -LiteralPath $target -Destination "$target.bak" -Force
        }
        Copy-Item -LiteralPath (Join-Path $source $name) -Destination "$target.new" -Force
        Move-Item -LiteralPath "$target.new" -Destination $target -Force
        $copied += $target
    }
    $run = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
    if (-not (Test-Path -LiteralPath $run)) { New-Item -Path $run -Force | Out-Null }
    New-ItemProperty -Path $run -Name 'peercarry' -Value ('"' + $tray + '"') -PropertyType String -Force | Out-Null
} catch {
    foreach ($target in $copied) {
        if (Test-Path -LiteralPath "$target.bak") { Copy-Item -LiteralPath "$target.bak" -Destination $target -Force }
    }
    foreach ($previousProcess in $processes) {
        if (Test-Path -LiteralPath $previousProcess.Path) { Start-Process -FilePath $previousProcess.Path -WindowStyle Hidden }
    }
    throw
}
if (-not $NoStart) { Start-Process -FilePath $tray -WindowStyle Hidden }
Write-Host "Installed to $destination. Existing data is not migrated."
