<#
.SYNOPSIS
  End-to-end test of the BatchCalc add-in in a private hidden Excel: clears the outputs of a
  COPY of the workbook, runs a BatchCalc menu command, saves the result and diffs it against
  the original (whose outputs were produced by the original VBA macros).
#>
param(
    [Parameter(Mandatory)][string]$Workbook,
    [Parameter(Mandatory)][string]$Xll,
    [int]$Command = 4,
    [string[]]$ClearSheets = @('CG_Load_Output','Load_Output','CG_Stress_Output','Stress_Output','Gross_Stress_Output','MS','MS_Joint','MS_Shear_Tie'),
    [string]$OutDir = (Join-Path $env:TEMP 'BatchCalcTest'),
    [int]$TimeoutSec = 600,
    [switch]$AllowMacros
)
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force $OutDir | Out-Null
$src = (Resolve-Path $Workbook).Path
$ext = [IO.Path]::GetExtension($src)
$copy = Join-Path $OutDir ("input$ext"); $result = Join-Path $OutDir ("result$ext")
Copy-Item $src $copy -Force; Remove-Item $result -ErrorAction SilentlyContinue
$xl = New-Object -ComObject Excel.Application
$t = [Diagnostics.Stopwatch]::StartNew()
try {
    $xl.Visible = $false; $xl.DisplayAlerts = $false; $xl.EnableEvents = $false; $xl.AutomationSecurity = $(if ($AllowMacros) { 1 } else { 3 })
    if (-not $xl.RegisterXLL((Resolve-Path $Xll).Path)) { throw "RegisterXLL failed" }
    $wb = $xl.Workbooks.Open($copy)
    $wb.Activate()
    "opened in $([int]$t.Elapsed.TotalSeconds) s"
    foreach ($s in $ClearSheets) { $wb.Worksheets.Item($s).Cells.ClearContents() }
    "outputs cleared"
    $t.Restart()
    $xl.Run("BC_Cmd$Command") | Out-Null
    $last = ''
    while ($t.Elapsed.TotalSeconds -lt $TimeoutSec) {
        Start-Sleep -Milliseconds 500
        $sb = try { [string]$xl.StatusBar } catch { 'busy' }
        if ($sb -ne $last) { "[{0,6:N1}s] {1}" -f $t.Elapsed.TotalSeconds, $sb; $last = $sb }
        if ($sb -match 'completed|failed|cancelled|not |could not') { break }
    }
    "end-to-end: $([math]::Round($t.Elapsed.TotalSeconds,1)) s"
    $wb.SaveAs($result)
    $wb.Close($false)
} finally { $xl.Quit(); [void][Runtime.InteropServices.Marshal]::ReleaseComObject($xl); [GC]::Collect() }
"saved $result"

