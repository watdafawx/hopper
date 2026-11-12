# Memory around big pictures: banner, Screenshots grid, image viewer open / close.
# Uses a scratch set from mem-scratch.py (1920x1080 banner + 12 screenshots). Not CI.
#   pwsh -NoProfile -File mem-peek.ps1 -Exe <hv.exe> -Scratch <scratch> -Port 47850 -Label fix
param([Parameter(Mandatory)][string]$Exe, [Parameter(Mandatory)][string]$Scratch, [int]$Port = 47850, [string]$Label = "", [string]$Out = "")
$ErrorActionPreference = "Stop"
if ($Port -in 47801, 47802) { throw "port reserved" }
$url = "http://127.0.0.1:$Port/mcp"
function Mcp($n, $j = '{}') { $b = '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"' + $n + '","arguments":' + $j + '}}'; Invoke-RestMethod -Uri $url -Method Post -ContentType application/json -Body $b -TimeoutSec 60 }
$env:HOPPER_DATA_DIR = $Scratch; $env:HOPPER_LOOPBACK = "1"; $env:HOPPER_TEST_WINDOW = "1"; $env:HOPPER_MCP_PORT = "$Port"
$p = Start-Process $Exe -ArgumentList "--mcp" -PassThru
$rows = @()
function Sample($step, $wait) {
    Start-Sleep $wait
    $pr = Get-Process -Id $p.Id; $gpu = ""
    try { $c = Get-Counter "\GPU Process Memory(pid_$($p.Id)*)\Dedicated Usage" -ErrorAction Stop; $gpu = [math]::Round((($c.CounterSamples | Measure-Object CookedValue -Sum).Sum) / 1MB) } catch {}
    $script:rows += [pscustomobject]@{ Label = $Label; Step = $step; WS_MB = [math]::Round($pr.WorkingSet64 / 1MB); Private_MB = [math]::Round($pr.PrivateMemorySize64 / 1MB); GPU_MB = $gpu }
}
try {
    for ($i = 0; $i -lt 60; $i++) { try { Mcp state | Out-Null; break } catch { Start-Sleep 1 } }
    if ((Mcp state).result.content[0].text -notmatch "Big Pack") { throw "scratch instance missing" }
    Mcp key '{"name":"escape"}' | Out-Null
    Sample "1 library" 3
    Mcp navigate '{"instance":"Big Pack","view":"Overview"}' | Out-Null
    Sample "2 instance header (1920x1080 banner)" 4
    Mcp navigate '{"instance":"Big Pack","view":"Screenshots"}' | Out-Null
    Sample "3 screenshots grid (12 x 1920x1080)" 5
    foreach ($round in 1, 2, 3) {
        Mcp click '{"x":442,"y":343}' | Out-Null
        Sample "4.$round viewer open +0.5s" 0.5
        Sample "4.$round viewer open +5s" 5
        Mcp key '{"name":"escape"}' | Out-Null
        Sample "5.$round viewer closed +1s" 1
        Sample "5.$round viewer closed +5s" 5
    }
    $rows | Format-Table -AutoSize | Out-String | Write-Host
    if ($Out) { $rows | Export-Csv -Append -NoTypeInformation $Out }
} finally { if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force } }
