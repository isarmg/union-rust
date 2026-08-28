@echo off
setlocal EnableExtensions DisableDelayedExpansion

if "%~4"=="" goto :usage
if not "%~5"=="" goto :usage

set "PRODUCT_VERSION=%~1"
set "AGENT_EXE=%~f2"
set "MAINTENANCE_EXE=%~f3"
set "TRAY_EXE=%~f4"
set "SCRIPT_ROOT=%~dp0"

if not exist "%AGENT_EXE%" (
  echo Agent executable not found: "%AGENT_EXE%" 1>&2
  exit /b 2
)
if not exist "%MAINTENANCE_EXE%" (
  echo Maintenance executable not found: "%MAINTENANCE_EXE%" 1>&2
  exit /b 2
)
if not exist "%TRAY_EXE%" (
  echo Tray executable not found: "%TRAY_EXE%" 1>&2
  exit /b 2
)

set "DETECTED_AGENT_VERSION="
for /f "delims=" %%V in ('"%AGENT_EXE%" --version') do set "DETECTED_AGENT_VERSION=%%V"
if not "%DETECTED_AGENT_VERSION%"=="unionc-agent %PRODUCT_VERSION%" (
  echo Product version %PRODUCT_VERSION% does not match Agent executable version "%DETECTED_AGENT_VERSION%". 1>&2
  exit /b 2
)

rem The project repeats the exact binary/version check so direct MSBuild callers cannot bypass it.
dotnet build "%SCRIPT_ROOT%UnionCAgent.Installer.wixproj" ^
  --configuration Release ^
  --nologo ^
  -p:ProductVersion="%PRODUCT_VERSION%" ^
  -p:AgentExe="%AGENT_EXE%" ^
  -p:MaintenanceExe="%MAINTENANCE_EXE%" ^
  -p:TrayExe="%TRAY_EXE%"
if errorlevel 1 exit /b %errorlevel%

echo MSI created below "%SCRIPT_ROOT%bin\x64\Release".
exit /b 0

:usage
echo Usage: build-msi.cmd VERSION AGENT_EXE MAINTENANCE_EXE TRAY_EXE 1>&2
echo Example: build-msi.cmd 0.5.0 ..\..\..\target\x86_64-pc-windows-msvc\release\unionc-agent.exe ..\..\..\target\x86_64-pc-windows-msvc\release\unionc-agent-maintenance.exe ..\..\..\target\x86_64-pc-windows-msvc\release\unionc-agent-tray.exe 1>&2
exit /b 2
