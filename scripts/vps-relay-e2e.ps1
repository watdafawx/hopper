<#
.SYNOPSIS
  Runs the whole "Relay through a VPS" setup against a throwaway Linux box
  and checks every step a player would see, start to finish.

.DESCRIPTION
  The box is a WSL distro that stands in for a rented VPS (sshd on
  127.0.0.1, a user with passwordless sudo). The script:

    1. resets the box from its clean snapshot (unless -NoReset),
    2. starts a scratch Hopper Server (its own data, home and servers
       folders; 127.0.0.1 only; HOPPERD_SERVICE=hopperd-test) with one
       empty server in it,
    3. signs in to the box over ssh through the panel's API,
    4. checks a box that pretends to be another system (a fake
       os-release): the check must stop after the first row,
    5. checks the real box, installs the relay, waits for it to connect,
    6. runs the health test ("Test again"): every row must pass,
    7. stops the relay on the box: the card must show it down in plain
       words and the test must fail on the relay row; starts it again and
       the test must pass,
    8. removes the relay and checks nothing of it is left on the box,
    9. stops the scratch panel and deletes its folders (unless -Keep).

  Everything listens on 127.0.0.1 (HOPPER_LOOPBACK=1 at home,
  HOPPERD_VPS_TEST_BIND=127.0.0.1 on the box), so nothing reaches the LAN.
  Velocity on the box runs without account checks
  (HOPPERD_VPS_TEST_OFFLINE=1): the box has no Mojang login to test with.

  The relay program on the box is the published Hopper Server release
  that matches the panel's version (downloaded on the box and checked
  against its SHA-256, as for a player), or -LinuxBinary to upload a local
  Linux build instead.

  The snapshot and the key are never touched. Never point this at a real
  VPS: it reinstalls and removes things there.

.EXAMPLE
  pwsh -NoProfile -File scripts/vps-relay-e2e.ps1 -Exe X:\Dev\hopper\target\perf\fast\hopperd.exe
#>
param(
  # The hopperd.exe to test (a build with the web UI embedded).
  [Parameter(Mandatory)] [string] $Exe,
  # The test box: its snapshot (clean.tar), the ssh key and known_hosts.
  [string] $BoxDir = 'X:\Dev\hopper-vps-test',
  [string] $Distro = 'hopper-vps-test',
  [int] $SshPort = 2222,
  [string] $SshUser = 'tester',
  # A Linux hopperd to upload instead of the release download.
  [string] $LinuxBinary = '',
  # Keep the box as it is (skip the reset from clean.tar).
  [switch] $NoReset,
  # Keep the scratch panel's folders afterwards (for a look at its logs).
  [switch] $Keep,
  # Stop before Remove: the relay stays on the box and the panel's folders
  # are kept, to start that panel again and look at it in a browser.
  [switch] $NoRemove
)
if ($NoRemove) { $Keep = $true }

$ErrorActionPreference = 'Stop'
$script:failures = @()
$script:step = 0

function Say([string] $m) { Write-Host "  $m" }
function Step([string] $m) { $script:step++; Write-Host ""; Write-Host "[$script:step] $m" -ForegroundColor Cyan }
function Pass([string] $m) { Write-Host "  PASS $m" -ForegroundColor Green }
function Fail([string] $m) { Write-Host "  FAIL $m" -ForegroundColor Red; $script:failures += $m }
function Expect([bool] $ok, [string] $m) { if ($ok) { Pass $m } else { Fail $m } }

function Free-Port {
  $l = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
  $l.Start(); $p = $l.LocalEndpoint.Port; $l.Stop(); $p
}

$key = Join-Path $BoxDir 'id_ed25519'
$known = Join-Path $BoxDir 'known_hosts'
$tar = Join-Path $BoxDir 'clean.tar'
foreach ($f in @($Exe, $key, $tar)) { if (-not (Test-Path $f)) { throw "Missing: $f" } }
if ($LinuxBinary -and -not (Test-Path $LinuxBinary)) { throw "Missing: $LinuxBinary" }

