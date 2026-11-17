<#
.SYNOPSIS
  Tries "several servers behind one relay" (B7) for real: a real Velocity on
  a throwaway Linux box, two scratch servers, player-side handshakes.

.DESCRIPTION
  Same box and style as vps-relay-e2e.ps1 (a WSL distro standing in for a
  rented VPS; everything on 127.0.0.1; Velocity without account checks via
  HOPPERD_VPS_TEST_OFFLINE). The script:

    1. resets the box from its clean snapshot (unless -NoReset),
    2. starts a scratch Hopper Server with two throwaway servers, Alpha
       (a.test) and Beta (b.test), both set up as Fabric servers with a
       Simple Voice Chat jar (empty files: neither server is ever started;
       home's router answers their server list pings from their own
       server.properties),
    3. sets up the relay for Alpha, then "Use the relay" on it,
    4. sets up the relay again for Beta (a second server on a relay that's
       already set up), then "Use the relay" on it,
    5. checks velocity.toml on the box and what a player's handshake gets:
       a.test -> Alpha, b.test -> Beta, the bare IP and an unknown name ->
       the relay's refusal (two routes),
    6. checks the voice answers (first voice server owns it, the other
       says so),
    7. takes Beta off ("Stop using the relay", then the relay's servers
       list) and runs the setup once more: Velocity restarts, Alpha still
       answers, and with one server left every address reaches it,
    8. runs the setup a third time with nothing changed: Velocity is not
       restarted,
    9. removes the relay and stops the scratch panel.

  Player-side means: a Minecraft handshake + status request (and a login
  handshake for the refusal text) sent from this PC to the relay's
  players' port on the box. No Minecraft client or server jar is used.

  Never point this at a real VPS. Never deletes the box snapshot or key.

.EXAMPLE
  pwsh -NoProfile -File scripts/vps-relay-multi-e2e.ps1 -Exe X:\Dev\hopper\target\perf\fast\hopperd.exe
