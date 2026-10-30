param([Parameter(Mandatory=$true)][string]$Installer)
$ErrorActionPreference = 'Stop'
$Installer = (Resolve-Path -LiteralPath $Installer).Path
$root = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$destination = Join-Path $root ('target/installer-smoke-' + [Guid]::NewGuid().ToString('N'))
$uninstallKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\PeerCarry.Desktop_is1'
if (Test-Path $uninstallKey) { throw 'An installer-managed copy already exists; test in a clean Windows user account instead.' }
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
function Read-OtherStartupEntries {
    $value = Get-ItemProperty -LiteralPath $runKey -ErrorAction SilentlyContinue
    $entries = [ordered]@{}
    if ($value) {
        foreach ($property in ($value.PSObject.Properties | Sort-Object Name)) {
            if (-not $property.Name.StartsWith('PS') -and $property.Name -ne 'peercarry') {
                $entries[$property.Name] = $property.Value
            }
        }
    }
    return ConvertTo-Json -InputObject $entries -Compress
}
$runValue = Get-ItemProperty -LiteralPath $runKey -Name peercarry -ErrorAction SilentlyContinue
if ($runValue) { throw 'A PeerCarry startup entry already exists; test in a clean Windows user account instead.' }
$beforeOtherStartup = Read-OtherStartupEntries
function Read-PeerCarryStartup {
    $value = Get-ItemProperty -LiteralPath $runKey -Name peercarry -ErrorAction SilentlyContinue
    return $value.peercarry
}
function Run-Installer([string]$Type, [bool]$EnableStartup = $false) {
    $tasks = if ($EnableStartup) { '/TASKS=startup' } else { '/TASKS=""' }
    $process = Start-Process -FilePath $Installer -ArgumentList @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/NOICONS', $tasks, "/TYPE=$Type", "/DIR=`"$destination`"", "/LOG=`"$destination-$Type.log`"") -WindowStyle Hidden -Wait -PassThru
    if ($process.ExitCode -ne 0) { throw "Installer returned $($process.ExitCode)" }
}
try {
    Run-Installer 'compact'
    if (-not (Test-Path "$destination/peercarry-tray.exe")) { throw 'Tray missing' }
    if (Test-Path "$destination/peercarry.exe") { throw 'Compact install included CLI' }
    'PASS: compact install contains desktop only'
    [IO.File]::WriteAllText("$destination/user-sentinel.txt", 'preserve user data')
    Run-Installer 'full' $true
    if (-not (Test-Path "$destination/peercarry.exe")) { throw 'Full install missing CLI' }
    $version = & "$destination/peercarry.exe" --version
    if ($LASTEXITCODE -ne 0) { throw 'Installed CLI failed' }
    if (-not $version) { throw 'Installed CLI --version returned no output' }
    if ((Read-PeerCarryStartup) -ne ('"' + (Join-Path $destination 'peercarry-tray.exe') + '"')) { throw 'PeerCarry startup registration is incorrect' }
    'PASS: full install, current CLI --version option and PeerCarry startup registration'
} finally {
    if (Test-Path "$destination/unins000.exe") {
        $process = Start-Process -FilePath "$destination/unins000.exe" -ArgumentList '/VERYSILENT /SUPPRESSMSGBOXES /NORESTART' -WindowStyle Hidden -Wait -PassThru
        if ($process.ExitCode -ne 0) { throw 'Uninstall failed' }
    }
}
if (Test-Path "$destination/peercarry-tray.exe") { throw 'Uninstall left the installed tray' }
if (Test-Path $uninstallKey) { throw 'Uninstall registration remains' }
if (Read-PeerCarryStartup) { throw 'Uninstall left the PeerCarry startup registration' }
if (-not (Test-Path "$destination/user-sentinel.txt")) { throw 'Uninstall removed user data' }
if ((Read-OtherStartupEntries) -ne $beforeOtherStartup) { throw 'Unrelated startup entries changed' }
'PASS: uninstall, user data preservation and existing startup entries unchanged'
"Evidence: $destination"
