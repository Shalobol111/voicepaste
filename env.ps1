# Dot-source before building: . .\env.ps1
# Sets the variables whisper-rs-sys needs to build with CUDA.
# EDIT THE PATHS BELOW for your machine (CUDA version, LLVM location, GPU architecture).
$cuda = 'C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v13.4'
$env:Path = [Environment]::GetEnvironmentVariable('Path','Machine') + ';' + [Environment]::GetEnvironmentVariable('Path','User') + ';C:\Program Files\LLVM\bin'
$env:Path += ";$cuda\bin\x64"          # CUDA 13 keeps runtime DLLs in bin\x64; needed to *run* the exe
$env:LIBCLANG_PATH ='C:\Program Files\LLVM\bin'
$env:CUDA_PATH = $cuda
$env:CUDA_PATH_V13_4 = $cuda      # MSBuild CUDA integration needs this, otherwise CudaToolkitDir is empty
$env:CudaToolkitDir = "$cuda\"
$env:CMAKE_CUDA_ARCHITECTURES = '89'  # RTX 4060 (Ada); drop for a multi-arch build
