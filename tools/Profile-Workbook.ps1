<#
.SYNOPSIS
  Read-only profile of a slow Excel workbook: what makes it slow. No admin needed.
.DESCRIPTION
  Opens the workbook READ-ONLY in a separate hidden Excel process, macros forcibly
  disabled, calculation set to manual (the file is never saved). Reports formula
  counts, functions used, volatile functions, whole-column references, data tables,
  links, VBA presence and the most frequent formula shapes. Cell VALUES are never
  collected. Optional -TimeSheets measures each sheet's recalculation (can take long).
.EXAMPLE
  powershell -ExecutionPolicy Bypass -File Profile-Workbook.ps1 -Path "D:\calc.xlsm"
  powershell -ExecutionPolicy Bypass -File Profile-Workbook.ps1 -Path "D:\calc.xlsm" -TimeSheets
#>
param(
    [Parameter(Mandatory)][string]$Path,
    [string]$Out,
    [switch]$TimeSheets,
    [switch]$NoFormulaText,
    [int]$TopPatterns = 40
)
$ErrorActionPreference = 'Stop'
$Path = (Resolve-Path $Path).Path
if (-not $Out) { $Out = [IO.Path]::ChangeExtension($Path, $null).TrimEnd('.') + '.profile.json' }

$volatile = 'OFFSET','INDIRECT','TODAY','NOW','RAND','RANDBETWEEN','RANDARRAY','CELL','INFO'
$fnRx = [regex]'([A-Z][A-Z0-9\._]*)\('
$colRx = [regex]'(?<![A-Z0-9\]\.!_])C(\[-?\d+\]|\d+)(:C(\[-?\d+\]|\d+))?(?![\d\[A-Z\(_])'

$xl = New-Object -ComObject Excel.Application
$report = [ordered]@{ file = [IO.Path]::GetFileName($Path); size_mb = [math]::Round((Get-Item $Path).Length / 1MB, 1)
    excel_version = "$($xl.Version) build $($xl.Build)"; threads = $null; sheets = @() }
