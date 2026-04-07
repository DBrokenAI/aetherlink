@echo off
REM AetherLink one-click installer.
REM Double-click this file to install AetherLink for the current user.
REM
REM Works in two scenarios:
REM   1. End-user distribution: drop install.bat next to aetherlink.exe
REM      and double-click. Both files in the same folder, no other setup.
REM   2. Developer use: run from the project root after `cargo build --release`.
REM      The script will find target\release\aetherlink.exe automatically.

setlocal enabledelayedexpansion

echo ================================================================
echo   AetherLink Installer
echo ================================================================
echo.

REM ---- Locate the binary -------------------------------------------
REM Prefer aetherlink.exe sitting next to this .bat file (the
REM distribution case). Fall back to target\release\aetherlink.exe
REM (the developer-from-source case).

set "SCRIPT_DIR=%~dp0"
set "SOURCE_EXE="

if exist "%SCRIPT_DIR%aetherlink.exe" (
    set "SOURCE_EXE=%SCRIPT_DIR%aetherlink.exe"
    echo Found binary next to installer: !SOURCE_EXE!
) else if exist "%SCRIPT_DIR%target\release\aetherlink.exe" (
    set "SOURCE_EXE=%SCRIPT_DIR%target\release\aetherlink.exe"
    echo Found release build: !SOURCE_EXE!
) else (
    echo ERROR: Could not find aetherlink.exe.
    echo.
    echo Looked in:
    echo   %SCRIPT_DIR%aetherlink.exe
    echo   %SCRIPT_DIR%target\release\aetherlink.exe
    echo.
    echo If you are a developer, run `cargo build --release` first.
    echo If you received this from a friend, make sure aetherlink.exe is
    echo in the same folder as install.bat.
    echo.
    pause
    exit /b 1
)
echo.

REM ---- Pick an install location ------------------------------------
REM Use %LOCALAPPDATA%\AetherLink — no admin needed, survives reboots,
REM stays out of Program Files (which would require elevation).

set "INSTALL_DIR=%LOCALAPPDATA%\AetherLink"
set "INSTALL_EXE=%INSTALL_DIR%\aetherlink.exe"

echo Installing to: %INSTALL_DIR%

if not exist "%INSTALL_DIR%" (
    mkdir "%INSTALL_DIR%"
    if errorlevel 1 (
        echo ERROR: Could not create %INSTALL_DIR%
        pause
        exit /b 1
    )
)

REM ---- Copy the binary ---------------------------------------------
copy /Y "%SOURCE_EXE%" "%INSTALL_EXE%" >nul
if errorlevel 1 (
    echo ERROR: Could not copy aetherlink.exe to %INSTALL_EXE%
    echo The file may be in use. Quit Claude Desktop and try again.
    pause
    exit /b 1
)
echo Copied aetherlink.exe to install location.
echo.

REM ---- Register with Claude Desktop --------------------------------
echo Running registration...
echo ----------------------------------------------------------------
"%INSTALL_EXE%" --register
if errorlevel 1 (
    echo.
    echo ERROR: --register failed. See the message above.
    pause
    exit /b 1
)

REM ---- Register with Claude Code (CLI) -----------------------------
REM Claude Desktop and Claude Code use separate MCP configs. The Rust
REM --register flow only touches Desktop's config, so do Code here.
REM Skipped silently if the `claude` CLI isn't installed — Desktop users
REM shouldn't see a scary error for a tool they don't have.

where claude >nul 2>nul
if %errorlevel%==0 (
    echo Registering with Claude Code CLI...
    REM -s user = available across all projects for this user.
    REM Re-running install should refresh the path, so remove any stale
    REM entry first and ignore failure if none exists.
    REM `claude` is a .cmd shim — must use `call`, otherwise control
    REM transfers to it and this script exits before reaching `pause`,
    REM closing the window before the user can read the output.
    call claude mcp remove aetherlink -s user >nul 2>nul
    call claude mcp add aetherlink -s user -- "%INSTALL_EXE%"
    if errorlevel 1 (
        echo WARNING: claude mcp add failed. AetherLink will still work
        echo in Claude Desktop, but you'll need to register it manually
        echo for Claude Code:
        echo   claude mcp add aetherlink -s user -- "%INSTALL_EXE%"
    ) else (
        echo Registered with Claude Code as MCP server 'aetherlink'.
    )
) else (
    echo Claude Code CLI not detected on PATH; skipping its registration.
    echo If you use Claude Code, install it and then run:
    echo   claude mcp add aetherlink -s user -- "%INSTALL_EXE%"
)
echo.

REM ---- Done --------------------------------------------------------
echo.
echo ================================================================
echo   Installation complete.
echo.
echo   Binary location: %INSTALL_EXE%
echo.
echo   Next: fully QUIT Claude Desktop (right-click the system tray
echo   icon -^> Quit) and reopen it. AetherLink will be active.
echo ================================================================
echo.
pause
endlocal
