# Downloads the hardware engine that ships inside Lumora's Windows build:
#   - OpenRGB (GPL-2.0), unmodified, from its official Codeberg release
#   - the PawnIO driver installer, used only if you turn on RAM & motherboard lighting
#   - the UsbDk driver installer, used only if you set up a Corsair Hydro Pro cooler
#   - PawnIO's signed CPU sensor modules, for the CPU temperature helper
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

# Downloads retry a few times; CI networks hiccup. GitHub API calls use the
# workflow token when there is one, to avoid the anonymous rate limit.
$apiHeaders = @{ 'User-Agent' = 'lumora-build' }
if ($env:GITHUB_TOKEN) { $apiHeaders['Authorization'] = "Bearer $env:GITHUB_TOKEN" }
function Retry([scriptblock]$Action, [string]$What) {
  for ($i = 1; $i -le 4; $i++) {
    try { return & $Action }
    catch {
      if ($i -eq 4) { throw "$What failed: $_" }
      Write-Host "$What failed (attempt $i): $_. Retrying..."
      Start-Sleep -Seconds (5 * $i)
    }
  }
}
New-Item -ItemType Directory -Path $tmp | Out-Null

# OpenRGB
$zipName = "OpenRGB_${OpenRgbVersion}_Windows_64_${OpenRgbBuild}.zip"
$url = "https://codeberg.org/OpenRGB/OpenRGB/releases/download/release_${OpenRgbVersion}/$zipName"
Write-Host "Downloading $url"
Retry { Invoke-WebRequest -Uri $url -OutFile (Join-Path $tmp $zipName) } 'OpenRGB download'
Expand-Archive -Path (Join-Path $tmp $zipName) -DestinationPath (Join-Path $tmp 'openrgb')
$exe = Get-ChildItem -Path (Join-Path $tmp 'openrgb') -Recurse -Filter 'OpenRGB.exe' | Select-Object -First 1
if (-not $exe) { throw 'OpenRGB.exe was not found in the download' }
Copy-Item -Path $exe.Directory.FullName -Destination (Join-Path $engine 'openrgb') -Recurse
Set-Content -Path (Join-Path $engine 'VERSION') -Value "$OpenRgbVersion-$OpenRgbBuild" -NoNewline

# PawnIO driver installer (latest release)
$release = Retry { Invoke-RestMethod -Uri 'https://api.github.com/repos/namazso/PawnIO.Setup/releases/latest' -Headers $apiHeaders } 'PawnIO release lookup'
$asset = $release.assets | Where-Object { $_.name -eq 'PawnIO_setup.exe' } | Select-Object -First 1
if (-not $asset) { throw 'PawnIO_setup.exe was not found in the latest PawnIO release' }
Write-Host "Downloading PawnIO $($release.tag_name)"
Retry { Invoke-WebRequest -Uri $asset.browser_download_url -OutFile (Join-Path $engine 'PawnIO_setup.exe') } 'PawnIO download'

# UsbDk driver installer (Apache-2.0), for Corsair Hydro Pro coolers
$usbdk = Retry { Invoke-RestMethod -Uri 'https://api.github.com/repos/daynix/UsbDk/releases/latest' -Headers $apiHeaders } 'UsbDk release lookup'
$asset = $usbdk.assets | Where-Object { $_.name -match '_x64\.msi(\.zip)?$' } | Select-Object -First 1
if (-not $asset) { throw 'No x64 installer was found in the latest UsbDk release' }
Write-Host "Downloading UsbDk $($usbdk.tag_name) ($($asset.name))"
$download = Join-Path $tmp $asset.name
Retry { Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $download } 'UsbDk download'
if ($asset.name -like '*.zip') {
  Expand-Archive -Path $download -DestinationPath (Join-Path $tmp 'usbdk')
  $download = (Get-ChildItem -Path (Join-Path $tmp 'usbdk') -Recurse -Filter '*.msi' | Select-Object -First 1).FullName
}
Copy-Item $download (Join-Path $engine 'UsbDk_x64.msi')

# PawnIO's signed modules for reading CPU temperature (AMD Ryzen, Intel).
# The helper itself (lumora-sensors.exe) is built from this repo by CI.
$sensors = Join-Path $engine 'sensors'
New-Item -ItemType Directory -Force -Path $sensors | Out-Null
try {
  $mods = Retry { Invoke-RestMethod -Uri 'https://api.github.com/repos/namazso/PawnIO.Modules/releases/latest' -Headers $apiHeaders } 'PawnIO modules lookup'
  $wanted = @('AMDFamily17.bin', 'IntelMSR.bin')
  foreach ($a in $mods.assets) {
    if ($wanted -contains $a.name) {
      Retry { Invoke-WebRequest -Uri $a.browser_download_url -OutFile (Join-Path $sensors $a.name) } "$($a.name) download"
    } elseif ($a.name -like '*.zip') {
      $zip = Join-Path $tmp $a.name
      Retry { Invoke-WebRequest -Uri $a.browser_download_url -OutFile $zip } "$($a.name) download"
      $out = Join-Path $tmp ('mods-' + [IO.Path]::GetFileNameWithoutExtension($a.name))
      Expand-Archive -Path $zip -DestinationPath $out -Force
      foreach ($w in $wanted) {
        $f = Get-ChildItem -Path $out -Recurse -Filter $w | Select-Object -First 1
        if ($f -and -not (Test-Path (Join-Path $sensors $w))) { Copy-Item $f.FullName (Join-Path $sensors $w) }
      }
    }
  }
  foreach ($w in $wanted) {
    if (Test-Path (Join-Path $sensors $w)) { Write-Host "PawnIO module $w ($($mods.tag_name))" }
    else { Write-Warning "PawnIO module $w not found; CPU temperature won't be available for that CPU maker" }
  }
} catch {
  Write-Warning "PawnIO modules unavailable: $_. CPU temperature won't be available in this build."
}

# Licenses travel with the binaries.
$license = Get-ChildItem -Path (Join-Path $engine 'openrgb') -Recurse -Include 'LICENSE*', 'COPYING*' | Select-Object -First 1
if (-not $license) {
  Invoke-WebRequest -Uri "https://codeberg.org/OpenRGB/OpenRGB/raw/tag/release_${OpenRgbVersion}/LICENSE" -OutFile (Join-Path $engine 'openrgb\LICENSE')
}
Copy-Item (Join-Path $PSScriptRoot '..\THIRD_PARTY.md') (Join-Path $engine 'THIRD_PARTY.md')

Remove-Item $tmp -Recurse -Force
$size = (Get-ChildItem $engine -Recurse | Measure-Object -Property Length -Sum).Sum / 1MB
Write-Host ("Engine ready in src-tauri/engine ({0:N1} MB)" -f $size)
