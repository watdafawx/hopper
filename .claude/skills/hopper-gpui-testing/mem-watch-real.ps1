# Start-up and use of Hopper on a COPY of real data (never the real folders): memory, threads, CPU.
#   pwsh -NoProfile -File mem-watch-real.ps1 -Exe <hv.exe> -Data X:\Dev\hopper-mem-real -Label after [-Port 47855] [-IdleSecs 180]
# The copy's settings must point at the copied instances (see the memory-use card). Not CI.
param([Parameter(Mandatory)][string]$Exe, [Parameter(Mandatory)][string]$Data, [string]$Label = "", [int]$Port = 47855, [int]$IdleSecs = 180, [string]$Out = "")
if ($Port -in 47801, 47802) { throw "reserved port" }
$ErrorActionPreference = "Stop"
$env:HOPPER_DATA_DIR = $Data; $env:HOPPER_LOOPBACK = "1"; $env:HOPPER_MCP_PORT = "$Port"
$name = [IO.Path]::GetFileNameWithoutExtension($Exe)
function Mcp($n, $j = '{}') { $b = '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"' + $n + '","arguments":' + $j + '}}'; Invoke-RestMethod -Uri "http://127.0.0.1:$Port/mcp" -Method Post -ContentType application/json -Body $b -TimeoutSec 60 }
$rows = @(); $t0 = Get-Date
$p = Start-Process $Exe -ArgumentList "--mcp" -PassThru
function Row($step) { $pr = Get-Process -Id $p.Id; $script:rows += [pscustomobject]@{ Label = $Label; Step = $step; WS_MB = [int]($pr.WorkingSet64 / 1MB); Priv_MB = [int]($pr.PrivateMemorySize64 / 1MB); PeakWS_MB = [int]($pr.PeakWorkingSet64 / 1MB); Threads = $pr.Threads.Count; Handles = $pr.HandleCount; CPU_s = [math]::Round($pr.TotalProcessorTime.TotalSeconds, 1) } }
function WaitUntil($sec) { $left = $sec - ((Get-Date) - $t0).TotalSeconds; if ($left -gt 0) { Start-Sleep -Seconds $left } }
try {
    WaitUntil 5;  Row "start +5s"
    WaitUntil 15; Row "start +15s"
    WaitUntil 30; Row "start +30s"
    WaitUntil 60; Row "start +60s (Library)"
    $st = (Mcp state).result.content[0].text | ConvertFrom-Json
    if ($st.instances.Count -lt 1) { throw "no instances" }
    foreach ($i in $st.instances) { Mcp navigate ('{"instance":"' + ($i.name -replace '"', '') + '"}') | Out-Null; Start-Sleep 3 }
    Row "each instance opened once"
    $big = ($st.instances | Sort-Object addons -Descending | Select -First 1).name
    Mcp navigate ('{"instance":"' + $big + '","view":"Mods"}') | Out-Null; Start-Sleep 5; Row "big instance Mods tab"
    1..70 | % { Mcp scroll '{"x":700,"y":500,"dy":50,"notches":true}' | Out-Null }; Start-Sleep 5; Row "Mods scrolled to the end"
    Mcp navigate ('{"instance":"' + $big + '","view":"Overview"}') | Out-Null; Mcp navigate ('{"instance":"' + $big + '","view":"Mods"}') | Out-Null; Start-Sleep 4
    foreach ($y in 443, 509, 707) { Mcp click ('{"x":192,"y":' + $y + '}') | Out-Null; Start-Sleep 3 }; Row "after 3 mod peeks"
    Mcp key '{"name":"escape"}' | Out-Null; Mcp navigate '{"tab":"Library"}' | Out-Null; Start-Sleep 5; Row "Library again"
    $c0 = (Get-Process -Id $p.Id).TotalProcessorTime.TotalSeconds; Start-Sleep $IdleSecs; $c1 = (Get-Process -Id $p.Id).TotalProcessorTime.TotalSeconds
    Row "idle $IdleSecs s"
    $idle = [math]::Round(($c1 - $c0) / $IdleSecs / [Environment]::ProcessorCount * 100, 2)
    $rows | Format-Table -AutoSize | Out-String | Write-Host
    "idle CPU: $([math]::Round(($c1 - $c0) / $IdleSecs * 100, 1))% of one core, $idle% of the machine ($([Environment]::ProcessorCount) logical cores), $([math]::Round($c1 - $c0, 1)) CPU s in $IdleSecs s"
    if ($Out) { $rows | Export-Csv -Append -NoTypeInformation $Out }
} finally { if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force } }
