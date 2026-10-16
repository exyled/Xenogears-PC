# Installs Xenogears for the current user: copies the program (and a GameData folder or Saves folder that sits beside this script) to the
# target folder, adds a Start menu shortcut and an uninstall script. No administrator rights are needed.
#   powershell -ExecutionPolicy Bypass -File install.ps1 [-Target <folder>] [-NoShortcut]
param(
    [string]$Target = (Join-Path $env:LOCALAPPDATA "Programs\Xenogears"),
    [switch]$NoShortcut
)
$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$exe = Join-Path $here "Xenogears.exe"
if (-not (Test-Path $exe)) { throw "Xenogears.exe is not beside this script ($here)." }
New-Item -ItemType Directory -Force $Target | Out-Null
Copy-Item $exe (Join-Path $Target "Xenogears.exe") -Force
foreach ($d in "GameData", "Saves", "TextureReplace") {
    $src = Join-Path $here $d
    if (Test-Path $src) { Copy-Item $src (Join-Path $Target $d) -Recurse -Force }
}
$readme = Join-Path $here "README.txt"
if (Test-Path $readme) { Copy-Item $readme (Join-Path $Target "README.txt") -Force }
$link = Join-Path ([Environment]::GetFolderPath("Programs")) "Xenogears.lnk"
if (-not $NoShortcut) {
    $sh = New-Object -ComObject WScript.Shell
    $s = $sh.CreateShortcut($link)
    $s.TargetPath = Join-Path $Target "Xenogears.exe"
    $s.WorkingDirectory = $Target
    $s.Save()
}
# the way back: removes the program and the shortcut, and leaves the saves and the game data alone
$un = @"
`$ErrorActionPreference = "Stop"
Remove-Item "$link" -ErrorAction SilentlyContinue
Remove-Item "$Target\Xenogears.exe", "$Target\README.txt" -ErrorAction SilentlyContinue
Write-Output "Xenogears removed. The Saves and GameData folders in $Target were left; delete them by hand if you do not want them."
"@
Set-Content -Path (Join-Path $Target "uninstall.ps1") -Value $un -Encoding UTF8
Write-Output "Installed to $Target"
if (-not (Test-Path (Join-Path $Target "GameData"))) {
    Write-Output "The game data is not there yet: run  Xenogears.exe --extract `"Disc 1.bin`" `"Disc 2.bin`"  in that folder once."
}
