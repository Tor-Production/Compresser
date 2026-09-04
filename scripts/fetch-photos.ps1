<#
.SYNOPSIS
Downloads real photographs into the sample corpus.

.DESCRIPTION
The synthetic corpus from `gen-samples` brackets the algorithm's behaviour but says nothing about
how it does on photographs, which is what the format is ultimately for. This fetches part of the
Kodak True Color Image Suite -- the set most lossless-codec papers benchmark against, so numbers
measured here are comparable with published ones.

The images are 768x512 or 512x768 PNGs, roughly 700 KB each. They are downloaded rather than
committed: they are large, they are not ours, and everything in samples/ is gitignored.

Licensing: Kodak released this suite for unrestricted research and comparison use. It is not a
formal public-domain dedication, so use these for measurement, not redistribution.

.EXAMPLE
pwsh scripts/fetch-photos.ps1
pwsh scripts/fetch-photos.ps1 -Destination samples -Images 1,5,13
#>
[CmdletBinding()]
param(
    [string]$Destination = "samples",
    [int[]]$Images = @(1, 4, 5, 7, 9, 13, 19, 23)
)

$ErrorActionPreference = "Stop"
$base = "https://r0k.us/graphics/kodak/kodak"

if (-not (Test-Path $Destination)) {
    New-Item -ItemType Directory -Path $Destination | Out-Null
}

$downloaded = 0
$skipped = 0

foreach ($n in $Images) {
    $name = "kodim{0:d2}.png" -f $n
    $target = Join-Path $Destination "photo-$name"

    if (Test-Path $target) {
        Write-Host ("{0,-24} already present" -f $target)
        $skipped++
        continue
    }

    $url = "$base/$name"
    try {
        Invoke-WebRequest -Uri $url -OutFile $target -TimeoutSec 120 -UseBasicParsing
        $size = (Get-Item $target).Length
        Write-Host ("{0,-24} {1,10:N0} bytes" -f $target, $size)
        $downloaded++
    } catch {
        Write-Warning "failed to fetch ${url}: $($_.Exception.Message)"
        if (Test-Path $target) { Remove-Item $target -Force }
    }
}

Write-Host ""
Write-Host "$downloaded downloaded, $skipped already present, in $Destination"
Write-Host "Now run:  cargo run -p brp-lab --release -- $Destination"
