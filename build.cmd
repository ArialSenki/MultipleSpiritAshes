@echo off
rem Builds the built-in Multiple Spirit Ashes static library (and runs its
rem tests with "build.cmd test"). "build.cmd standalone" builds the standalone
rem MultipleSpiritAshes.dll from standalone\ ("standalone-test" runs its tests).
rem Rust: cargo from PATH, or a portable install with rustup\ and cargo\ in
rem NT_RUST_HOME; MSVC 2022. CMake calls this before linking NativeTweaks.dll.
rem Set NT_CARGO_OFFLINE=1 to build only from crates already downloaded.
setlocal
if not "%NT_RUST_HOME%"=="" (
  set "RUSTUP_HOME=%NT_RUST_HOME%\rustup"
  set "CARGO_HOME=%NT_RUST_HOME%\cargo"
  set "PATH=%NT_RUST_HOME%\cargo\bin;%PATH%"
)
set "OFFLINE="
if "%NT_CARGO_OFFLINE%"=="1" set "OFFLINE=--offline"
if "%NT_MSA_TARGET_DIR%"=="" set "NT_MSA_TARGET_DIR=%LOCALAPPDATA%\NativeTweaks-msa-build"
rem The standalone build.rs hands the VERSIONINFO .res path to the linker
rem verbatim, so this target directory must not contain spaces.
if "%NT_MSA_STANDALONE_TARGET_DIR%"=="" set "NT_MSA_STANDALONE_TARGET_DIR=%LOCALAPPDATA%\MultipleSpiritAshes-standalone-build"
cd /d "%~dp0"
if /i "%1"=="test" (
  cargo test --release %OFFLINE% --target x86_64-pc-windows-msvc --target-dir "%NT_MSA_TARGET_DIR%" --lib
  exit /b %errorlevel%
)
if /i "%1"=="standalone" (
  cd /d "%~dp0standalone"
  cargo build --release %OFFLINE% --target x86_64-pc-windows-msvc --target-dir "%NT_MSA_STANDALONE_TARGET_DIR%"
  exit /b %errorlevel%
)
if /i "%1"=="standalone-test" (
  cd /d "%~dp0standalone"
  cargo test --release %OFFLINE% --target x86_64-pc-windows-msvc --target-dir "%NT_MSA_STANDALONE_TARGET_DIR%" --lib
  exit /b %errorlevel%
)
cargo build --release %OFFLINE% --target x86_64-pc-windows-msvc --target-dir "%NT_MSA_TARGET_DIR%"
exit /b %errorlevel%
