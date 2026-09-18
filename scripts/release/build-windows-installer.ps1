param(
    [Parameter(Mandatory=$true)][string]$Version,
    [Parameter(Mandatory=$true)][string]$BinaryDir,
    [string]$OutputDir = 'dist'
)
$ErrorActionPreference = 'Stop'
$Version = $Version.TrimStart('v')
if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw 'Expected a stable semantic version' }
$BinaryDir = (Resolve-Path -LiteralPath $BinaryDir).Path
foreach ($name in @('peercarry-tray.exe', 'peercarry.exe')) {
    if (-not (Test-Path -LiteralPath (Join-Path $BinaryDir $name) -PathType Leaf)) { throw "Missing $name" }
}
New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
$OutputDir = (Resolve-Path -LiteralPath $OutputDir).Path
$compiler = Get-Command ISCC.exe -ErrorAction SilentlyContinue
if ($compiler) { $iscc = $compiler.Source }
else { $iscc = Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe' }
if (-not (Test-Path -LiteralPath $iscc)) { throw 'Install Inno Setup 6 first' }
& $iscc "/DAppVersion=$Version" "/DBinaryDir=$BinaryDir" "/DOutputDir=$OutputDir" (Join-Path $PSScriptRoot 'windows-installer.iss')
if ($LASTEXITCODE -ne 0) { throw 'Installer compilation failed' }
$installer = Join-Path $OutputDir "PeerCarry-$Version-windows-x64-Setup.exe"
$hash = (Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash.ToLowerInvariant()
[IO.File]::WriteAllText("$installer.sha256", "$hash  $([IO.Path]::GetFileName($installer))`n", [Text.Encoding]::ASCII)
