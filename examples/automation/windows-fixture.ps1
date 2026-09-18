param([switch]$RunSmoke)
# Fixture for native UIA tests. RunSmoke starts the fixed flow from this foreground app.
# Run from an interactive Windows PowerShell session, then close the window when done.
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

$form = New-Object System.Windows.Forms.Form
$form.Text = 'peercarry automation fixture'
$form.ClientSize = New-Object System.Drawing.Size(560, 180)
$form.StartPosition = 'CenterScreen'
$form.FormBorderStyle = [System.Windows.Forms.FormBorderStyle]::FixedDialog
$form.MaximizeBox = $false
$form.MinimizeBox = $false
$form.TopMost = $true

$label = New-Object System.Windows.Forms.Label
$label.Text = 'Automation fixture (no submit action)'
$label.AutoSize = $true
$label.Location = New-Object System.Drawing.Point(18, 20)

$fixtureInput = New-Object System.Windows.Forms.TextBox
$fixtureInput.Name = 'AutomationPrompt'
$fixtureInput.AccessibleName = 'Automation Prompt'
$fixtureInput.AccessibleRole = [System.Windows.Forms.AccessibleRole]::Text
$fixtureInput.Location = New-Object System.Drawing.Point(18, 58)
$fixtureInput.Size = New-Object System.Drawing.Size(520, 28)
$fixtureInput.Multiline = $false
$fixtureInput.Text = ''

$form.Controls.Add($label)
$form.Controls.Add($fixtureInput)
$script:fixtureSmokeProcess = $null
$fixtureRoot = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$fixtureStdout = Join-Path $fixtureRoot 'target/fixture-smoke-output.txt'
$fixtureStderr = Join-Path $fixtureRoot 'target/fixture-smoke-error.txt'
$form.Add_Shown({
    $form.Activate()
    $fixtureInput.Focus() | Out-Null
    $fixtureInput.Select(0, 0)
    if ($RunSmoke) {
        $script:fixtureSmokeProcess = Start-Process (Join-Path $fixtureRoot 'target/debug/peercarry-automation.exe') -WindowStyle Hidden -ArgumentList 'examples/automation/fixture-input.json --execute' -WorkingDirectory $fixtureRoot -RedirectStandardOutput $fixtureStdout -RedirectStandardError $fixtureStderr -PassThru
        $null = $script:fixtureSmokeProcess.Handle
    }
})
$timer = New-Object System.Windows.Forms.Timer
$timer.Interval = 300000
$timer.Add_Tick({ $timer.Stop(); $form.Close() })
$timer.Start()
$smokeTimer = New-Object System.Windows.Forms.Timer
$smokeTimer.Interval = 200
$smokeTimer.Add_Tick({
    if ($null -ne $script:fixtureSmokeProcess -and $script:fixtureSmokeProcess.HasExited) {
        $smokeTimer.Stop()
        $form.Close()
    }
})
if ($RunSmoke) { $smokeTimer.Start() }
[void]$form.ShowDialog()
$smokeTimer.Dispose()
$timer.Dispose()
$form.Dispose()
if ($RunSmoke) {
    Get-Content $fixtureStdout -Encoding UTF8 -ErrorAction SilentlyContinue
    Get-Content $fixtureStderr -Encoding UTF8 -ErrorAction SilentlyContinue
    if ($null -eq $script:fixtureSmokeProcess -or !$script:fixtureSmokeProcess.HasExited) { exit 2 }
    $script:fixtureSmokeProcess.WaitForExit()
    if ($null -eq $script:fixtureSmokeProcess.ExitCode) { exit 1 }
    exit $script:fixtureSmokeProcess.ExitCode
}
