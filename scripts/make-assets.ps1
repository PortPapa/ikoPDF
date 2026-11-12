# Draws the icons into package\Assets (event_code blog style: dark tile, coral accent).
#   menu.ico  - right-click menu icon: dark tile with "PDF" and a coral cursor block
#   app.ico   - program / window icon: the iko pixel character (crates\converter\ui\iko.png)
#   *.png     - package logos (same tile as menu.ico)
# (ASCII only on purpose: Windows PowerShell 5.1 reads BOM-less scripts as ANSI.)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$root = Split-Path $PSScriptRoot -Parent
$assets = Join-Path $root 'package\Assets'
New-Item -ItemType Directory -Force $assets | Out-Null

$ink = [System.Drawing.Color]::FromArgb(255, 28, 26, 24)
$cream = [System.Drawing.Color]::FromArgb(255, 243, 237, 226)
$coral = [System.Drawing.Color]::FromArgb(255, 224, 105, 75)

function New-TileBitmap([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit
    $g.Clear([System.Drawing.Color]::Transparent)

    # Dark rounded tile
    $pad = [Math]::Max(0.5, $size * 0.04)
    $side = $size - 2 * $pad
    $d = 2 * ($side * 0.22)
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $path.AddArc($pad, $pad, $d, $d, 180, 90)
    $path.AddArc($pad + $side - $d, $pad, $d, $d, 270, 90)
    $path.AddArc($pad + $side - $d, $pad + $side - $d, $d, $d, 0, 90)
    $path.AddArc($pad, $pad + $side - $d, $d, $d, 90, 90)
    $path.CloseFigure()
    $g.FillPath((New-Object System.Drawing.SolidBrush $ink), $path)

    # Cream label + coral cursor block: "PDF|" on large sizes, "P|" when too small to read
    $text = if ($size -ge 32) { 'PDF' } else { 'P' }
    $fontSize = if ($size -ge 32) { $size * 0.27 } else { $size * 0.56 }
    $font = New-Object System.Drawing.Font('Segoe UI', $fontSize, [System.Drawing.FontStyle]::Bold, [System.Drawing.GraphicsUnit]::Pixel)
    $format = [System.Drawing.StringFormat]::GenericTypographic
    $measured = $g.MeasureString($text, $font, 1000, $format)
    $cursorW = [Math]::Max(2.0, $size * 0.085)
    $gap = [Math]::Max(1.0, $size * 0.04)
    $total = $measured.Width + $gap + $cursorW
    $x = ($size - $total) / 2
    $y = ($size - $measured.Height) / 2
    $g.DrawString($text, $font, (New-Object System.Drawing.SolidBrush $cream), $x, $y, $format)
    $cursorH = $fontSize * 0.86
    $g.FillRectangle((New-Object System.Drawing.SolidBrush $coral), [single]($x + $measured.Width + $gap), [single](($size - $cursorH) / 2 + $size * 0.01), [single]$cursorW, [single]$cursorH)

    $g.Dispose()
    return $bmp
}

function New-CharacterBitmap([int]$size) {
    $source = New-Object System.Drawing.Bitmap((Join-Path $root 'crates\converter\ui\iko.png'))
    $bmp = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.Clear([System.Drawing.Color]::Transparent)
    # Rounded clip so the icon sits nicely on any background
    $radius = $size * 0.2; $d = 2 * $radius
    $clip = New-Object System.Drawing.Drawing2D.GraphicsPath
    $clip.AddArc(0, 0, $d, $d, 180, 90); $clip.AddArc($size - $d, 0, $d, $d, 270, 90)
    $clip.AddArc($size - $d, $size - $d, $d, $d, 0, 90); $clip.AddArc(0, $size - $d, $d, $d, 90, 90); $clip.CloseFigure()
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.SetClip($clip)
    # Keep the pixels crisp when enlarging, smooth when shrinking
    if ($size -ge 2 * $source.Width) { $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::NearestNeighbor }
    else { $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic }
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::Half
    $g.DrawImage($source, 0, 0, $size, $size)
    $g.Dispose(); $source.Dispose()
    return $bmp
}

function Get-PngBytes($bitmap) {
    $ms = New-Object System.IO.MemoryStream
    $bitmap.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $bitmap.Dispose()
    return ,$ms.ToArray()
}

# Multi-size .ico with PNG-compressed entries
function Save-Ico([string]$path, [int[]]$sizes, [scriptblock]$draw) {
    $images = foreach ($s in $sizes) { ,(Get-PngBytes (& $draw $s)) }
    $out = New-Object System.IO.MemoryStream
    $w = New-Object System.IO.BinaryWriter($out)
    $w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]$sizes.Count)
    $offset = 6 + 16 * $sizes.Count
    for ($i = 0; $i -lt $sizes.Count; $i++) {
        $dim = if ($sizes[$i] -ge 256) { 0 } else { $sizes[$i] }
        $w.Write([byte]$dim); $w.Write([byte]$dim); $w.Write([byte]0); $w.Write([byte]0)
        $w.Write([uint16]1); $w.Write([uint16]32)
        $w.Write([uint32]$images[$i].Length); $w.Write([uint32]$offset)
        $offset += $images[$i].Length
    }
    foreach ($img in $images) { $w.Write($img) }
    $w.Flush()
    [System.IO.File]::WriteAllBytes($path, $out.ToArray())
}

$sizes = 16, 20, 24, 32, 40, 48, 64, 256
Save-Ico (Join-Path $assets 'menu.ico') $sizes { param($s) New-TileBitmap $s }
Save-Ico (Join-Path $assets 'app.ico') $sizes { param($s) New-CharacterBitmap $s }

# Package logos
[System.IO.File]::WriteAllBytes((Join-Path $assets 'Square44x44Logo.png'), (Get-PngBytes (New-TileBitmap 44)))
[System.IO.File]::WriteAllBytes((Join-Path $assets 'Square150x150Logo.png'), (Get-PngBytes (New-TileBitmap 150)))
[System.IO.File]::WriteAllBytes((Join-Path $assets 'StoreLogo.png'), (Get-PngBytes (New-TileBitmap 50)))

# Previews for a quick look (not used by the build)
$preview = Join-Path $root 'target\icon-preview'
New-Item -ItemType Directory -Force $preview | Out-Null
foreach ($s in 16, 32, 64) {
    [System.IO.File]::WriteAllBytes((Join-Path $preview "menu-$s.png"), (Get-PngBytes (New-TileBitmap $s)))
    [System.IO.File]::WriteAllBytes((Join-Path $preview "app-$s.png"), (Get-PngBytes (New-CharacterBitmap $s)))
}

Get-ChildItem $assets | Select-Object Name, Length
