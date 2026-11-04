<#
.SYNOPSIS
  Build the BatchCalc release: tests, engine CLI, Excel add-in, single-file installer and a
  portable zip, into .\dist. Run from the repository root.
#>
param([switch]$SkipTests)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
if (Test-Path 'C:\ve-tools\Enter-VEDev.ps1') { . 'C:\ve-tools\Enter-VEDev.ps1' }
$version = (Select-String -Path crates\xll\Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
function Run($exe, [string[]]$a) { & $exe @a; if ($LASTEXITCODE -ne 0) { throw "$exe $($a -join ' ') failed ($LASTEXITCODE)" } }

if (-not $SkipTests) { Run cargo @('test', '--workspace', '--release', '-q', '--exclude', 'batchcalc-setup') }
Run cargo @('build', '--release', '-q', '-p', 'batchcalc', '-p', 'batchcalc-xll')

$payload = Join-Path $root 'target\payload'
Remove-Item $payload -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force "$payload\contracts", "$payload\docs", "$payload\tools" | Out-Null
Copy-Item target\release\batchcalc_xll.dll "$payload\BatchCalc.xll"
Copy-Item target\release\batchcalc.exe "$payload\batchcalc.exe"
Copy-Item contracts\*.json "$payload\contracts\"
Copy-Item docs\USER_GUIDE_RU.md, docs\CONTRACTS.md, docs\VERIFICATION.md "$payload\docs\"
Copy-Item tools\Capture-Oracle.ps1, tools\Profile-Workbook.ps1 "$payload\tools\"

$env:BC_PAYLOAD_DIR = $payload
Run cargo @('build', '--release', '-q', '-p', 'batchcalc-setup')
Remove-Item Env:\BC_PAYLOAD_DIR

$dist = Join-Path $root 'dist'
New-Item -ItemType Directory -Force $dist | Out-Null
$setup = Join-Path $dist "BatchCalc-Setup-$version.exe"
Copy-Item target\release\BatchCalc-Setup.exe $setup -Force
$zip = Join-Path $dist "BatchCalc-$version-portable.zip"
Remove-Item $zip -ErrorAction SilentlyContinue
Compress-Archive -Path "$payload\*" -DestinationPath $zip
$sums = Get-ChildItem $dist -File | Where-Object { $_.Extension -in '.exe', '.zip' } | ForEach-Object { "{0}  {1}" -f (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower(), $_.Name }
$sums | Out-File -Encoding ascii (Join-Path $dist 'SHA256SUMS.txt')
Get-ChildItem $dist | Select-Object Name, Length | Format-Table -AutoSize
