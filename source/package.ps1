# Builds the release executable, puts it beside the game data, and writes the source zip (no target folder, no game data).
$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$env:Path += ";$env:USERPROFILE\.cargo\bin"
Push-Location $here
cargo build --release
if ($LASTEXITCODE -ne 0) { Pop-Location; exit 1 }
Pop-Location
$exe = Join-Path $here "target\release\xenogears.exe"
$dest = "C:\Users\exyle\Xenogears\Xenogears.exe"
Copy-Item $exe $dest -Force
# the ready-to-run game folder (program + Xenogears.dat + Saves): the data is private and never goes into the zip
$dist = "C:\Users\exyle\Xenogears\Dist"
New-Item -ItemType Directory -Force (Join-Path $dist "Saves") | Out-Null
Copy-Item $exe (Join-Path $dist "Xenogears.exe") -Force
Copy-Item (Join-Path $here "install.ps1") $dist -Force
if (Test-Path "C:\Users\exyle\Xenogears\Xenogears.dat") { Copy-Item "C:\Users\exyle\Xenogears\Xenogears.dat" $dist -Force }
$bad = Get-ChildItem -Recurse -File (Join-Path $here "src"), (Join-Path $here "Cargo.toml"), (Join-Path $here "README.md") |
    Select-String -Pattern "ps1|psx|playstation|sony" -CaseSensitive:$false
if ($bad) { $bad | ForEach-Object { Write-Output ("name check: " + $_.Path + ":" + $_.LineNumber) }; exit 1 }
$stamp = Get-Date -Format "yyyy-MM-dd_HHmm"
$zip = "C:\Users\exyle\Downloads\Xenogears-native_$stamp.zip"
$stage = Join-Path $env:TEMP ("xg-native-" + $stamp)
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item (Join-Path $here "src") $stage -Recurse
Copy-Item (Join-Path $here "Cargo.toml"), (Join-Path $here "Cargo.lock"), (Join-Path $here "README.md"), (Join-Path $here "package.ps1"), (Join-Path $here "install.ps1") $stage
Copy-Item $exe (Join-Path $stage "Xenogears.exe")
Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $zip -Force
Remove-Item $stage -Recurse -Force
Write-Output ("exe: " + $dest + " (" + [math]::Round((Get-Item $dest).Length / 1KB) + " KB)")
Write-Output ("zip: " + $zip)
