# Install the freshly built Arciin Desktop over whatever is installed now.
#
# Why this script exists
#
# Tauri's NSIS installer refuses to overwrite a binary that is in use, and
# under `/S` there is no prompt to show for it — it exits 0 having done
# nothing. The result is a silent no-op: the build succeeds, the install
# "succeeds", and the Start menu keeps launching last week's binary. That
# happened, and it cost real debugging time chasing behaviour that had already
# been fixed in code the running app did not contain.
#
# So: stop the app first, wait for it to actually be gone, then verify the
# installed file really changed.

$ErrorActionPreference = "Stop"

$repo = Split-Path -Parent $PSScriptRoot
$installer = Join-Path $repo "src-tauri\target\release\bundle\nsis\Arciin Desktop_0.1.0_x64-setup.exe"
$installed = Join-Path $env:LOCALAPPDATA "Arciin Desktop\arciin-desktop.exe"

if (-not (Test-Path $installer)) {
    throw "No installer at $installer. Run: npx tauri build"
}

# Stop every instance, wherever it was launched from: one started out of
# target\release holds the same lock as the installed copy.
#
# Closed politely first, and this matters more than it looks. WebView2 writes
# its cookie store to disk on shutdown, so killing the process outright throws
# away the signed-in session every single time — which turned every rebuild
# into "please sign in again", including for sessions the user had explicitly
# asked to be remembered. A force kill is the fallback, not the default.
$running = Get-Process arciin-desktop -ErrorAction SilentlyContinue
if ($running) {
    Write-Output ("Closing {0} running instance(s)..." -f $running.Count)
    & (Join-Path $PSScriptRoot "stop.ps1") | Out-Null

    $graceful = (Get-Date).AddSeconds(10)
    while ((Get-Process arciin-desktop -ErrorAction SilentlyContinue) -and (Get-Date) -lt $graceful) {
        Start-Sleep -Milliseconds 250
    }

    $stubborn = Get-Process arciin-desktop -ErrorAction SilentlyContinue
    if ($stubborn) {
        Write-Output "Did not close in time; forcing (the session may be lost)."
        $stubborn | Stop-Process -Force
    }
}

# Wait for the handles to close rather than assuming Stop-Process is
# instantaneous — that assumption is exactly what made this silent before.
$deadline = (Get-Date).AddSeconds(20)
while (Get-Process arciin-desktop -ErrorAction SilentlyContinue) {
    if ((Get-Date) -gt $deadline) {
        throw "Arciin Desktop is still running; installer would silently do nothing."
    }
    Start-Sleep -Milliseconds 200
}

Write-Output "Installing..."
$proc = Start-Process -FilePath $installer -ArgumentList "/S" -Wait -PassThru
if ($proc.ExitCode -ne 0) {
    throw "Installer exited with $($proc.ExitCode)."
}

# NSIS returns before the files have settled.
Start-Sleep -Seconds 2

if (-not (Test-Path $installed)) {
    throw "Installer finished but $installed does not exist."
}

# The check that catches the silent no-op: the installed binary must not be
# older than the installer that was just run. A reinstall of the same build
# legitimately leaves the file untouched, so comparing before/after would cry
# wolf; comparing against the installer catches the case that actually
# matters — a months-old binary still sitting there.
#
# Hashing against target\release would not work: Tauri patches that exe with
# NSIS bundle-type information on its way into the installer, so the built and
# installed files always differ by a few bytes.
$installerTime = (Get-Item $installer).LastWriteTimeUtc
$installedTime = (Get-Item $installed).LastWriteTimeUtc

if ($installedTime -lt $installerTime.AddMinutes(-1)) {
    throw ("Installed binary is older than the installer " +
           "(installed $installedTime, installer $installerTime). The install did nothing.")
}

Write-Output ("Installer: {0:yyyy-MM-dd HH:mm:ss} UTC" -f $installerTime)
Write-Output ("Installed: {0:yyyy-MM-dd HH:mm:ss} UTC  {1}" -f $installedTime, $installed)
Write-Output "OK: the installed binary is the current build."
