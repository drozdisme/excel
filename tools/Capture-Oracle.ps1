<#
.SYNOPSIS
  Ground truth from real Excel: open a COPY of a workbook in a private hidden Excel, assign
  driver cells, recalculate and dump the values of chosen sheets as TSV.
.DESCRIPTION
  The original file is never modified (a temporary copy is opened read-only and closed
  without saving). Macros are allowed only so that VBA UDFs calculate; events are disabled
  so no Workbook_Open/Change macro runs.
.EXAMPLE
  Capture-Oracle.ps1 -Workbook book.xlsm -Set 'Section_Analysis!K60=TCL-L' -Sheets Shear_Tie_Analysis -Out oracle.tsv
#>
param(
    [Parameter(Mandatory)][string]$Workbook,
    [string[]]$Set = @(),
    [Parameter(Mandatory)][string[]]$Sheets,
    [Parameter(Mandatory)][string]$Out,
    [switch]$Full
)
$ErrorActionPreference = 'Stop'
$src = (Resolve-Path $Workbook).Path
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("bc_oracle_" + [guid]::NewGuid().ToString('N') + [IO.Path]::GetExtension($src))
Copy-Item $src $tmp
$xl = New-Object -ComObject Excel.Application
try {
    $xl.Visible = $false; $xl.DisplayAlerts = $false; $xl.ScreenUpdating = $false
    $xl.EnableEvents = $false; $xl.AskToUpdateLinks = $false
    $xl.AutomationSecurity = 1
    $wb = $xl.Workbooks.Open($tmp, 0, $true)
    $xl.Calculation = -4135
    foreach ($s in $Set) {
        $addr, $val = $s.Split('=', 2)
        $sheet, $cell = $addr.Split('!', 2)
        $r = $wb.Worksheets.Item($sheet.Trim("'")).Range($cell)
        $num = 0.0
        if ($val -match '^-?\d+(\.\d+)?([eE][-+]?\d+)?$' -and -not $val.StartsWith("'")) { $r.Value2 = [double]$val }
        else { $r.Value2 = $val }
    }
    if ($Full) { $xl.CalculateFull() } else { $xl.Calculate() }
    $w = [IO.StreamWriter]::new($Out, $false, [Text.UTF8Encoding]::new($false))
    $w.WriteLine("sheet`trow`tcol`ttype`tvalue")
    foreach ($name in $Sheets) {
        $ws = $wb.Worksheets.Item($name)
        $ur = $ws.UsedRange
        $r0 = $ur.Row; $c0 = $ur.Column
        $vals = $ur.Value2
        if ($vals -isnot [Array]) { $tmpv = New-Object 'object[,]' 1,1; $tmpv[0,0] = $vals; $vals = $tmpv }
        $lr = $vals.GetLowerBound(0); $lc = $vals.GetLowerBound(1)
        for ($i = $lr; $i -le $vals.GetUpperBound(0); $i++) {
            for ($j = $lc; $j -le $vals.GetUpperBound(1); $j++) {
                $v = $vals[$i, $j]
                if ($null -eq $v) { continue }
                $t = switch ($v.GetType().Name) { 'Double' { 'n' } 'String' { 's' } 'Boolean' { 'b' } 'Int32' { 'e' } default { 'x' } }
                $text = if ($t -eq 'n') { $v.ToString('R', [Globalization.CultureInfo]::InvariantCulture) } elseif ($t -eq 'e') { [string]$v } else { ([string]$v).Replace('\', '\\').Replace("`t", '\t').Replace("`r", '\r').Replace("`n", '\n') }
                $w.WriteLine("$name`t$($r0 + $i - $lr)`t$($c0 + $j - $lc)`t$t`t$text")
            }
        }
    }
    $w.Close()
    $wb.Close($false)
} finally {
    $xl.Quit(); [void][Runtime.InteropServices.Marshal]::ReleaseComObject($xl); [GC]::Collect(); [GC]::WaitForPendingFinalizers()
    Remove-Item $tmp -ErrorAction SilentlyContinue
}
Write-Host "oracle written: $Out"


