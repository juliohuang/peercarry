param([Parameter(Mandatory=$true)][string]$Installer)
$ErrorActionPreference = 'Stop'
$Installer = (Resolve-Path -LiteralPath $Installer).Path
$root = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$destination = Join-Path $root ('target/installer-smoke-' + [Guid]::NewGuid().ToString('N'))
$uninstallKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\PeerCarry.Desktop_is1'
if (Test-Path $uninstallKey) { throw 'An installer-managed copy already exists; test in a clean Windows user account instead.' }
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
function Read-Startup {
    $value = Get-ItemProperty -LiteralPath $runKey -ErrorAction SilentlyContinue
    return @($value.peercarry, $value.'sync-clip') | ConvertTo-Json -Compress
}
$before = Read-Startup
function Run-Installer([string]$Type) {
    $process = Start-Process -FilePath $Installer -ArgumentList @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/NOICONS', '/TASKS=""', "/TYPE=$Type", "/DIR=`"$destination`"", "/LOG=`"$destination-$Type.log`"") -WindowStyle Hidden -Wait -PassThru
    if ($process.ExitCode -ne 0) { throw "Installer returned $($process.ExitCode)" }
}
try {
    Run-Installer 'compact'
    if (-not (Test-Path "$destination/peercarry-tray.exe")) { throw 'Tray missing' }
    if (Test-Path "$destination/peercarry.exe") { throw 'Compact install included CLI' }
    if (Test-Path "$destination/sclip.exe") { throw 'Compact install included compatibility CLI' }
    'PASS: compact install contains desktop only'
    [IO.File]::WriteAllText("$destination/user-sentinel.txt", 'preserve user data')
    Run-Installer 'full'
    if (-not (Test-Path "$destination/peercarry.exe")) { throw 'Full install missing CLI' }
    if (-not (Test-Path "$destination/sclip.exe")) { throw 'Full install missing compatibility CLI' }
    & "$destination/peercarry.exe" --version
    if ($LASTEXITCODE -ne 0) { throw 'Installed CLI failed' }
    'PASS: full install and CLI smoke'
} finally {
    if (Test-Path "$destination/unins000.exe") {
        $process = Start-Process -FilePath "$destination/unins000.exe" -ArgumentList '/VERYSILENT /SUPPRESSMSGBOXES /NORESTART' -WindowStyle Hidden -Wait -PassThru
        if ($process.ExitCode -ne 0) { throw 'Uninstall failed' }
    }
}
if (Test-Path "$destination/peercarry-tray.exe") { throw 'Uninstall left the installed tray' }
if (Test-Path $uninstallKey) { throw 'Uninstall registration remains' }
if (-not (Test-Path "$destination/user-sentinel.txt")) { throw 'Uninstall removed user data' }
if ((Read-Startup) -ne $before) { throw 'Unrelated startup entries changed' }
'PASS: uninstall, user data preservation and existing startup entries unchanged'
"Evidence: $destination"
