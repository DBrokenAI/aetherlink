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

REM ---- Register Claude Code PreToolUse hooks -----------------------
REM This is the part that turns AetherLink from "MCP server the agent
REM might call" into a hard guardrail. We add a PreToolUse hook for
REM Edit / Write / MultiEdit that pipes the tool payload into
REM `aetherlink --hook-check`. The hook exits 2 to block illegal
REM writes; Claude Code surfaces stderr back to the model. The agent
REM physically cannot bypass this — even if it has no idea AetherLink
REM exists, every write goes through it.
REM
REM We merge into ~/.claude/settings.json via PowerShell so we don't
REM clobber the user's existing settings or other hooks.

echo Installing Claude Code PreToolUse hooks...
powershell -NoProfile -ExecutionPolicy Bypass -Command ^
    "$cfgDir = Join-Path $env:USERPROFILE '.claude';" ^
    "if (-not (Test-Path $cfgDir)) { New-Item -ItemType Directory -Force -Path $cfgDir | Out-Null };" ^
    "$cfg = Join-Path $cfgDir 'settings.json';" ^
    "if (Test-Path $cfg) { $obj = Get-Content -Raw $cfg | ConvertFrom-Json } else { $obj = [pscustomobject]@{} };" ^
    "if (-not $obj.PSObject.Properties.Match('hooks').Count) { $obj | Add-Member -NotePropertyName hooks -NotePropertyValue ([pscustomobject]@{}) };" ^
    "$exe = '%INSTALL_EXE%' -replace '\\','/';" ^
    "$entry = [pscustomobject]@{ matcher = 'Edit|Write|MultiEdit'; hooks = @(@{ type = 'command'; command = ($exe + ' --hook-check') }) };" ^
    "$obj.hooks | Add-Member -NotePropertyName PreToolUse -NotePropertyValue @($entry) -Force;" ^
    "$obj | ConvertTo-Json -Depth 10 | Set-Content -Path $cfg -Encoding UTF8;" ^
    "Write-Host 'Installed PreToolUse hook for Edit/Write/MultiEdit.'"
if errorlevel 1 (
    echo WARNING: hook installation failed. AetherLink will still work as
    echo an MCP server, but agents that use built-in Edit/Write tools will
    echo bypass the rules. Add this to ~/.claude/settings.json manually:
    echo   "hooks": { "PreToolUse": [ { "matcher": "Edit^|Write^|MultiEdit",
    echo     "hooks": [ { "type": "command", "command": "%INSTALL_EXE% --hook-check" } ] } ] }
)
echo.

REM ---- Register with Cursor (MCP) ----------------------------------
REM Cursor stores MCP servers in %USERPROFILE%\.cursor\mcp.json. We
REM merge our entry into that file via PowerShell so we don't clobber
REM whatever the user already has registered. Skipped silently if
REM Cursor is not installed.

if exist "%USERPROFILE%\.cursor" (
    echo Registering with Cursor...
    powershell -NoProfile -ExecutionPolicy Bypass -Command ^
        "$cfg = Join-Path $env:USERPROFILE '.cursor\mcp.json';" ^
        "if (Test-Path $cfg) { $obj = Get-Content -Raw $cfg | ConvertFrom-Json } else { $obj = [pscustomobject]@{} };" ^
        "if (-not $obj.PSObject.Properties.Match('mcpServers').Count) { $obj | Add-Member -NotePropertyName mcpServers -NotePropertyValue ([pscustomobject]@{}) };" ^
        "$entry = [pscustomobject]@{ command = '%INSTALL_EXE%'; args = @() };" ^
        "if ($obj.mcpServers.PSObject.Properties.Match('aetherlink').Count) { $obj.mcpServers.aetherlink = $entry } else { $obj.mcpServers | Add-Member -NotePropertyName aetherlink -NotePropertyValue $entry };" ^
        "$obj | ConvertTo-Json -Depth 10 | Set-Content -Path $cfg -Encoding UTF8;" ^
        "Write-Host 'Registered with Cursor as MCP server aetherlink.'"
    if errorlevel 1 (
        echo WARNING: Cursor registration failed. Add it manually by editing
        echo %USERPROFILE%\.cursor\mcp.json and adding under mcpServers:
        echo   "aetherlink": { "command": "%INSTALL_EXE%", "args": [] }
    )
) else (
    echo Cursor not detected; skipping its registration.
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
