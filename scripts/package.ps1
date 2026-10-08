# Assembles the release folders and zip files from the exes that build-release.ps1 made.
#   .\scripts\package.ps1
# Output: dist\release\VoicePaste-<version>-nvidia (folder + .zip) and ...-cpu, plus
# dist\release\SHA256SUMS.txt. Variants whose exe is missing are skipped.

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
Add-Type -AssemblyName System.IO.Compression.FileSystem

$version = (Select-String -Path "$root\Cargo.toml" -Pattern '^version\s*=\s*"([^"]+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value
$cuda = if ($env:CUDA_PATH) { Join-Path $env:CUDA_PATH 'bin\x64' }
        else { 'C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v13.4\bin\x64' }
$system32 = Join-Path $env:WINDIR 'System32'

# App-local copies of the Visual C++ runtime (redistributable), so the app starts even
# on a PC without the "Visual C++ Redistributable" installed.
$vcDlls = 'msvcp140.dll', 'vcruntime140.dll', 'vcruntime140_1.dll'
# cuBLAS and its dependency; the NVIDIA driver supplies nvcuda.dll.
$cudaDlls = 'cublas64_13.dll', 'cublasLt64_13.dll', 'cudart64_13.dll'

$variants = @(
    @{ Name = 'nvidia'; Dlls = $cudaDlls + $vcDlls; Source = $cuda },
    @{ Name = 'cpu';    Dlls = $vcDlls;             Source = $null }
)

$outRoot = Join-Path $root 'dist\release'
New-Item -ItemType Directory -Force $outRoot | Out-Null
$sums = @()

# Older layouts put the folders straight into dist\; remove them if nothing runs from them.
Get-ChildItem (Join-Path $root 'dist') -Filter 'VoicePaste-*' -ErrorAction SilentlyContinue | ForEach-Object {
    try {
        if ($_.PSIsContainer) { [IO.Directory]::Delete($_.FullName, $true) } else { [IO.File]::Delete($_.FullName) }
    } catch { Write-Host "left in place (in use): $($_.Name)" }
}

foreach ($v in $variants) {
    $exe = Join-Path $root "dist\build\$($v.Name)\voicepaste.exe"
    if (-not (Test-Path $exe)) { Write-Host "skip $($v.Name): no exe at $exe"; continue }

    $folder = Join-Path $outRoot "VoicePaste-$version-$($v.Name)"
    $zip = "$folder.zip"
    if (Test-Path $folder) { [IO.Directory]::Delete($folder, $true) }
    if (Test-Path $zip) { [IO.File]::Delete($zip) }
    New-Item -ItemType Directory -Force $folder | Out-Null

    Copy-Item $exe (Join-Path $folder 'voicepaste.exe')
    foreach ($doc in 'README.md', 'README.ru.md', 'LICENSE') {
        Copy-Item (Join-Path $root $doc) (Join-Path $folder $doc)
    }
    foreach ($dll in $v.Dlls) {
        $src = if ($dll -like 'cu*') { Join-Path $v.Source $dll } else { Join-Path $system32 $dll }
        if (-not (Test-Path $src)) { throw "missing $src" }
        Copy-Item $src (Join-Path $folder $dll)
    }

    [IO.Compression.ZipFile]::CreateFromDirectory($folder, $zip, [IO.Compression.CompressionLevel]::Optimal, $true)
    $hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
    $sums += "$hash  $(Split-Path -Leaf $zip)"
    '{0}: folder {1:N0} MB, zip {2:N0} MB' -f $v.Name,
        ((Get-ChildItem $folder -Recurse -File | Measure-Object Length -Sum).Sum / 1MB),
        ((Get-Item $zip).Length / 1MB)
}

if ($sums) { $sums | Set-Content (Join-Path $outRoot 'SHA256SUMS.txt') -Encoding ascii }
