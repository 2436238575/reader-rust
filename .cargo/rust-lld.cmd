@echo off
rem Use rustup's bundled rust-lld instead of MSVC link.exe.
rem
rem Why: `cargo test` builds 40+ integration-test binaries, each linking the
rem whole crate rlib. link.exe needs 5-10s per binary, which dominates the test
rem build. lld links (and writes PDBs) much faster and ships with the toolchain.
rem
rem rustc detects lld from the linker file name and passes `-flavor link`.
rem Prefer the env vars injected by rustup so we do not run
rem `rustc --print sysroot` on every single link; fall back if missing.
rem
rem Keep this file ASCII-only: cmd.exe reads it in the OEM code page.

setlocal
if not defined RUSTUP_HOME set "RUSTUP_HOME=%USERPROFILE%\.rustup"

set "LLD="
if defined RUSTUP_TOOLCHAIN set "LLD=%RUSTUP_HOME%\toolchains\%RUSTUP_TOOLCHAIN%\lib\rustlib\x86_64-pc-windows-msvc\bin\rust-lld.exe"
if defined LLD if not exist "%LLD%" set "LLD="

if not defined LLD for /f "usebackq delims=" %%i in (`rustc --print sysroot`) do set "LLD=%%i\lib\rustlib\x86_64-pc-windows-msvc\bin\rust-lld.exe"

if not exist "%LLD%" (
  echo rust-lld not found; expected at "%LLD%" 1>&2
  exit /b 1
)

"%LLD%" %*
