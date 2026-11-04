@echo off
rem Drag a slow Excel workbook onto this file. It is opened READ-ONLY, macros disabled,
rem nothing is saved. Result: <workbook>.profile.json next to it (no cell values inside).
if "%~1"=="" (echo Drag an Excel file onto PROFILE_WORKBOOK.cmd & pause & exit /b 1)
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0Profile-Workbook.ps1" -Path "%~1" -TimeSheets
pause

