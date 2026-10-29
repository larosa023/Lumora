# Downloads the hardware engine that ships inside Lumora's Windows build:
#   - OpenRGB (GPL-2.0), unmodified, from its official Codeberg release
#   - the PawnIO driver installer, used only if you turn on RAM & motherboard lighting
#   - the UsbDk driver installer, used only if you set up a Corsair Hydro Pro cooler
# Run from the repo root before `npm run build` on Windows (CI does this for you):
#   powershell -ExecutionPolicy Bypass -File scripts/fetch-engine.ps1
param(
  [string]$OpenRgbVersion = '1.1',
  [string]$OpenRgbBuild = '8e16de2'
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$engine = Join-Path $PSScriptRoot '..\src-tauri\engine'
if (Test-Path $engine) { Remove-Item $engine -Recurse -Force }
New-Item -ItemType Directory -Path $engine | Out-Null
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("lumora-engine-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null

# OpenRGB
$zipName = "OpenRGB_${OpenRgbVersion}_Windows_64_${OpenRgbBuild}.zip"
$url = "https://codeberg.org/OpenRGB/OpenRGB/releases/download/release_${OpenRgbVersion}/$zipName"
Write-Host "Downloading $url"
Invoke-WebRequest -Uri $url -OutFile (Join-Path $tmp $zipName)
Expand-Archive -Path (Join-Path $tmp $zipName) -DestinationPath (Join-Path $tmp 'openrgb')
$exe = Get-ChildItem -Path (Join-Path $tmp 'openrgb') -Recurse -Filter 'OpenRGB.exe' | Select-Object -First 1
if (-not $exe) { throw 'OpenRGB.exe was not found in the download' }
Copy-Item -Path $exe.Directory.FullName -Destination (Join-Path $engine 'openrgb') -Recurse
Set-Content -Path (Join-Path $engine 'VERSION') -Value "$OpenRgbVersion-$OpenRgbBuild" -NoNewline

# PawnIO driver installer (latest release)
$release = Invoke-RestMethod -Uri 'https://api.github.com/repos/namazso/PawnIO.Setup/releases/latest' -Headers @{ 'User-Agent' = 'lumora-build' }
$asset = $release.assets | Where-Object { $_.name -eq 'PawnIO_setup.exe' } | Select-Object -First 1
if (-not $asset) { throw 'PawnIO_setup.exe was not found in the latest PawnIO release' }
Write-Host "Downloading PawnIO $($release.tag_name)"
Invoke-WebRequest -Uri $asset.browser_download_url -OutFile (Join-Path $engine 'PawnIO_setup.exe')

# UsbDk driver installer (Apache-2.0), for Corsair Hydro Pro coolers
$usbdk = Invoke-RestMethod -Uri 'https://api.github.com/repos/daynix/UsbDk/releases/latest' -Headers @{ 'User-Agent' = 'lumora-build' }
$asset = $usbdk.assets | Where-Object { $_.name -match '_x64\.msi(\.zip)?$' } | Select-Object -First 1
if (-not $asset) { throw 'No x64 installer was found in the latest UsbDk release' }
Write-Host "Downloading UsbDk $($usbdk.tag_name) ($($asset.name))"
$download = Join-Path $tmp $asset.name
Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $download
if ($asset.name -like '*.zip') {
  Expand-Archive -Path $download -DestinationPath (Join-Path $tmp 'usbdk')
  $download = (Get-ChildItem -Path (Join-Path $tmp 'usbdk') -Recurse -Filter '*.msi' | Select-Object -First 1).FullName
}
Copy-Item $download (Join-Path $engine 'UsbDk_x64.msi')

# Licenses travel with the binaries.
$license = Get-ChildItem -Path (Join-Path $engine 'openrgb') -Recurse -Include 'LICENSE*', 'COPYING*' | Select-Object -First 1
if (-not $license) {
  Invoke-WebRequest -Uri "https://codeberg.org/OpenRGB/OpenRGB/raw/tag/release_${OpenRgbVersion}/LICENSE" -OutFile (Join-Path $engine 'openrgb\LICENSE')
}
Copy-Item (Join-Path $PSScriptRoot '..\THIRD_PARTY.md') (Join-Path $engine 'THIRD_PARTY.md')

Remove-Item $tmp -Recurse -Force
$size = (Get-ChildItem $engine -Recurse | Measure-Object -Property Length -Sum).Sum / 1MB
Write-Host ("Engine ready in src-tauri/engine ({0:N1} MB)" -f $size)
