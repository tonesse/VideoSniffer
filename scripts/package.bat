@echo off
setlocal

set "PAUSE_ON_EXIT=1"
if /I "%~1"=="--no-pause" (
  set "PAUSE_ON_EXIT=0"
  shift
)

for %%I in ("%~f0") do set "SCRIPT_DIR=%%~dpI"
set "SCRIPT_PATH=%SCRIPT_DIR%package.ps1"
if not exist "%SCRIPT_PATH%" set "SCRIPT_PATH=%CD%\scripts\package.ps1"
if not exist "%SCRIPT_PATH%" set "SCRIPT_PATH=%CD%\package.ps1"
if not exist "%SCRIPT_PATH%" (
  echo Cannot find package.ps1.
  if "%PAUSE_ON_EXIT%"=="1" pause
  exit /b 1
)
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%SCRIPT_PATH%" %*
set "EXIT_CODE=%ERRORLEVEL%"

if not "%EXIT_CODE%"=="0" (
  echo.
  echo Package failed with exit code %EXIT_CODE%.
  if "%PAUSE_ON_EXIT%"=="1" pause
  exit /b %EXIT_CODE%
)

echo.
echo Package finished successfully.
if "%PAUSE_ON_EXIT%"=="1" pause
exit /b 0
