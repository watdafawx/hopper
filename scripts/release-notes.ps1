# Prints a release's notes: the release commit's body ("Hopper x.y.z" or
# "Hopper Server x.y.z"), then the subjects of the commits it shipped since
# the tag before, then a compare link. Both release workflows use it for new
# releases; it can also refill older ones:
#
#   .\scripts\release-notes.ps1 v0.7.56
#   .\scripts\release-notes.ps1 v0.7.56 | gh release edit v0.7.56 --notes-file -
#   .\scripts\release-notes.ps1 hopperd-v0.13.5
#
# Hopper desktop tags are v*, Hopper Server tags hopperd-v*. The previous
# tag is looked up among the same app's tags only, so desktop notes don't
# compare against a server release and the other way round.

param([Parameter(Mandatory)][string]$Tag, [string]$Repo = 'watdafawx/hopper')
$ErrorActionPreference = 'Stop'

$server = $Tag.StartsWith('hopperd-v')
$match = if ($server) { 'hopperd-v*' } else { 'v[0-9]*' }

$body = @(git log -1 --format=%b "$Tag^{commit}" | Where-Object { $_ -notmatch '^Co-Authored-By:' })
$notes = ($body -join "`n").Trim()
$prev = git describe --tags --abbrev=0 --match $match "$Tag^" 2>$null
if ($LASTEXITCODE -ne 0) { $prev = $null }
if ($prev) {
    $shipped = @(git log --no-merges --format='- %s' "$prev..$Tag^" | Where-Object { $_ -notmatch '^- Cargo.lock' -and $_ -notmatch '^- Hopper (Server )?\d+\.\d+\.\d+$' })
    if ($shipped.Count) { $notes = ($notes + "`n`n" + ($shipped -join "`n")).Trim() }
    $notes = ($notes + "`n`n**Full Changelog**: https://github.com/$Repo/compare/$prev...$Tag").Trim()
}
if ($server) {
    $notes = ($notes + "`n`nSetup, services and Docker: see the [Hopper Server README](https://github.com/$Repo/blob/main/crates/hopperd/README.md).").Trim()
}
$notes
