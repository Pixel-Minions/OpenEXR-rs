@echo off
rem SPDX-License-Identifier: BSD-3-Clause
rem Copyright Contributors to the OpenEXR Project.
rem
rem Build upstream OpenEXR (OpenEXRCore, OpenEXR, OpenEXRCoreTest) from the pinned submodule,
rem out of tree under target\upstream-build\<config>, with the dependency settings of the
rem official wheel (upstream pyproject.toml [tool.scikit-build.cmake.define]): vendored
rem libdeflate, zstd and OpenJPH, Imath fetched at OPENEXR_IMATH_TAG, static libraries.
rem
rem Run it inside the MSVC environment:
rem   avr --non-interactive --run-as-tool --no-validation -p rez run rez-env msvc-14.44.35207 -- ^
rem     cmd /c scripts\upstream-build.cmd [Release^|Debug] [ctest-args...]
rem
rem cmake and ninja come from PyPI through uvx, pinned here.

setlocal
set CONFIG=%1
if "%CONFIG%"=="" set CONFIG=Release
set ROOT=%~dp0..
set SRC=%ROOT%\upstream\openexr
set BUILD=%ROOT%\target\upstream-build\msvc-%CONFIG%
set CMAKE=uvx --from cmake==4.4.4 cmake
set CTEST=uvx --from cmake==4.4.4 ctest
set NINJA_PKG=ninja==1.13.0

if not exist "%BUILD%" mkdir "%BUILD%"
uvx --from %NINJA_PKG% python -c "import ninja,os;print(os.path.join(ninja.BIN_DIR,'ninja.exe'))" > "%BUILD%\ninja-path.txt"
set /p NINJA=<"%BUILD%\ninja-path.txt"
if not exist "%NINJA%" (
  echo ninja not found: %NINJA%
  exit /b 1
)

%CMAKE% -S "%SRC%" -B "%BUILD%" -G Ninja ^
  -DCMAKE_MAKE_PROGRAM="%NINJA%" ^
  -DCMAKE_BUILD_TYPE=%CONFIG% ^
  -DCMAKE_C_COMPILER=cl -DCMAKE_CXX_COMPILER=cl ^
  -DBUILD_SHARED_LIBS=OFF ^
  -DCMAKE_POSITION_INDEPENDENT_CODE=ON ^
  -DOPENEXR_INSTALL=OFF ^
  -DOPENEXR_INSTALL_PKG_CONFIG=OFF ^
  -DOPENEXR_BUILD_TOOLS=OFF ^
  -DOPENEXR_BUILD_EXAMPLES=OFF ^
  -DOPENEXR_BUILD_PYTHON=OFF ^
  -DOPENEXR_FORCE_INTERNAL_DEFLATE=ON ^
  -DOPENEXR_FORCE_INTERNAL_IMATH=ON ^
  -DOPENEXR_FORCE_INTERNAL_OPENJPH=ON ^
  -DOPENEXR_FORCE_INTERNAL_ZSTD=ON ^
  -DBUILD_TESTING=ON ^
  -DOPENEXR_TEST_LIBRARIES=ON ^
  -DOPENEXR_TEST_TOOLS=OFF ^
  -DOPENEXR_TEST_PYTHON=OFF
if errorlevel 1 exit /b 1

%CMAKE% --build "%BUILD%" --target OpenEXRCoreTest
if errorlevel 1 exit /b 1

shift
%CTEST% --test-dir "%BUILD%" -L OpenEXRCore --output-on-failure --timeout 1800 %1 %2 %3 %4 %5 %6 %7 %8 %9
exit /b %errorlevel%
