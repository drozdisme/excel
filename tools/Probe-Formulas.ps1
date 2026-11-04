<#
.SYNOPSIS
  Evaluate formulas in a private hidden Excel (blank workbook) and print the results.
  Used for differential tests of engine semantics against real Excel.
.EXAMPLE
  Probe-Formulas.ps1 -Formulas '=IFERROR(HSTACK(1,NA()),"")','=1/0' [-Out results.tsv]
  Each formula is entered with Formula2 (dynamic-array semantics) in column A of its own row;
  the spill (up to 6 columns) is reported.
#>
param([Parameter(Mandatory)][string[]]$Formulas, [string]$Out, [string]$Setup)
$ErrorActionPreference = 'Stop'
$xl = New-Object -ComObject Excel.Application
try {
    $xl.Visible = $false; $xl.DisplayAlerts = $false; $xl.ScreenUpdating = $false; $xl.EnableEvents = $false
    $wb = $xl.Workbooks.Add(); $ws = $wb.Worksheets.Item(1)
    if ($Setup) { foreach ($kv in $Setup.Split(';')) { $a, $v = $kv.Split('=', 2); $ws.Range($a).Formula2 = $v } }
    $rows = @()
    for ($i = 0; $i -lt $Formulas.Count; $i++) {
        $r = 100 + $i * 3
        # Prefix "L:" enters the formula with legacy (implicit intersection) semantics.
        if ($Formulas[$i].StartsWith('L:')) { $ws.Cells.Item($r, 8).Formula = $Formulas[$i].Substring(2) } else { $ws.Cells.Item($r, 1).Formula2 = $Formulas[$i] }
    }
    $xl.CalculateFull()
    for ($i = 0; $i -lt $Formulas.Count; $i++) {
        $r = 100 + $i * 3
        $vals = @()
        $cols = if ($Formulas[$i].StartsWith('L:')) { 8..8 } else { 1..6 }
        foreach ($c in $cols) {
            $cell = $ws.Cells.Item($r, $c)
            $v = $cell.Value2
            if ($null -eq $v -and $c -gt 1 -and $c -ne 8) { continue }
            $t = if ($null -eq $v) { 'empty' } elseif ($v -is [int]) { 'err:' + $cell.Text } else { $v.GetType().Name }
            $vals += "$t=$v"
        }
        $line = "$($Formulas[$i])`t$($vals -join ' | ')"
        $rows += $line
        Write-Output $line
    }
    if ($Out) { $rows | Out-File -Encoding utf8 $Out }
    $wb.Close($false)
} finally { $xl.Quit(); [void][Runtime.InteropServices.Marshal]::ReleaseComObject($xl) }