#>
param(
  [Parameter(Mandatory)] [string] $Exe,
  [string] $BoxDir = 'X:\Dev\hopper-vps-test',
  [string] $Distro = 'hopper-vps-test',
  [int] $SshPort = 2222,
  [string] $SshUser = 'tester',
  # A Linux hopperd to upload instead of the release download.
  [string] $LinuxBinary = '',
  [switch] $NoReset,
  [switch] $Keep,
  # Stop before Remove and keep the panel's folders.
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
# Windows' ssh refuses a key other users can read, and a key in a shared
# folder inherits that. Use a private copy (the original stays as it is).
$keyDir = Join-Path ([System.IO.Path]::GetTempPath()) ("hopper-vps-key-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Force $keyDir | Out-Null
$keyCopy = Join-Path $keyDir 'id_ed25519'
Copy-Item $key $keyCopy
& icacls $keyCopy /inheritance:r /grant:r "$($env:USERDOMAIN)\$($env:USERNAME):F" | Out-Null
$key = $keyCopy
if ($LinuxBinary -and -not (Test-Path $LinuxBinary)) { throw "Missing: $LinuxBinary" }

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

# ---- a player's side of the Minecraft protocol (no client needed) -----------------------------

function VarInt([int] $v) {
  $b = [System.Collections.Generic.List[byte]]::new()
  while ($true) {
    if (($v -band -128) -eq 0) { $b.Add([byte] $v); break }
    $b.Add([byte](($v -band 127) -bor 128)); $v = $v -shr 7
  }
  , $b.ToArray()
}
function Mc-String([string] $s) {
  $t = [Text.Encoding]::UTF8.GetBytes($s)
  $l = [System.Collections.Generic.List[byte]]::new(); $l.AddRange([byte[]](VarInt $t.Length)); $l.AddRange($t); , $l.ToArray()
}
function Mc-Packet([int] $id, [byte[]] $body) {
  $in = [System.Collections.Generic.List[byte]]::new(); $in.AddRange([byte[]](VarInt $id)); if ($body) { $in.AddRange($body) }
  $o = [System.Collections.Generic.List[byte]]::new(); $o.AddRange([byte[]](VarInt $in.Count)); $o.AddRange($in); , $o.ToArray()
}
function Mc-Handshake([string] $addr, [int] $port, [int] $state) {
  $b = [System.Collections.Generic.List[byte]]::new()
  $b.AddRange([byte[]](VarInt 767)); $b.AddRange([byte[]](Mc-String $addr))
  $b.Add([byte]($port -shr 8)); $b.Add([byte]($port -band 255)); $b.AddRange([byte[]](VarInt $state))
  , (Mc-Packet 0 $b.ToArray())
}
function Read-VarInt($s) {
  $n = 0; $shift = 0
  while ($true) {
    $b = $s.ReadByte(); if ($b -lt 0) { throw 'closed' }
    $n = $n -bor (($b -band 127) -shl $shift); if (($b -band 128) -eq 0) { break }; $shift += 7
  }
  $n
}
# The text of the first packet's JSON string (status answer or disconnect).
function Read-Json($s) {
  $len = Read-VarInt $s
  $buf = [byte[]]::new($len); $got = 0
  while ($got -lt $len) { $r = $s.Read($buf, $got, $len - $got); if ($r -le 0) { throw 'closed' }; $got += $r }
  $i = 0; $id = 0; $shift = 0
  while ($true) { $b = $buf[$i++]; $id = $id -bor (($b -band 127) -shl $shift); if (($b -band 128) -eq 0) { break }; $shift += 7 }
  $sl = 0; $shift = 0
  while ($true) { $b = $buf[$i++]; $sl = $sl -bor (($b -band 127) -shl $shift); if (($b -band 128) -eq 0) { break }; $shift += 7 }
  [Text.Encoding]::UTF8.GetString($buf, $i, $sl)
}

# A server list ping to 127.0.0.1:$port typing address $addr: the status
# JSON as an object, or an object with .error when it didn't answer.
function Mc-Status([int] $port, [string] $addr) {
  $c = [System.Net.Sockets.TcpClient]::new()
  try {
    $c.ReceiveTimeout = 20000; $c.SendTimeout = 10000
    $c.Connect('127.0.0.1', $port)
    $s = $c.GetStream()
    $out = [byte[]]((Mc-Handshake $addr $port 1) + (Mc-Packet 0 $null))
    $s.Write($out, 0, $out.Length)
    (Read-Json $s) | ConvertFrom-Json
  } catch { [pscustomobject]@{ error = $_.Exception.Message } } finally { $c.Dispose() }
}
function Status-Text($st) {
  if ($st.error) { return "error: $($st.error)" }
  $d = $st.description
  if ($d -is [string]) { return $d }
  if ($d.text -and -not $d.extra) { return [string] $d.text }
  $t = [string] $d.text; foreach ($x in @($d.extra)) { $t += [string] $x.text }; $t
}
# A login for player $name on address $addr, as far as the first answer: what
# came back ('' when nothing did in 8 s).
function Mc-Login([int] $port, [string] $addr, [string] $name) {
  $c = [System.Net.Sockets.TcpClient]::new()
  try {
    $c.ReceiveTimeout = 8000; $c.Connect('127.0.0.1', $port)
    $s = $c.GetStream()
    $b = [System.Collections.Generic.List[byte]]::new()
    $b.AddRange([byte[]](Mc-String $name)); $b.AddRange([byte[]]::new(16))
    $out = [byte[]]((Mc-Handshake $addr $port 2) + (Mc-Packet 0 $b.ToArray()))
    $s.Write($out, 0, $out.Length)
    # Velocity answers a login with Set Compression (3 bytes), then packets
    # carry a data length (0: not compressed). The next one is the server's
    # side of it: a kick here, since nothing runs behind the router.
    $len = Read-VarInt $s
    $buf = [byte[]]::new($len); $got = 0
    while ($got -lt $len) { $r = $s.Read($buf, $got, $len - $got); if ($r -le 0) { throw 'closed' }; $got += $r }
    if ($buf[0] -eq 3) {
      $len = Read-VarInt $s
      $buf = [byte[]]::new($len); $got = 0
      while ($got -lt $len) { $r = $s.Read($buf, $got, $len - $got); if ($r -le 0) { throw 'closed' }; $got += $r }
      $i = 0; if ($buf[$i] -eq 0) { $i++ }   # data length 0
      $i++                                     # packet id
      # The string's length prefix is a varint: skip it, keep the text.
      while ($buf[$i] -band 128) { $i++ }; $i++
      [Text.Encoding]::UTF8.GetString($buf, $i, $buf.Length - $i)
    } else { "packet id $($buf[0])" }
  } catch { "no answer ($($_.Exception.Message -replace '\s+', ' '))" } finally { $c.Dispose() }
}
# A login attempt: the text it's turned away with ('' when it was let in far enough to stop reading).
function Mc-Join([int] $port, [string] $addr) {
  $c = [System.Net.Sockets.TcpClient]::new()
  try {
    $c.ReceiveTimeout = 15000; $c.Connect('127.0.0.1', $port)
    $s = $c.GetStream()
    $out = [byte[]](Mc-Handshake $addr $port 2)
    $s.Write($out, 0, $out.Length)
    $j = Read-Json $s
    try { ($j | ConvertFrom-Json).text } catch { $j }
  } catch { "error: $($_.Exception.Message)" } finally { $c.Dispose() }
}

# ---- 1. the box ------------------------------------------------------------------------------

Step 'Test box'
if (-not $NoReset) {
  Say "resetting $Distro from $tar"
  wsl --terminate $Distro 2>$null | Out-Null
  wsl --unregister $Distro | Out-Null
  wsl --import $Distro (Join-Path $BoxDir 'distro') $tar | Out-Null
}
$alive = Start-Process -PassThru -WindowStyle Hidden wsl -ArgumentList @('-d', $Distro, '-u', 'root', '--', 'sleep', '7200')
Wait-Until 'ssh on the box' 240 { (Box 'true').ok } | Out-Null
Pass "box answers over ssh ($((Box '. /etc/os-release; echo $PRETTY_NAME').out))"

# ---- 2. the scratch panel and two servers ----------------------------------------------------

Step 'Scratch Hopper Server with two servers'
$root = Join-Path ([System.IO.Path]::GetTempPath()) ("hopper-vps-multi-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
$data = Join-Path $root 'pdata'; $home_ = Join-Path $root 'home'; $srv = Join-Path $root 'srv'
New-Item -ItemType Directory -Force $data, $home_ | Out-Null
foreach ($n in @('Alpha', 'Beta')) {
  $d = Join-Path $srv $n
  New-Item -ItemType Directory -Force (Join-Path $d 'mods') | Out-Null
  Set-Content -Path (Join-Path $d 'server.properties') -Value "motd=$n test`nserver-port=$(Free-Port)`n" -NoNewline
  # A Fabric 1.20.1 server as far as Hopper can tell, never started.
  Set-Content -Path (Join-Path $d 'hopper.json') -Value (@{ name = $n; game_version = '1.20.1'; loader = 'fabric'; format = 6 } | ConvertTo-Json -Compress) -NoNewline
  # A voice mod, as far as the relay can tell (it looks at the jar's name).
  [IO.File]::WriteAllBytes((Join-Path $d 'mods\voicechat-fabric-1.20.1-2.6.27.jar'), [byte[]]@())
}
$panelPort = Free-Port; $routerPort = Free-Port; $sftpPort = Free-Port
$gamePort = Free-Port; $tunnelPort = Free-Port
Set-Content -Path (Join-Path $data 'hopperd.toml') -Value "sftp_port = $sftpPort`ndiscovery = false`n`n[router]`nport = $routerPort`n"
$envs = @{
  HOPPERD_SERVICE = 'hopperd-test'; HOPPER_LOOPBACK = '1'
  HOPPERD_VPS_TEST_BIND = '127.0.0.1'; HOPPERD_VPS_TEST_OFFLINE = '1'
}
if ($LinuxBinary) { $envs.HOPPERD_VPS_TEST_BINARY = (Resolve-Path $LinuxBinary).Path }
$saved = @{}
foreach ($k in $envs.Keys) { $saved[$k] = [Environment]::GetEnvironmentVariable($k); [Environment]::SetEnvironmentVariable($k, $envs[$k]) }
$log = Join-Path $root 'hopperd.log'
Say "panel port $panelPort, router $routerPort, players' port on the box $gamePort, tunnel $tunnelPort"
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
function Run-Job($j, [int] $secs = 300) {
  $r = Wait-Until "job $($j.job)" $secs { $x = Api GET "/jobs/$($j.job)"; if ($x.done) { $x } }
  if ($r.error) { throw "job '$($r.title)' failed: $($r.error)" }
  $r
}
function Install([string] $sid) {
  Api POST '/vps-relay/setup/install' @{ server = $sid; game_port = $gamePort; tunnel_port = $tunnelPort } | Out-Null
  $run = Wait-Until 'the install' 900 { $s = Api GET '/vps-relay/setup'; if ($s.run -and -not $s.run.running) { $s.run } }
  foreach ($s in $run.steps) { Say "$($s.state.PadRight(4)) $($s.title)$(if ($s.detail) { ': ' + $s.detail })" }
  $run
}
function Velocity-Pid { (Box 'systemctl show -p MainPID --value hopper-velocity').out.Trim() }
function Velocity-Toml { (Box 'cat /etc/hopper-relay/velocity.toml').out }

try {
  $codeFile = Join-Path $home_ '.hopper-server\setup-code.txt'
  Wait-Until 'the panel' 60 { (Test-Path $codeFile) -and (Invoke-WebRequest -Uri "$base/auth/me" -SkipHttpErrorCheck -TimeoutSec 3).StatusCode -gt 0 } | Out-Null
  $code = (Get-Content $codeFile -Raw).Trim()
  $pw = 'relay-multi-' + [guid]::NewGuid().ToString('N').Substring(0, 12)
  Api POST '/auth/setup' @{ code = $code; password = $pw } | Out-Null
  if ($Keep) { Set-Content -Path (Join-Path $root 'owner-password.txt') -Value $pw }
  $list = Wait-Until 'both servers in the list' 30 { $s = @(Api GET '/servers'); if ($s.Count -ge 2 -and $s[0].id -and $s[1].id) { $s } }
  $A = ($list | Where-Object name -eq 'Alpha').id; $B = ($list | Where-Object name -eq 'Beta').id
  Expect ($A -and $B) "panel on port $panelPort, servers Alpha ($A) and Beta ($B)"
  Api PUT "/servers/$A/address" @{ addresses = @('a.test') } | Out-Null
  Api PUT "/servers/$B/address" @{ addresses = @('b.test') } | Out-Null

  # ---- 3. first server -------------------------------------------------------------------------

  Step 'Connect, check, set up the relay for Alpha'
  $c = Api POST '/vps-relay/setup/connect' @{ host = '127.0.0.1'; port = $SshPort; user = $SshUser; mode = 'key'; key = (Get-Content $key -Raw) } -timeout 90
  $c = Api POST '/vps-relay/setup/check' @{ game_port = $gamePort; tunnel_port = $tunnelPort }
  foreach ($r in $c.rows) { Say "$($r.state.PadRight(4)) $($r.text)" }
  if (-not $c.ok) { throw 'The check failed.' }
  $run = Install $A
  Expect (-not $run.error) "install for Alpha finished$(if ($run.error) { ': ' + $run.error })"
  if ($run.error) { throw 'install failed' }
  Wait-Until 'the relay to connect' 90 { (Api GET "/servers/$A/vps-relay").state -eq 'up' } | Out-Null
  $toml1 = Velocity-Toml
  Say ($toml1 -split "`n" | Where-Object { $_ -match '^(\w+ = "127|try|"[a-z.]+" = )' } | ForEach-Object { "toml: $_" })
  Expect ($toml1 -match '(?m)^alpha = "127.0.0.1:25580"') 'velocity.toml: alpha on 25580'
  Expect ($toml1 -match '(?m)^"a.test" = \["alpha"\]') 'velocity.toml: a.test is forced to alpha'
  $pid1 = Velocity-Pid
  Say "Velocity pid $pid1"

  Step 'Use the relay on Alpha'
  $j = Api POST "/servers/$A/vps-relay/forward" @{ action = 'use'; restart = $false }
  Run-Job $j | Out-Null
  $st = Api GET "/servers/$A/vps-relay"
  Expect ($st.mode -eq 'relay' -or $st.forward.mode -eq 'relay') "Alpha behind Velocity (mode: $($st.mode)$($st.forward.mode))"

  # ---- 4. second server on a relay that is already set up -------------------------------------

  Step 'Second server: set up the relay again for Beta'
  $run = Install $B
  Expect (-not $run.error) "install for Beta finished$(if ($run.error) { ': ' + $run.error })"
  if ($run.error) { throw 'install for Beta failed' }
  $vstep = $run.steps | Where-Object { $_.title -match 'forwarding secret' } | Select-Object -First 1
  Say "Velocity step: $($vstep.state) $($vstep.detail)"
  $toml2 = Velocity-Toml
  Say ($toml2 -split "`n" | Where-Object { $_ -match '^(\w+ = "127|try|"[a-z.]+" = )' } | ForEach-Object { "toml: $_" })
  Expect ($toml2 -match '(?m)^alpha = "127.0.0.1:25580"' -and $toml2 -match '(?m)^beta = "127.0.0.1:25581"') 'velocity.toml: alpha keeps 25580, beta gets 25581'
  Expect ($toml2 -match '(?m)^"a.test" = \["alpha"\]' -and $toml2 -match '(?m)^"b.test" = \["beta"\]') 'forced hosts: a.test -> alpha, b.test -> beta'
  Expect ($toml2 -match '(?m)^try = \["alpha", "beta"\]') 'try lists both'
  $pid2 = Velocity-Pid
  Expect ($pid2 -ne $pid1) "Velocity restarted for the new server (pid $pid1 -> $pid2)"
  Wait-Until 'both servers on the relay' 60 { $r = Api GET '/vps-relays'; @($r.relays[0].servers).Count -eq 2 -and $r.relays[0].state -eq 'up' } | Out-Null
  $j = Api POST "/servers/$B/vps-relay/forward" @{ action = 'use'; restart = $false }
  Run-Job $j | Out-Null
  Wait-Until 'Velocity listening' 60 { (Box 'ss -Hltn | grep -c :25577').out.Trim() -ne '0' } | Out-Null
  Wait-Until 'both backend ports' 60 { (Box 'ss -Hltn | grep -c -e :25580 -e :25581').out.Trim() -eq '2' } | Out-Null
  Pass 'Velocity and both backend ports are listening on the box'

  # ---- 5. what players get ---------------------------------------------------------------------

  Step "Player-side status pings to the players' port $gamePort"
  # Velocity may need a moment after its restart.
  $sa = Wait-Until 'a.test to answer' 60 { $x = Mc-Status $gamePort 'a.test'; if (-not $x.error) { $x } }
  $sb = Mc-Status $gamePort 'b.test'
  $ta = Status-Text $sa; $tb = Status-Text $sb
  Say "a.test -> '$ta'"; Say "b.test -> '$tb'"
  Expect ($ta -match 'Alpha') 'a.test lands on Alpha'
  Expect ($tb -match 'Beta') 'b.test lands on Beta'
  $sa2 = Mc-Status $gamePort 'A.Test.'
  Expect ((Status-Text $sa2) -match 'Alpha') "'A.Test.' (case, trailing dot) lands on Alpha: '$(Status-Text $sa2)'"
  $sip = Mc-Status $gamePort '127.0.0.1'
  $tip = Status-Text $sip; Say "bare IP -> '$tip'"
  Expect ($tip -match 'No server goes by this address') 'two servers: the bare IP is refused at the relay (status)'
  $sx = Mc-Status $gamePort 'c.test'
  Expect ((Status-Text $sx) -match 'No server goes by this address') "an unknown name is refused (status): '$(Status-Text $sx)'"
  $jx = Mc-Join $gamePort '127.0.0.1'
  Expect ($jx -match 'No server here goes by this address') "a login on the bare IP is turned away: '$jx'"
  # A login (offline mode on the box, so no account is needed) goes through
  # Velocity's forced hosts. Which server it goes on to is not told apart here:
  # the player is let in and then dropped, because nothing runs behind the
  # router. (Velocity's login rate limit is 3 s per address; the streams
  # counters at home lag, so they are printed for a look, not checked.)
  function Streams([string] $id) { [int](Api GET "/servers/$id/vps-relay").total.streams }
  foreach ($pair in @(@('a.test', $A, $B, 'LoginA'), @('b.test', $B, $A, 'LoginB'))) {
    Start-Sleep -Seconds 4
    $mine = Streams $pair[1]; $other = Streams $pair[2]
    $r = Mc-Login $gamePort $pair[0] $pair[3]
    # What came back is binary: Login Success (the player's id and name).
    Say "login as $($pair[3]) on $($pair[0]): $(if ($r -match $pair[3]) { 'Login Success' } else { $r })"
    Expect ($r -match $pair[3]) "Velocity let $($pair[3]) in on $($pair[0]) (offline logins; the relay didn't refuse it)"
    try { Wait-Until "a stream for the server of $($pair[0])" 20 { (Streams $pair[1]) -gt $mine } | Out-Null } catch { }
    $dm = (Streams $pair[1]) - $mine; $do = (Streams $pair[2]) - $other
    Say "streams at home: this address's server +$dm, the other +$do"
  }
  $bs = (Box 'grep -v ^# /var/lib/hopper-velocity/plugins/bStats/config.txt').out
  Expect ($bs -match '(?m)^enabled=false') "usage reports stay off after Velocity started ($(($bs -split "`n" | Select-Object -First 1)))"

  # ---- 6. voice --------------------------------------------------------------------------------

  Step 'Voice: first voice server owns the relay voice'
  $rl = Api GET '/vps-relays'
  $r0 = $rl.relays[0]
  Say "voice owner: $($r0.voice_owner)"
  foreach ($s in $r0.servers) { Say "server $($s.name): mode=$($s.mode) voice=$($s.voice) address=$($s.address) addresses=$($s.addresses | ConvertTo-Json -Compress)" }
  Expect ($r0.voice_owner -eq 'Alpha') 'GET /vps-relays: voice owner is Alpha'
  Expect (($r0.servers | Where-Object name -eq 'Alpha').voice -eq $true -and ($r0.servers | Where-Object name -eq 'Beta').voice -eq $false) 'only Alpha is marked as having the voice'
  $cardB = Api GET "/servers/$B/vps-relay"
  Say "Beta's voice answer: $($cardB.voice | ConvertTo-Json -Compress)"
  Expect ($cardB.voice.state -eq 'other' -and $cardB.voice.owner -eq 'Alpha') 'Beta''s card says the voice goes through for Alpha only'
  $cardA = Api GET "/servers/$A/vps-relay"
  Say "Alpha's voice answer: $($cardA.voice | ConvertTo-Json -Compress)"
  Expect ($cardA.voice.state -ne 'other') 'Alpha''s own card does not say "other"'
  # Owner only.
  $anon = Invoke-WebRequest -Uri "$base/vps-relays" -Headers $H -SkipHttpErrorCheck
  Expect ($anon.StatusCode -eq 401) "GET /vps-relays without a login: $($anon.StatusCode)"

  # ---- 7. take Beta off ------------------------------------------------------------------------

  Step 'Take Beta off the relay; Alpha keeps working'
  $refused = $false
  try { Api PUT '/vps-relay' @{ servers = @($A) } | Out-Null } catch { $refused = $_.Exception.Message -match 'Stop using the relay for Beta first' }
  Expect $refused 'taking Beta off while it checks accounts through the relay is refused'
  $j = Api POST "/servers/$B/vps-relay/forward" @{ action = 'stop'; restart = $false }
  Run-Job $j | Out-Null
  Api PUT '/vps-relay' @{ servers = @($A) } | Out-Null
  $pidBefore = Velocity-Pid
  # Until the setup runs again, Velocity still has beta's line: Alpha must already be fine.
  $sa = Mc-Status $gamePort 'a.test'
  Expect ((Status-Text $sa) -match 'Alpha') "Alpha answers right after Beta came off: '$(Status-Text $sa)'"
  $run = Install $A
  Expect (-not $run.error) 'setup run again for Alpha'
  $toml3 = Velocity-Toml
  Say ($toml3 -split "`n" | Where-Object { $_ -match '^(\w+ = "127|try|"[a-z.]+" = )' } | ForEach-Object { "toml: $_" })
  Expect ($toml3 -notmatch 'beta' -and $toml3 -match '(?m)^alpha = "127.0.0.1:25580"') 'velocity.toml: beta is gone, alpha still on 25580'
  $pidAfter = Velocity-Pid
  Expect ($pidAfter -ne $pidBefore) "Velocity restarted when the file changed (pid $pidBefore -> $pidAfter)"
  $sa = Wait-Until 'Alpha after the restart' 60 { $x = Mc-Status $gamePort 'a.test'; if (-not $x.error) { $x } }
  Expect ((Status-Text $sa) -match 'Alpha') "a.test still reaches Alpha: '$(Status-Text $sa)'"
  $sip = Mc-Status $gamePort '127.0.0.1'
  $sb = Mc-Status $gamePort 'b.test'
  Say "one server left: bare IP -> '$(Status-Text $sip)'; b.test -> '$(Status-Text $sb)'"
  Expect ((Status-Text $sip) -match 'Alpha') 'one route left: the bare IP reaches Alpha (IP joins kept)'
  Expect ((Status-Text $sb) -match 'Alpha') 'one route left: even the old b.test reaches Alpha'
  $cardB = Api GET "/servers/$B/vps-relay"
  Expect ($cardB.relayed -eq $false) 'Beta''s card: not on the relay any more'

  # ---- 8. nothing changed: no restart ------------------------------------------------------------

  Step 'Setup again with nothing changed: Velocity is left alone'
  $pidBefore = Velocity-Pid
  $run = Install $A
  $vstep = $run.steps | Where-Object { $_.title -match 'forwarding secret' } | Select-Object -First 1
  Expect ($vstep.detail -match 'already there') "the Velocity step changed nothing ($($vstep.detail))"
  Expect ((Velocity-Pid) -eq $pidBefore) 'Velocity was not restarted'

  # ---- 9. remove -------------------------------------------------------------------------------

  if ($NoRemove) {
    Say "left installed; panel folders kept: $root"
    throw 'stop-before-remove'
  }
  Step 'Remove'
  $j = Api POST "/servers/$A/vps-relay/forward" @{ action = 'stop'; restart = $false }
  Run-Job $j | Out-Null
  Api POST '/vps-relay/setup/remove' @{} | Out-Null
  $run = Wait-Until 'the removal' 300 { $s = Api GET '/vps-relay/setup'; if ($s.run -and $s.run.kind -eq 'remove' -and -not $s.run.running) { $s.run } }
  Expect (-not $run.error) "removal finished$(if ($run.error) { ': ' + $run.error })"
  $left = (Box ("systemctl list-unit-files --no-legend | grep hopper; getent passwd hopper-relay hopper-velocity; " +
    "ls -d /opt/hopper* /var/lib/hopper* /etc/hopper* 2>/dev/null; ss -Hltn | grep -e :$gamePort -e :$tunnelPort -e :2558; true")).out.Trim()
  Expect (-not $left) "nothing of the relay is left on the box$(if ($left) { ": $left" })"
} catch {
  if ($_.Exception.Message -ne 'stop-before-remove') { Fail $_.Exception.Message; Write-Host $_.ScriptStackTrace -ForegroundColor DarkGray }
} finally {
  Remove-Item -Recurse -Force $keyDir -ErrorAction SilentlyContinue
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
