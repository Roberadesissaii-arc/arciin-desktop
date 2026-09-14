# Stop Arciin Desktop without losing the signed-in session.
#
# WebView2 flushes its cookie store on shutdown. `taskkill /F` skips that, so a
# forced stop silently discards the session — including a persistent one the
# user asked to be remembered. Every rebuild that killed the app was also
# signing the user out.
#
# `CloseMainWindow()` alone is not enough here: the process's "main" window is
# the hidden onboarding window, so WM_CLOSE goes to every top-level window it
# owns instead.

$ErrorActionPreference = "Stop"

Add-Type @'
using System;
using System.Runtime.InteropServices;
public class ArciinStop {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, System.Text.StringBuilder s, int n);
  public const uint WM_CLOSE = 0x0010;
}
'@

# Close the *Arciin* window specifically, not whichever window happens to be
# frontmost. The settings surface deliberately refuses to close while Arciin is
# running — closing it means "go back", not "quit" — so aiming at it stalls the
# shutdown and ends in a force kill, which is exactly what loses the session.
function Close-ArciinWindows {
    param([int] $ProcessId)

    $closedArciin = $false
    $cb = [ArciinStop+EnumProc] {
        param($handle, $lparam)
        $owner = 0
        [ArciinStop]::GetWindowThreadProcessId($handle, [ref]$owner) | Out-Null
        if ($owner -ne $ProcessId) { return $true }

        $sb = New-Object System.Text.StringBuilder 256
        [ArciinStop]::GetWindowText($handle, $sb, 256) | Out-Null
        if ($sb.ToString() -eq 'Arciin') {
            [ArciinStop]::PostMessage($handle, [ArciinStop]::WM_CLOSE, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
            $script:closedArciin = $true
        }
        return $true
    }
    [ArciinStop]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null

    # No Arciin window (never connected): closing onboarding does quit.
    if (-not $closedArciin) {
        $cb2 = [ArciinStop+EnumProc] {
            param($handle, $lparam)
            $owner = 0
            [ArciinStop]::GetWindowThreadProcessId($handle, [ref]$owner) | Out-Null
            if ($owner -eq $ProcessId) {
                [ArciinStop]::PostMessage($handle, [ArciinStop]::WM_CLOSE, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
            }
            return $true
        }
        [ArciinStop]::EnumWindows($cb2, [IntPtr]::Zero) | Out-Null
    }
}

$running = Get-Process arciin-desktop -ErrorAction SilentlyContinue
if (-not $running) {
    Write-Output "Not running."
    return
}

foreach ($proc in $running) { Close-ArciinWindows -ProcessId $proc.Id }

$deadline = (Get-Date).AddSeconds(12)
while ((Get-Process arciin-desktop -ErrorAction SilentlyContinue) -and (Get-Date) -lt $deadline) {
    Start-Sleep -Milliseconds 250
}

if (Get-Process arciin-desktop -ErrorAction SilentlyContinue) {
    Write-Output "Still running after 12s."
} else {
    Write-Output "Closed cleanly; session preserved."
}
