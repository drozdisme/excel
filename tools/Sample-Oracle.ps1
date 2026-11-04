<#
.SYNOPSIS
  Several ground-truth samples from real Excel in one session: for each sample assign the
  driver cells, recalculate and dump the chosen sheets to <Out>_<k>.tsv (Capture-Oracle format).
.EXAMPLE
  Sample-Oracle.ps1 -Workbook pd.xlsb -Cells 'SkinAnalysis!E3','SkinAnalysis!E4' `
     -Samples '1497151896L|187000+4228014','1497151811R|6062000+4235082' -Sheets SkinAnalysis -Out C:\tmp\pd
#>
param(
    [Parameter(Mandatory)][string]$Workbook,
    [Parameter(Mandatory)][string[]]$Cells,
    [Parameter(Mandatory)][string[]]$Samples,
    [Parameter(Mandatory)][string[]]$Sheets,
    [Parameter(Mandatory)][string]$Out
)
$ErrorActionPreference = 'Stop'
$inv = [Globalization.CultureInfo]::InvariantCulture
$src = (Resolve-Path $Workbook).Path
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("bc_oracle_" + [guid]::NewGuid().ToString('N') + [IO.Path]::GetExtension($src))
Copy-Item $src $tmp
$xl = New-Object -ComObject Excel.Application
try {
    $xl.Visible = $false; $xl.DisplayAlerts = $false; $xl.ScreenUpdating = $false; $xl.EnableEvents = $false
    $xl.AutomationSecurity = 1
    $wb = $xl.Workbooks.Open($tmp, 0, $true)
    $xl.Calculation = -4135
    for ($k = 0; $k -lt $Samples.Count; $k++) {
        $vals = $Samples[$k].Split('|')
        for ($i = 0; $i -lt $Cells.Count; $i++) {
            $sheet, $cell = $Cells[$i].Split('!', 2)
            $v = $vals[$i]
            $r = $wb.Worksheets.Item($sheet).Range($cell)
            if ($v -match '^-?\d+(\.\d+)?([eE][-+]?\d+)?$') { $r.Value2 = [double]$v } else { $r.Value2 = $v }
        }
        $sw = [Diagnostics.Stopwatch]::StartNew(); $xl.Calculate()
        $path = "${Out}_$k.tsv"
        $w = [IO.StreamWriter]::new($path, $false, [Text.UTF8Encoding]::new($false))
        $w.WriteLine("sheet`trow`tcol`ttype`tvalue")
        foreach ($name in $Sheets) {
            $ur = $wb.Worksheets.Item($name).UsedRange
            $r0 = $ur.Row; $c0 = $ur.Column; $a = $ur.Value2
            for ($i = $a.GetLowerBound(0); $i -le $a.GetUpperBound(0); $i++) {
                for ($j = $a.GetLowerBound(1); $j -le $a.GetUpperBound(1); $j++) {
                    $x = $a[$i, $j]
                    if ($null -eq $x) { continue }
                    $t = switch ($x.GetType().Name) { 'Double' { 'n' } 'String' { 's' } 'Boolean' { 'b' } 'Int32' { 'e' } default { 'x' } }
                    $text = if ($t -eq 'n') { $x.ToString('R', $inv) } elseif ($t -eq 'e') { [string]$x } else { ([string]$x).Replace('\', '\\').Replace("`t", '\t').Replace("`r", '\r').Replace("`n", '\n') }
                    $w.WriteLine("$name`t$($r0 + $i - 1)`t$($c0 + $j - 1)`t$t`t$text")
                }
            }
        }
        $w.Close()
        Write-Host ("sample {0}: {1} (calc {2:N1} s) -> {3}" -f $k, $Samples[$k], $sw.Elapsed.TotalSeconds, $path)
    }
    $wb.Close($false)
} finally {
    $xl.Quit(); [void][Runtime.InteropServices.Marshal]::ReleaseComObject($xl); [GC]::Collect(); [GC]::WaitForPendingFinalizers()
    Remove-Item $tmp -ErrorAction SilentlyContinue
}

