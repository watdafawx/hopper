# Memory ceiling check for Hopper desktop. Not part of CI.
# Starts a scratch copy of an exe on a fake big instance (make_scratch.py), walks
# the same steps every time and prints working set / private / GPU memory.
#   pwsh -NoProfile -File mem-check.ps1 -Exe C:\scratch\hv-mem.exe -Scratch C:\scratch\mem -Port 47840 -Label 0.58.0 [-IdleSecs 600]
# Build the scratch set first: python .claude\skills\hopper-gpui-testing\mem-scratch.py <Scratch>
# Never use ports 47801/47802. Only the exe it started is killed.
param(
    [Parameter(Mandatory)][string]$Exe,
    [Parameter(Mandatory)][string]$Scratch,
    [int]$Port = 47840,
    [string]$Label = "",
    [int]$IdleSecs = 600,
    [string]$Out = ""
)
$ErrorActionPreference = "Stop"
if ($Port -in 47801, 47802) { throw "port reserved for the user's Hopper" }
$url = "http://127.0.0.1:$Port/mcp"
function Mcp($name, $json = "{}") {
    $body = '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"' + $name + '","arguments":' + $json + '}}'
    Invoke-RestMethod -Uri $url -Method Post -ContentType application/json -Body $body -TimeoutSec 60
}
$env:HOPPER_DATA_DIR = $Scratch
$env:HOPPER_LOOPBACK = "1"
$env:HOPPER_TEST_WINDOW = "1"
$env:HOPPER_MCP_PORT = "$Port"
$p = Start-Process $Exe -ArgumentList "--mcp" -PassThru
try {
    # wait for MCP
    for ($i = 0; $i -lt 60; $i++) { try { Mcp state | Out-Null; break } catch { Start-Sleep 1 } }
    $st = (Mcp state).result.content[0].text
    if ($st -notmatch "Big Pack") { throw "state does not show the scratch instance: $st" }
    if ($st -match "TNP|ATM") { throw "real instance visible, aborting" }
    function Sample($step) {
        Start-Sleep 3
        $pr = Get-Process -Id $p.Id
        $gpu = ""
        try {
            $c = Get-Counter "\GPU Process Memory(pid_$($p.Id)*)\Dedicated Usage" -ErrorAction Stop
            $gpu = [math]::Round((($c.CounterSamples | Measure-Object CookedValue -Sum).Sum) / 1MB)
        } catch {}
        [pscustomobject]@{
            Label = $Label; Step = $step
            WS_MB = [math]::Round($pr.WorkingSet64 / 1MB); Private_MB = [math]::Round($pr.PrivateMemorySize64 / 1MB)
            Peak_WS_MB = [math]::Round($pr.PeakWorkingSet64 / 1MB); GPU_MB = $gpu; Threads = $pr.Threads.Count
        }
    }
    $rows = @()
    # the "What's new" dialog of a fresh data dir: close it
    Mcp key '{"name":"escape"}' | Out-Null
    Mcp navigate '{"tab":"Library"}' | Out-Null
    $rows += Sample "1 library"
    Mcp navigate '{"instance":"Big Pack","view":"Mods"}' | Out-Null
    $rows += Sample "2 big mods tab"
    1..40 | ForEach-Object { Mcp scroll '{"x":700,"y":500,"dy":50,"notches":true}' | Out-Null }
    $rows += Sample "3 scrolled to end"
    # version chip in the title bar: set -ChipX/-ChipY by hand if the layout moves
    Mcp click '{"x":1230,"y":16}' | Out-Null
    $rows += Sample "4 versions dialog"
    Mcp key '{"name":"escape"}' | Out-Null
    Mcp navigate '{"tab":"Servers"}' | Out-Null
    $rows += Sample "5 servers"
    Mcp navigate '{"tab":"Library"}' | Out-Null
    Mcp navigate '{"instance":"Big Pack","view":"Mods"}' | Out-Null
    $rows += Sample "6 back to mods"
    if ($IdleSecs -gt 0) { Start-Sleep $IdleSecs; $rows += Sample "7 idle $IdleSecs s" }
    $rows | Format-Table -AutoSize | Out-String | Write-Host
    if ($Out) { $rows | Export-Csv -Append -NoTypeInformation $Out }
} finally {
    if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force }
}
