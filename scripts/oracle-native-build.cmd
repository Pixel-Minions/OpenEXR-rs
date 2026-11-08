@echo off
rem SPDX-License-Identifier: BSD-3-Clause
rem Copyright Contributors to the OpenEXR Project.
rem
rem Build the source-built Core harness (PLAN.md O1, oracle/native) out of tree under
rem target\oracle-native\msvc-<config>, with the settings of scripts\upstream-build.cmd.
rem
rem Run it inside the MSVC environment:
rem   avr --non-interactive --run-as-tool --no-validation -p rez run rez-env msvc-14.44.35207 -- ^
rem     cmd /c scripts\oracle-native-build.cmd [Release^|Debug]

setlocal
set CONFIG=%1
if "%CONFIG%"=="" set CONFIG=Release
set ROOT=%~dp0..
set BUILD=%ROOT%\target\oracle-native\msvc-%CONFIG%
set CMAKE=uvx --from cmake==4.4.4 cmake
set NINJA_PKG=ninja==1.13.0

if not exist "%BUILD%" mkdir "%BUILD%"
uvx --from %NINJA_PKG% python -c "import ninja,os;print(os.path.join(ninja.BIN_DIR,'ninja.exe'))" > "%BUILD%\ninja-path.txt"
set /p NINJA=<"%BUILD%\ninja-path.txt"
if not exist "%NINJA%" (
  echo ninja not found: %NINJA%
  exit /b 1
)

%CMAKE% -S "%ROOT%\oracle\native" -B "%BUILD%" -G Ninja ^
  -DCMAKE_MAKE_PROGRAM="%NINJA%" ^
  -DCMAKE_BUILD_TYPE=%CONFIG% ^
  -DCMAKE_C_COMPILER=cl -DCMAKE_CXX_COMPILER=cl
if errorlevel 1 exit /b 1

%CMAKE% --build "%BUILD%" --target core_harness
exit /b %errorlevel%
