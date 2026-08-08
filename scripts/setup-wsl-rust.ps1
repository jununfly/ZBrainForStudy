# setup-wsl-rust.ps1 - Install WSL2 + Ubuntu + Rust toolchain on Windows 11
#
# This script runs the parts that require system-level tool access
# (wsl.exe / dism.exe). The Bash sandbox inside WorkBuddy cannot invoke
# those binaries, so this script must be run from a regular PowerShell
# terminal on the host machine, AS ADMIN if possible.
#
# Steps:
#   1. Enable WSL feature (Microsoft-Windows-Subsystem-Linux) - no reboot on Win11
#   2. Enable VirtualMachinePlatform feature (for WSL2)
#   3. Set WSL default version to 2
#   4. Install Ubuntu (latest LTS) - downloads ~500MB from Microsoft Store
#   5. Launch Ubuntu once to create the default user (interactive prompt)
#   6. Inside Ubuntu: run scripts/setup-wsl-rust-inner.sh
#
# Idempotency: re-running is safe; the script checks each step's
# current state and only runs what is missing.

#Requires -RunAsAdministrator
[CmdletBinding()]
param(
    [switch]$SkipUbuntuInstall,
    [switch]$SkipRustInstall,
    [string]$UbuntuDistro = "Ubuntu"
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "Continue"

function Write-Section {
    param([string]$Text)
    Write-Host ""
    Write-Host "===== $Text =====" -ForegroundColor Cyan
}

function Test-RebootPending {
    # Checks CBS / Windows Update reboot flags. If true, subsequent dism
    # /online /enable-feature calls will be blocked.
    $paths = @(
        "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\WindowsUpdate\Auto Update\RebootRequired",
        "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Component Based Servicing\RebootPending"
    )
    foreach ($p in $paths) {
        if (Test-Path $p) { return $true }
    }
    return $false
}

function Test-WSLFeature {
    param([string]$FeatureName)
    $f = Get-WindowsOptionalFeature -Online -FeatureName $FeatureName -ErrorAction SilentlyContinue
    return ($f.State -eq "Enabled")
}

function Test-DistroInstalled {
    param([string]$DistroName)
    $list = wsl --list --quiet 2>$null
    if ($LASTEXITCODE -ne 0) { return $false }
    return ($list -match [regex]::Escape($DistroName))
}

# ---- 0. Reboot-pending guard ----
Write-Section "Step 0/6: Check reboot-pending"
if (Test-RebootPending) {
    Write-Host "  Reboot is pending (Windows Update or CBS flag set)." -ForegroundColor Red
    Write-Host "  dism /online /enable-feature will be BLOCKED until you reboot." -ForegroundColor Red
    Write-Host "  Please reboot, then re-run this script." -ForegroundColor Red
    exit 2
}
Write-Host "  No pending reboot." -ForegroundColor Green

# ---- 1. WSL feature ----
Write-Section "Step 1/6: Enable WSL feature"
if (Test-WSLFeature "Microsoft-Windows-Subsystem-Linux") {
    Write-Host "  WSL feature already enabled." -ForegroundColor Green
} else {
    Write-Host "  Enabling Microsoft-Windows-Subsystem-Linux ..." -ForegroundColor Yellow
    dism.exe /online /enable-feature /featurename:Microsoft-Windows-Subsystem-Linux /all /norestart
    # dism exit codes:
    #   0    = SUCCESS
    #   3010 = SUCCESS_REBOOT_REQUIRED  (Windows 10 + some Win11 builds)
    #   740  = ELEVATION_REQUIRED  (re-run PowerShell as Administrator)
    #   0x800f0922 = missing dependency  (often: install media not mounted)
    if ($LASTEXITCODE -eq 0) {
        Write-Host "  WSL feature enabled." -ForegroundColor Green
    } elseif ($LASTEXITCODE -eq 3010) {
        Write-Host "  WSL feature enabled (REBOOT REQUIRED before step 3 will work)." -ForegroundColor Yellow
        Write-Host "  Save your work, then re-run this script after reboot." -ForegroundColor Yellow
        Write-Host "  Skipping remaining steps to avoid half-configured state." -ForegroundColor Yellow
        exit 0
    } else {
        throw "dism failed to enable WSL feature (exit $LASTEXITCODE). Try: (1) ensure PowerShell is run as Administrator; (2) check that no pending reboot is blocking (reboot then retry); (3) if 0x800f0922, run \`DISM /Online /Cleanup-Image /RestoreHealth\` then retry."
    }
}

# ---- 2. VirtualMachinePlatform ----
Write-Section "Step 2/6: Enable VirtualMachinePlatform (WSL2 backend)"
if (Test-WSLFeature "VirtualMachinePlatform") {
    Write-Host "  VirtualMachinePlatform already enabled." -ForegroundColor Green
} else {
    Write-Host "  Enabling VirtualMachinePlatform ..." -ForegroundColor Yellow
    dism.exe /online /enable-feature /featurename:VirtualMachinePlatform /all /norestart
    if ($LASTEXITCODE -eq 0) {
        Write-Host "  VirtualMachinePlatform enabled." -ForegroundColor Green
    } elseif ($LASTEXITCODE -eq 3010) {
        Write-Host "  VirtualMachinePlatform enabled (REBOOT REQUIRED before step 3 will work)." -ForegroundColor Yellow
        Write-Host "  Save your work, then re-run this script after reboot." -ForegroundColor Yellow
        exit 0
    } else {
        throw "dism failed to enable VirtualMachinePlatform (exit $LASTEXITCODE). Try as Administrator and after a reboot."
    }
}

# ---- 3. Default WSL version ----
Write-Section "Step 3/6: Set default WSL version to 2"
$currentDefault = (wsl --status 2>$null | Select-String "Default Version" | ForEach-Object { $_.ToString() })
if ($currentDefault -match "Default Version:\s*2") {
    Write-Host "  Default already 2." -ForegroundColor Green
} else {
    wsl --set-default-version 2
    if ($LASTEXITCODE -ne 0) {
        Write-Host "  WARNING: wsl --set-default-version 2 returned $LASTEXITCODE" -ForegroundColor Yellow
    } else {
        Write-Host "  Default set to WSL2." -ForegroundColor Green
    }
}

# ---- 4. Install Ubuntu ----
if (-not $SkipUbuntuInstall) {
    Write-Section "Step 4/6: Install $UbuntuDistro"
    if (Test-DistroInstalled $UbuntuDistro) {
        Write-Host "  $UbuntuDistro already installed." -ForegroundColor Green
    } else {
        Write-Host "  Downloading and installing $UbuntuDistro from Microsoft Store (~500MB) ..." -ForegroundColor Yellow
        wsl --install -d $UbuntuDistro --no-launch
        if ($LASTEXITCODE -ne 0) {
            throw "wsl --install failed (exit $LASTEXITCODE)"
        }
        Write-Host "  $UbuntuDistro installed." -ForegroundColor Green
    }
} else {
    Write-Host "  Skipped Ubuntu install (--SkipUbuntuInstall)." -ForegroundColor Yellow
}

# ---- 5. First-launch to set up user ----
Write-Section "Step 5/6: Initialize $UbuntuDistro user (interactive)"
Write-Host "  Launching $UbuntuDistro briefly to set the default UNIX user." -ForegroundColor Yellow
Write-Host "  When prompted, type a username + password (NOT your Windows credentials)." -ForegroundColor Yellow
Write-Host "  After the user is created and you see a prompt, type:  exit" -ForegroundColor Yellow
Write-Host "  Then re-run this script with --SkipUbuntuInstall to continue." -ForegroundColor Yellow
Write-Host ""
$answer = Read-Host "  Have you already created a $UbuntuDistro user? (y/n)"
if ($answer -ne "y") {
    wsl -d $UbuntuDistro -u root -- echo "Welcome. Please create your UNIX user via adduser, then exit. (We re-launched root because no default user exists yet.)"
    wsl -d $UbuntuDistro -u root
    Write-Host "  Re-run this script after creating the user to continue with Rust install." -ForegroundColor Cyan
    exit 0
}

# ---- 6. Inner Rust install ----
if (-not $SkipRustInstall) {
    Write-Section "Step 6/6: Install Rust toolchain inside $UbuntuDistro"
    $innerScript = "scripts/setup-wsl-rust-inner.sh"
    if (-not (Test-Path $innerScript)) {
        throw "Inner script not found: $innerScript (run this script from the ZBrain repo root)"
    }
    # Convert repo path to WSL-style (D:\workspace\... -> /mnt/d/workspace/...)
    $pwdWin = (Get-Location).Path -replace "\\", "/"
    $driveLetter = $pwdWin.Substring(0, 1).ToLower()
    $rest = $pwdWin.Substring(2)
    $pwdWsl = "/mnt/$driveLetter$rest"

    Write-Host "  Translating $pwdWin to $pwdWsl for WSL" -ForegroundColor Yellow
    wsl -d $UbuntuDistro -- bash -c "cd '$pwdWsl' && bash '$pwdWsl/$innerScript'"
    if ($LASTEXITCODE -ne 0) {
        throw "Inner Rust install failed (exit $LASTEXITCODE)"
    }
} else {
    Write-Host "  Skipped Rust install (--SkipRustInstall)." -ForegroundColor Yellow
}

Write-Section "Done"
Write-Host "  Try: wsl -d $UbuntuDistro -- cargo --version" -ForegroundColor Green
Write-Host "  Then: wsl -d $UbuntuDistro -- cd /mnt/d/.../ZBrain && cargo test -p zbrain-core --lib" -ForegroundColor Green
