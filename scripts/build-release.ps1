# Builds a release exe without a console window (tray-only GUI app).
#   .\scripts\build-release.ps1 -Variant nvidia   # CUDA build for RTX 40 (sm_89)
#   .\scripts\build-release.ps1 -Variant cpu      # runs on any PC, no NVIDIA needed
# Runs at below-normal priority with a limited number of jobs, so the PC stays usable.
param(
    [ValidateSet('nvidia', 'cpu')][string]$Variant = 'nvidia',
    [int]$Jobs = 4,
    # Rebuild whisper.cpp from scratch. Needed after changing the CMake settings below,
    # because whisper-rs-sys does not watch these environment variables.
    [switch]$Clean
)

# Not 'Stop': cargo prints progress to stderr, which PowerShell 5.1 would treat as an error.
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
. "$root\env.ps1"

# Children inherit this, so cargo, cmake and the compilers all run politely.
(Get-Process -Id $PID).PriorityClass = 'BelowNormal'

# cmake-rs replaces CMake's default Release flags with its own, and on MSVC those contain
# no optimization switch at all: whisper.cpp was compiled unoptimized (/Od), about 100x
# slower on the CPU. Spell the flags out (whisper-rs-sys forwards CMAKE_* variables).
$env:CMAKE_C_FLAGS_RELEASE = '-nologo -MD -Brepro -W0 /O2 /Ob2 /DNDEBUG'
$env:CMAKE_CXX_FLAGS_RELEASE = '/utf-8 -nologo -MD -Brepro -W0 /O2 /Ob2 /DNDEBUG'

New-Item -ItemType Directory -Force "$root\target-dist" | Out-Null
$log = "$root\target-dist\build-$Variant.log"
"started $(Get-Date -Format s)" | Out-File $log -Encoding utf8

if ($Variant -eq 'nvidia') {
    # Same target dir and architecture as development builds, so whisper.cpp is reused.
    $env:CMAKE_CUDA_ARCHITECTURES = '89'
    $features = @('--no-default-features', '--features', 'cuda')
    $exe = "$root\target\release\voicepaste.exe"
} else {
    Remove-Item Env:CMAKE_CUDA_ARCHITECTURES -ErrorAction SilentlyContinue
    # MSVC does not auto-detect the CPU (GGML_NATIVE only works for GCC/Clang), so without
    # these flags whisper.cpp runs scalar code, about 100 times slower. AVX2+FMA+F16C is
    # available on every x86-64 CPU made since 2013-2015.
    $env:GGML_AVX = 'ON'
    $env:GGML_AVX2 = 'ON'
    $env:GGML_FMA = 'ON'
    $env:GGML_F16C = 'ON'
    $env:CARGO_TARGET_DIR = "$root\target-dist\cpu"
    $features = @('--no-default-features')
    $exe = "$root\target-dist\cpu\release\voicepaste.exe"
}

if ($Clean) { cargo clean --release -p whisper-rs-sys 2>&1 | Out-File -Append $log -Encoding utf8 }

$sw = [Diagnostics.Stopwatch]::StartNew()
cargo build --release -j $Jobs @features 2>&1 | Out-File -Append $log -Encoding utf8
$code = $LASTEXITCODE
if ($code -eq 0 -and (Test-Path $exe)) {
    # Keep a copy that later (console) dev builds cannot overwrite.
    $out = "$root\dist\build\$Variant"
    New-Item -ItemType Directory -Force $out | Out-Null
    Copy-Item $exe "$out\voicepaste.exe" -Force
}
"exit=$code elapsed=$($sw.Elapsed) exe=$exe" | Out-File -Append $log -Encoding utf8
exit $code
