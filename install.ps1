# Installs QuickRust on Windows: downloads the release, checks it against its published checksum, and
# runs `qrust setup` (QuickRust into ~/.qrust/bin, first on the user PATH; stable Rust).
#
#   irm https://github.com/jowharshamshiri/quickrust/releases/latest/download/install.ps1 | iex
$ErrorActionPreference = 'Stop'
$releases = if ($env:QRUST_UPDATE_URL) { $env:QRUST_UPDATE_URL } else { 'https://github.com/jowharshamshiri/quickrust/releases/latest/download' }
if (-not [Environment]::Is64BitOperatingSystem -or $env:PROCESSOR_ARCHITECTURE -ne 'AMD64') {
    throw 'qrust install: no release for this processor; build QuickRust from source'
}
$target = 'x86_64-pc-windows-msvc'
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("qrust-install-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    $archive = Join-Path $tmp 'qrust.tar.gz'
    Invoke-WebRequest -UseBasicParsing "$releases/qrust-$target.tar.gz" -OutFile $archive
    $want = ((Invoke-WebRequest -UseBasicParsing "$releases/qrust-$target.tar.gz.sha256").Content -split '\s+')[0].ToLower()
    $got = (Get-FileHash -Algorithm SHA256 $archive).Hash.ToLower()
    if ($want -ne $got) { throw "qrust install: the download is not the published file (checksum $got, published $want)" }
    tar -xzf $archive -C $tmp
    & (Join-Path $tmp "qrust-$target\qrust.exe") setup @args
    if ($LASTEXITCODE -ne 0) { throw "qrust setup failed ($LASTEXITCODE)" }
} finally {
    Remove-Item -Recurse -Force $tmp
}