# Runs a command on the box as root, over the same ssh login the panel uses.
function Box([string] $cmd) {
  $out = & ssh -i $key -o UserKnownHostsFile=$known -o StrictHostKeyChecking=yes -o BatchMode=yes -o ConnectTimeout=10 -p $SshPort "$SshUser@127.0.0.1" "sudo sh -c '$($cmd -replace "'", "'\''")'" 2>&1
  [pscustomobject]@{ ok = ($LASTEXITCODE -eq 0); out = ($out -join "`n") }
}

function Wait-Until([string] $what, [int] $secs, [scriptblock] $f) {
  $until = (Get-Date).AddSeconds($secs)
  while ((Get-Date) -lt $until) {
    try { $v = & $f; if ($v) { return $v } } catch { }
    Start-Sleep -Milliseconds 700
  }
  throw "Timed out after $secs s waiting for $what"
}

# ---- 1. the box ------------------------------------------------------------------------------

Step 'Test box'
if (-not $NoReset) {
  Say "resetting $Distro from $tar"
  wsl --terminate $Distro 2>$null | Out-Null
  wsl --unregister $Distro | Out-Null
  wsl --import $Distro (Join-Path $BoxDir 'distro') $tar | Out-Null
}
# Starting the distro starts its sshd; this keeps it running while the
# script does (WSL stops an idle distro).
$alive = Start-Process -PassThru -WindowStyle Hidden wsl -ArgumentList @('-d', $Distro, '-u', 'root', '--', 'sleep', '3600')
Wait-Until 'ssh on the box' 60 { (Box 'true').ok } | Out-Null
$os = (Box '. /etc/os-release; echo $PRETTY_NAME').out
Pass "box answers over ssh ($os)"

# ---- 2. the scratch panel --------------------------------------------------------------------