try {
    $xl.Visible = $false; $xl.DisplayAlerts = $false; $xl.ScreenUpdating = $false
    $xl.EnableEvents = $false; $xl.AskToUpdateLinks = $false
    $xl.AutomationSecurity = 3   # msoAutomationSecurityForceDisable: no macros run
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $wb = $xl.Workbooks.Open($Path, 0, $true)   # UpdateLinks=0, ReadOnly
    $report.open_s = [math]::Round($sw.Elapsed.TotalSeconds, 1)
    $report.calc_mode_saved = switch ($xl.Calculation) { -4105 {'automatic'} -4135 {'manual'} 2 {'semiautomatic'} default {"$_"} }
    $xl.Calculation = -4135
    $report.threads = if ($xl.MultiThreadedCalculation.Enabled) { $xl.MultiThreadedCalculation.ThreadCount } else { 1 }
    $report.iteration = [ordered]@{ enabled = $xl.Iteration; max = $xl.MaxIterations; change = $xl.MaxChange }
    $report.has_vba = [bool]$wb.HasVBProject
    $links = $wb.LinkSources(1); $report.external_links = if ($links) { @($links).Count } else { 0 }
    $report.names = $wb.Names.Count

    $allFn = @{}; $allPat = @{}; $tot = [ordered]@{ formulas = 0; volatile_cells = 0; whole_col_ref_cells = 0; data_table_cells = 0; max_len = 0; sum_len = 0 }
    foreach ($ws in $wb.Worksheets) {
        $s = [ordered]@{ name = $ws.Name; used = $ws.UsedRange.Address($false, $false); formulas = 0; distinct_patterns = 0; volatile_cells = 0; whole_col_ref_cells = 0; data_table_cells = 0; top_functions = @{} }
        $ur = $ws.UsedRange
        $cells = [long]$ur.Rows.Count * $ur.Columns.Count
        $s.used_cells = $cells
        if ($cells -gt 0) {
            # Process in row bands to bound memory; FormulaR1C1 groups copies of the same formula.
            $band = [math]::Max(1, [math]::Floor(2000000 / [math]::Max(1, $ur.Columns.Count)))
            $pats = @{}
            for ($r0 = 1; $r0 -le $ur.Rows.Count; $r0 += $band) {
                $rows = [math]::Min($band, $ur.Rows.Count - $r0 + 1)
                $blk = $ur.Rows("$r0`:$($r0 + $rows - 1)")
                $f = $blk.FormulaR1C1
                if ($f -isnot [Array]) { $f = ,$f }
                foreach ($t in $f) {
                    if ($t -isnot [string] -or $t.Length -lt 2 -or $t[0] -ne '=') { continue }
                    $s.formulas++
                    $u = $t.ToUpperInvariant()
                    if ($u.Length -gt $tot.max_len) { $tot.max_len = $u.Length }
                    $tot.sum_len += $u.Length
                    if ($pats.ContainsKey($u)) { $pats[$u]++ ; continue } else { $pats[$u] = 1 }
                }
            }
            foreach ($kv in $pats.GetEnumerator()) {
                $u = $kv.Key; $n = $kv.Value
                $fns = @($fnRx.Matches($u) | ForEach-Object { $_.Groups[1].Value } | Select-Object -Unique)
                foreach ($fn in $fns) { $s.top_functions[$fn] = [long]$s.top_functions[$fn] + $n; $allFn[$fn] = [long]$allFn[$fn] + $n }
                if ($fns | Where-Object { $volatile -contains $_ }) { $s.volatile_cells += $n }
                if ($u -match '^=TABLE\(') { $s.data_table_cells += $n }
                # R1C1 whole-column ref (C3, C[-1]:C[2]) = a C-part with no preceding R-part.
                if ($colRx.IsMatch($u)) { $s.whole_col_ref_cells += $n }
                $allPat["$($ws.Name)|$u"] = $n
            }
            $s.distinct_patterns = $pats.Count
        }
        $s.top_functions = $s.top_functions.GetEnumerator() | Sort-Object Value -Descending | Select-Object -First 12 | ForEach-Object { "$($_.Key)=$($_.Value)" }
        foreach ($k in 'formulas','volatile_cells','whole_col_ref_cells','data_table_cells') { $tot[$k] += $s[$k] }
        if ($TimeSheets -and $s.formulas -gt 0) {
            # Range.Calculate forces this sheet's formulas even when nothing is dirty.
            $sw.Restart(); [void]$ws.UsedRange.Calculate(); $s.sheet_calc_s = [math]::Round($sw.Elapsed.TotalSeconds, 2)
        }
        $report.sheets += [pscustomobject]$s
        Write-Host ("{0,-30} formulas={1,10} patterns={2,7}" -f $ws.Name, $s.formulas, $s.distinct_patterns)
    }
    $tot.avg_len = if ($tot.formulas) { [math]::Round($tot.sum_len / $tot.formulas) } else { 0 }
    $tot.Remove('sum_len')
    $report.totals = $tot
    $report.functions = $allFn.GetEnumerator() | Sort-Object Value -Descending | ForEach-Object { "$($_.Key)=$($_.Value)" }
    if (-not $NoFormulaText) {
        $report.top_patterns_r1c1 = $allPat.GetEnumerator() | Sort-Object Value -Descending | Select-Object -First $TopPatterns |
            ForEach-Object { [pscustomobject]@{ count = $_.Value; sheet = $_.Key.Split('|')[0]; formula = $_.Key.Substring($_.Key.IndexOf('|') + 1) } }
    }
    if ($TimeSheets) { $sw.Restart(); $xl.CalculateFull(); $report.full_calc_s = [math]::Round($sw.Elapsed.TotalSeconds, 2) }
    $wb.Close($false)
} finally {
    $xl.Quit(); [void][Runtime.InteropServices.Marshal]::ReleaseComObject($xl); [GC]::Collect()
}
$report | ConvertTo-Json -Depth 6 | Out-File -Encoding utf8 $Out
Write-Host "Report: $Out"
