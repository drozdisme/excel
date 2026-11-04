<#
.SYNOPSIS
  Evaluate formulas in a private hidden Excel (blank workbook) and print the results.
  Used for differential tests of engine semantics against real Excel.
.DESCRIPTION
  Each formula goes in column H of its own block of rows (dynamic-array semantics via
  Formula2; prefix "L:" for legacy Formula semantics). Output per formula:
    <formula> TAB <rows>x<cols> TAB <type>=<value> | ...   (spill read row-major)
  Types: Double (R format), String, Boolean, empty, err:<text>.
.EXAMPLE
  Probe-Formulas.ps1 -Formulas '=1/0','=SEQUENCE(2)' -Setup 'A1=1;B1=x' -Out res.tsv
#>
param([Parameter(Mandatory)][string[]]$Formulas, [string]$Out, [string]$Setup)
$ErrorActionPreference = 'Stop'
$inv = [Globalization.CultureInfo]::InvariantCulture
function Show($cell) {
    $v = $cell.Value2
    if ($null -eq $v) { return 'empty=' }
    if ($v -is [int]) { return 'err:' + $cell.Text + '=' }
    if ($v -is [double]) { return 'Double=' + $v.ToString('R', $inv) }
    return $v.GetType().Name + '=' + ([string]$v).Replace("`t", ' ')
}
$xl = New-Object -ComObject Excel.Application
try {
    $xl.Visible = $false; $xl.DisplayAlerts = $false; $xl.ScreenUpdating = $false; $xl.EnableEvents = $false
    $wb = $xl.Workbooks.Add(); $ws = $wb.Worksheets.Item(1)
    if ($Setup) { foreach ($kv in $Setup.Split(';')) { $a, $v = $kv.Split('=', 2); $ws.Range($a).Formula2 = $v } }
    $step = 12
    for ($i = 0; $i -lt $Formulas.Count; $i++) {
        $c = $ws.Cells.Item(100 + $i * $step, 8)
        if ($Formulas[$i].StartsWith('L:')) { $c.Formula = $Formulas[$i].Substring(2) } else { $c.Formula2 = $Formulas[$i] }
    }
    $xl.CalculateFull()
    $rows = @()
    for ($i = 0; $i -lt $Formulas.Count; $i++) {
        $c = $ws.Cells.Item(100 + $i * $step, 8)
        $vals = @(); $shape = '1x1'
        if ($c.HasSpill) {
            $sp = $c.SpillingToRange
            $nr = [int]$sp.Rows.Count; $nc = [int]$sp.Columns.Count; if ($nr -lt 1) { $nr = [int]($sp.Count / [math]::Max(1, $nc)) }; $shape = "${nr}x${nc}"
            foreach ($r in 1..$nr) { foreach ($k in 1..$nc) { $vals += Show $sp.Cells.Item($r, $k) } }
        } else { $vals += Show $c }
        $line = "$($Formulas[$i])`t$shape`t$($vals -join ' | ')"
        $rows += $line
        Write-Output $line
    }
    if ($Out) { $rows | Out-File -Encoding utf8 $Out }
    $wb.Close($false)
} finally { $xl.Quit(); [void][Runtime.InteropServices.Marshal]::ReleaseComObject($xl) }