Step 'Scratch Hopper Server'
$root = Join-Path ([System.IO.Path]::GetTempPath()) ("hopper-vps-e2e-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
$data = Join-Path $root 'pdata'; $home_ = Join-Path $root 'home'; $srv = Join-Path $root 'srv'
New-Item -ItemType Directory -Force $data, $home_, (Join-Path $srv 'Relay Test') | Out-Null
Set-Content -Path (Join-Path $srv 'Relay Test\server.properties') -Value "motd=Relay test`nserver-port=$(Free-Port)`n" -NoNewline
$panelPort = Free-Port; $routerPort = Free-Port; $sftpPort = Free-Port
$gamePort = Free-Port; $tunnelPort = Free-Port
# Nothing on the usual ports: a real panel on this PC may have them.
Set-Content -Path (Join-Path $data 'hopperd.toml') -Value "sftp_port = $sftpPort`ndiscovery = false`n`n[router]`nport = $routerPort`n"
$envs = @{
  HOPPERD_SERVICE = 'hopperd-test'; HOPPER_LOOPBACK = '1'
  HOPPERD_VPS_TEST_BIND = '127.0.0.1'; HOPPERD_VPS_TEST_OFFLINE = '1'
}
if ($LinuxBinary) { $envs.HOPPERD_VPS_TEST_BINARY = (Resolve-Path $LinuxBinary).Path }
$saved = @{}
foreach ($k in $envs.Keys) { $saved[$k] = [Environment]::GetEnvironmentVariable($k); [Environment]::SetEnvironmentVariable($k, $envs[$k]) }
$log = Join-Path $root 'hopperd.log'
$proc = Start-Process -PassThru -WindowStyle Hidden -FilePath $Exe -RedirectStandardError $log -RedirectStandardOutput (Join-Path $root 'hopperd.out') `
  -ArgumentList @('--portable', '--port', $panelPort, '--data', "`"$data`"", '--home', "`"$home_`"", '--servers-root', "`"$srv`"")
foreach ($k in $saved.Keys) { [Environment]::SetEnvironmentVariable($k, $saved[$k]) }

$base = "http://127.0.0.1:$panelPort/api/v1"
$web = New-Object Microsoft.PowerShell.Commands.WebRequestSession
$H = @{ 'X-Hopperd' = '1' }
function Api([string] $method, [string] $path, $body = $null, [int] $timeout = 120) {
  $a = @{ Method = $method; Uri = "$base$path"; WebSession = $web; Headers = $H; TimeoutSec = $timeout; SkipHttpErrorCheck = $true; StatusCodeVariable = 'code' }
  if ($null -ne $body) { $a.Body = ($body | ConvertTo-Json -Depth 8 -Compress); $a.ContentType = 'application/json' }
  $r = Invoke-RestMethod @a
  if ($code -ge 400) { throw "$method $path -> $code $(if ($r.error) { $r.error } else { $r })" }
  $r
}

try {
  $codeFile = Join-Path $data 'setup-code.txt'
  Wait-Until 'the panel' 60 { (Test-Path $codeFile) -and (Invoke-WebRequest -Uri "$base/auth/me" -SkipHttpErrorCheck -TimeoutSec 3).StatusCode -gt 0 } | Out-Null
  $code = (Get-Content $codeFile -Raw).Trim()
  # A throwaway panel's owner password: printed only when its folders are kept.
  $pw = 'relay-e2e-' + [guid]::NewGuid().ToString('N').Substring(0, 12)
  Api POST '/auth/setup' @{ code = $code; password = $pw } | Out-Null
  if ($Keep) { Set-Content -Path (Join-Path $root 'owner-password.txt') -Value $pw }
  $servers = Wait-Until 'the test server in the list' 30 { $s = @(Api GET '/servers'); if ($s.Count -and $s[0].id) { $s } }
  $sid = $servers[0].id
  Pass "panel on port $panelPort, owner set up, server '$($servers[0].name)' ($sid)"

  # ---- 3. sign in ------------------------------------------------------------------------------

  Step 'Connect (ssh)'
  Api POST '/vps-relay/setup/connect' @{ host = '127.0.0.1'; port = $SshPort; user = $SshUser; mode = 'key'; key = (Get-Content $key -Raw) } -timeout 90 | Out-Null
  $st = Api GET '/vps-relay/setup'
  Expect ($st.login.host -eq '127.0.0.1' -and $st.login.host_key) "signed in, host key pinned ($($st.login.host_key))"

  # ---- 4. another system stops the check -------------------------------------------------------

  Step 'Check on an unsupported system (fake os-release)'
  $fake = "NAME=`"Rocky Linux`"`nVERSION_ID=`"9.4`"`nID=rocky`nPRETTY_NAME=`"Rocky Linux 9.4 (Blue Onyx)`""
  $b64 = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($fake + "`n"))
  $m = Box "echo $b64 | base64 -d > /run/fake-os-release && mount --bind /run/fake-os-release /etc/os-release"
  if (-not $m.ok) { throw "Couldn't fake the system on the box: $($m.out)" }
  try {
    $c = Api POST '/vps-relay/setup/check' @{ game_port = $gamePort; tunnel_port = $tunnelPort }
    Expect ($c.stop -eq $true -and -not $c.ok) 'the check stops'
    Expect (@($c.rows).Count -eq 1 -and $c.rows[0].id -eq 'system' -and $c.rows[0].state -eq 'fail') "only the system row, failed: $($c.rows[0].text)"
    Expect ($c.rows[0].text -match 'Debian' -and $c.rows[0].text -match 'Ubuntu') 'it names the systems that work'
  } finally {
    Box 'umount /etc/os-release; rm -f /run/fake-os-release' | Out-Null
  }

  # ---- 5. check and install --------------------------------------------------------------------

  Step "Check and install (players' port $gamePort, tunnel port $tunnelPort)"
  $c = Api POST '/vps-relay/setup/check' @{ game_port = $gamePort; tunnel_port = $tunnelPort }
  foreach ($r in $c.rows) { Say "$($r.state.PadRight(4)) $($r.text)" }
  Expect ([bool] $c.ok) 'every check row passes'
  if (-not $c.ok) { throw 'The check failed; nothing to install.' }
  Api POST '/vps-relay/setup/install' @{ server = $sid; game_port = $gamePort; tunnel_port = $tunnelPort } | Out-Null
  $run = Wait-Until 'the install' 900 { $s = Api GET '/vps-relay/setup'; if ($s.run -and -not $s.run.running) { $s.run } }
  foreach ($s in $run.steps) { Say "$($s.state.PadRight(4)) $($s.title)$(if ($s.detail) { ': ' + $s.detail })" }
  Expect (-not $run.error) "install finished$(if ($run.error) { ': ' + $run.error })"
  if ($run.error) { throw 'The install failed.' }
  $card = Wait-Until 'the relay to connect' 90 { $r = Api GET "/servers/$sid/vps-relay"; if ($r.state -eq 'up') { $r } }
  Pass "relay connected, players join at $($card.address)"

  # ---- 6. the health test ----------------------------------------------------------------------

  Step 'Test (all green)'
  $t = Api POST "/servers/$sid/vps-relay/test" @{} -timeout 60
  foreach ($r in $t.rows) { Say "$($r.state.PadRight(4)) $($r.text)" }
  Expect ([bool] $t.ok) 'the test passes'
  Expect ((@($t.rows | Where-Object id -eq 'players')[0]).state -eq 'ok') "a player's server list ping through the VPS reaches this panel"
  $card = Api GET "/servers/$sid/vps-relay"
  Expect ($card.test.ok -eq $true -and $null -ne $card.players) "the card keeps the result (players now: $($card.players))"

  # ---- 7. the relay goes away and comes back ---------------------------------------------------

  Step 'Relay stopped on the box'
  Expect (Box 'systemctl stop hopper-relay').ok 'stopped hopper-relay'
  $card = Wait-Until 'the card to show it down' 60 { $r = Api GET "/servers/$sid/vps-relay"; if ($r.state -eq 'down') { $r } }
  Say "card: $($card.error)"
  Expect ($card.error -and $card.error -notmatch 'os error|\(os |Mux|protocol error') 'the reason is in plain words'
  $t = Api POST "/servers/$sid/vps-relay/test" @{} -timeout 60
  foreach ($r in $t.rows) { Say "$($r.state.PadRight(4)) $($r.text)" }
  Expect (-not $t.ok -and (@($t.rows | Where-Object id -eq 'link')[0]).state -eq 'fail') 'the test fails on the relay row'
  Expect ((@($t.rows | Where-Object id -eq 'players')[0]).state -ne 'ok') 'and players are told they can''t get in'

  Step 'Relay started again'
  Expect (Box 'systemctl start hopper-relay').ok 'started hopper-relay'
  $t = Wait-Until 'a passing test' 60 { $x = Api POST "/servers/$sid/vps-relay/test" @{} -timeout 60; if ($x.ok) { $x } }
  foreach ($r in $t.rows) { Say "$($r.state.PadRight(4)) $($r.text)" }
  Pass 'the test passes again ("Test again" reconnected it)'

  # ---- 8. remove -------------------------------------------------------------------------------

  if ($NoRemove) {
    Say "left installed. Start the panel again with the same folders: $Exe --portable --port $panelPort --data `"$data`" --home `"$home_`" --servers-root `"$srv`" (with the same environment)"
    throw 'stop-before-remove'
  }
  Step 'Remove'
  Api POST '/vps-relay/setup/remove' @{} | Out-Null
  $run = Wait-Until 'the removal' 300 { $s = Api GET '/vps-relay/setup'; if ($s.run -and $s.run.kind -eq 'remove' -and -not $s.run.running) { $s.run } }
  Expect (-not $run.error) "removal finished$(if ($run.error) { ': ' + $run.error })"
  $left = (Box 'systemctl list-unit-files "hopper-*" --no-legend 2>/dev/null; id hopper-relay 2>/dev/null; ls -d /opt/hopper* /var/lib/hopper* 2>/dev/null; ss -Hltn | grep -E ":(' + $gamePort + '|' + $tunnelPort + ')\b"').out.Trim()
  Expect (-not $left) "nothing of the relay is left on the box$(if ($left) { ": $left" })"
} catch {
  if ($_.Exception.Message -ne 'stop-before-remove') { Fail $_.Exception.Message }
} finally {
  if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force }
  if ($alive -and -not $alive.HasExited) { Stop-Process -Id $alive.Id -Force }
  if ($Keep -or $script:failures.Count) { Say "scratch panel kept: $root (log: $log)" } else { Remove-Item -Recurse -Force $root -ErrorAction SilentlyContinue }
}

Write-Host ""
if ($script:failures.Count) {
  Write-Host "FAILED: $($script:failures.Count) check(s)" -ForegroundColor Red
  $script:failures | ForEach-Object { Write-Host "  - $_" -ForegroundColor Red }
  exit 1
}
Write-Host 'ALL PASSED' -ForegroundColor Green
