# Installs Haul on Windows: downloads the release, checks it against its published checksum, and
# runs `haul setup` (Haul into ~/.haul/bin, first on the user PATH; stable Rust).
#
#   irm https://github.com/jowharshamshiri/haul/releases/latest/download/install.ps1 | iex
$ErrorActionPreference = 'Stop'
$releases = if ($env:HAUL_UPDATE_URL) { $env:HAUL_UPDATE_URL } else { 'https://github.com/jowharshamshiri/haul/releases/latest/download' }
if (-not [Environment]::Is64BitOperatingSystem -or $env:PROCESSOR_ARCHITECTURE -ne 'AMD64') {
    throw 'haul install: no release for this processor; build Haul from source'
}
$target = 'x86_64-pc-windows-msvc'
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("haul-install-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    $archive = Join-Path $tmp 'haul.tar.gz'
    Invoke-WebRequest -UseBasicParsing "$releases/haul-$target.tar.gz" -OutFile $archive
    $want = ((Invoke-WebRequest -UseBasicParsing "$releases/haul-$target.tar.gz.sha256").Content -split '\s+')[0].ToLower()
    $got = (Get-FileHash -Algorithm SHA256 $archive).Hash.ToLower()
    if ($want -ne $got) { throw "haul install: the download is not the published file (checksum $got, published $want)" }
    tar -xzf $archive -C $tmp
    & (Join-Path $tmp "haul-$target\haul.exe") setup @args
    if ($LASTEXITCODE -ne 0) { throw "haul setup failed ($LASTEXITCODE)" }
} finally {
    Remove-Item -Recurse -Force $tmp
}
