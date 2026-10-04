# Import the MSVC build environment into the current PowerShell process.
#
#   . .\scripts\msvc-env.ps1
#
# Why this file exists
# --------------------
# `rustc` finds the MSVC linker through the Visual Studio SxS registry keys
# (HKLM\SOFTWARE\Microsoft\VisualStudio\SxS\VS7 and ...\VC7). A full Visual
# Studio install normally publishes them. Some installs do not - notably
# Visual Studio 18.x, which can be present and complete on disk while those
# keys are absent. The result is a confusing failure:
#
#   error: linker `link.exe` not found
#   note: program not found
#   note: the msvc targets depend on the msvc linker but `link.exe` was not found
#
# ...even though link.exe is sitting in the toolset directory. Importing
# vcvars64.bat supplies the PATH, LIB and INCLUDE that the compiler and linker
# actually need, and works regardless of registry state.
#
# This is a developer convenience only. It is not needed to build if your
# Visual Studio install is correctly registered.

$ErrorActionPreference = 'Stop'

function Import-MsvcEnv {
    $vcvars = Get-ChildItem "$env:ProgramFiles\Microsoft Visual Studio" `
        -Filter 'vcvars64.bat' -Recurse -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -like '*\VC\Auxiliary\Build\vcvars64.bat' } |
        Sort-Object FullName -Descending |
        Select-Object -First 1

    if (-not $vcvars) {
        throw 'vcvars64.bat not found. Install Visual Studio Build Tools with the "Desktop development with C++" workload.'
    }

    $lines = & cmd.exe /c "call `"$($vcvars.FullName)`" >nul 2>&1 && set"
    if ($LASTEXITCODE -ne 0) {
        throw "vcvars64.bat failed with exit code $LASTEXITCODE"
    }

    $applied = 0
    foreach ($line in $lines) {
        if ($line -match '^([^=]+)=(.*)$') {
            [Environment]::SetEnvironmentVariable($matches[1], $matches[2], 'Process')
            $applied++
        }
    }

    Write-Host "MSVC env applied from $($vcvars.FullName) ($applied vars)" -ForegroundColor DarkGray
}

Import-MsvcEnv

# Rust's own toolchain, for convenience.
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
