# Prints a release's notes: the "Hopper x.y.z" commit's body, then the
# subjects of the commits it shipped since the tag before, then a compare
# link. The Release workflow uses it for new releases; it can also refill
# older ones:
#
#   .\scripts\release-notes.ps1 v0.7.56
#   .\scripts\release-notes.ps1 v0.7.56 | gh release edit v0.7.56 --notes-file -

param([Parameter(Mandatory)][string]$Tag, [string]$Repo = 'watdafawx/hopper')
$ErrorActionPreference = 'Stop'

$body = @(git log -1 --format=%b "$Tag^{commit}" | Where-Object { $_ -notmatch '^Co-Authored-By:' })
$notes = ($body -join "`n").Trim()
$prev = git describe --tags --abbrev=0 "$Tag^" 2>$null
if ($LASTEXITCODE -ne 0) { $prev = $null }
if ($prev) {
    $shipped = @(git log --no-merges --format='- %s' "$prev..$Tag^" | Where-Object { $_ -notmatch '^- Cargo.lock' })
    if ($shipped.Count) { $notes = ($notes + "`n`n" + ($shipped -join "`n")).Trim() }
    $notes = ($notes + "`n`n**Full Changelog**: https://github.com/$Repo/compare/$prev...$Tag").Trim()
}
$notes
