# Draws the package logos and the context-menu icon into package\Assets.
# (ASCII only on purpose: Windows PowerShell 5.1 reads BOM-less scripts as ANSI.)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$assets = Join-Path (Split-Path $PSScriptRoot -Parent) 'package\Assets'
New-Item -ItemType Directory -Force $assets | Out-Null

function New-LogoBitmap([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit
    $g.Clear([System.Drawing.Color]::Transparent)

    # Rounded red tile
    $pad = [Math]::Max(1.0, $size * 0.06)
    $side = $size - 2 * $pad
    $radius = $side * 0.22
    $d = 2 * $radius
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $path.AddArc($pad, $pad, $d, $d, 180, 90)
    $path.AddArc($pad + $side - $d, $pad, $d, $d, 270, 90)
    $path.AddArc($pad + $side - $d, $pad + $side - $d, $d, $d, 0, 90)
    $path.AddArc($pad, $pad + $side - $d, $d, $d, 90, 90)
    $path.CloseFigure()
    $red = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 226, 52, 45))
    $g.FillPath($red, $path)

    # White label: "PDF" on large sizes, "P" when too small to read
    $text = if ($size -ge 32) { 'PDF' } else { 'P' }
    $fontSize = if ($size -ge 32) { $size * 0.30 } else { $size * 0.56 }
    $font = New-Object System.Drawing.Font('Segoe UI', $fontSize, [System.Drawing.FontStyle]::Bold, [System.Drawing.GraphicsUnit]::Pixel)
    $format = New-Object System.Drawing.StringFormat
    $format.Alignment = [System.Drawing.StringAlignment]::Center
    $format.LineAlignment = [System.Drawing.StringAlignment]::Center
    $rect = New-Object System.Drawing.RectangleF(0, 0, $size, $size)
    $g.DrawString($text, $font, [System.Drawing.Brushes]::White, $rect, $format)

    $g.Dispose()
    return $bmp
}

function Get-PngBytes([int]$size) {
    $bmp = New-LogoBitmap $size
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    return ,$ms.ToArray()
}

# Package logos
[System.IO.File]::WriteAllBytes((Join-Path $assets 'Square44x44Logo.png'), (Get-PngBytes 44))
[System.IO.File]::WriteAllBytes((Join-Path $assets 'Square150x150Logo.png'), (Get-PngBytes 150))
[System.IO.File]::WriteAllBytes((Join-Path $assets 'StoreLogo.png'), (Get-PngBytes 50))

# Multi-size .ico (PNG-compressed entries)
$sizes = 16, 20, 24, 32, 40, 48, 64, 256
$images = foreach ($s in $sizes) { ,(Get-PngBytes $s) }
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
[System.IO.File]::WriteAllBytes((Join-Path $assets 'menu.ico'), $out.ToArray())

Get-ChildItem $assets | Select-Object Name, Length
