# Draws the app icon (a black island pill with a green dot on a violet tile) at every size
# Windows asks for and writes assets\island.ico (PNG entries) and assets\island.png (256 px).
# Usage: powershell -File scripts\make-icon.ps1
Add-Type -AssemblyName System.Drawing
$root = Split-Path $PSScriptRoot
New-Item -ItemType Directory -Force "$root\assets" | Out-Null

function RoundedRect([float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = 2 * $r
    $p.AddArc($x, $y, $d, $d, 180, 90); $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90); $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $p.CloseFigure(); $p
}

function Draw([int]$s) {
    $bmp = New-Object System.Drawing.Bitmap $s, $s
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = 'AntiAlias'; $g.PixelOffsetMode = 'HighQuality'
    # Tile: violet diagonal gradient.
    $tile = RoundedRect 0 0 $s $s ($s * 0.225)
    $grad = New-Object System.Drawing.Drawing2D.LinearGradientBrush ([System.Drawing.PointF]::new(0, 0)), ([System.Drawing.PointF]::new($s, $s)),
        ([System.Drawing.Color]::FromArgb(255, 0x4F, 0x46, 0xE5)), ([System.Drawing.Color]::FromArgb(255, 0x93, 0x33, 0xEA))
    $g.FillPath($grad, $tile)
    # The island: a black pill in the upper half, larger on small icons so it stays visible.
    $w = $s * ($(if ($s -le 24) { 0.80 } else { 0.70 })); $h = $s * ($(if ($s -le 24) { 0.32 } else { 0.26 }))
    $x = ($s - $w) / 2; $y = $s * 0.40 - $h / 2
    $g.FillPath([System.Drawing.Brushes]::Black, (RoundedRect $x $y $w $h ($h / 2)))
    # A green dot at its right end, and from 32 px a short "text" bar at its left.
    $dot = $h * 0.32
    $g.FillEllipse((New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 0x30, 0xD1, 0x58))),
        $x + $w - $h / 2 - $dot / 2, $y + $h / 2 - $dot / 2, $dot, $dot)
    if ($s -ge 32) {
        $bar = $h * 0.22
        $g.FillPath((New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(235, 255, 255, 255))),
            (RoundedRect ($x + $h * 0.45) ($y + $h / 2 - $bar / 2) ($w * 0.40) $bar ($bar / 2)))
    }
    $g.Dispose(); $bmp
}

# 256 px as PNG; smaller sizes as 32-bit DIBs, the form every icon reader takes.
function Dib($bmp) {
    $s = $bmp.Width
    $data = $bmp.LockBits((New-Object System.Drawing.Rectangle 0, 0, $s, $s), 'ReadOnly', 'Format32bppArgb')
    $pixels = New-Object byte[] ($s * $s * 4)
    [System.Runtime.InteropServices.Marshal]::Copy($data.Scan0, $pixels, 0, $pixels.Length); $bmp.UnlockBits($data)
    $maskRow = [math]::Ceiling($s / 32) * 4
    $ms = New-Object System.IO.MemoryStream; $w = New-Object System.IO.BinaryWriter $ms
    $w.Write([uint32]40); $w.Write([int32]$s); $w.Write([int32]($s * 2)); $w.Write([uint16]1); $w.Write([uint16]32)
    $w.Write([uint32]0); $w.Write([uint32]($pixels.Length + $maskRow * $s)); $w.Write([int32]0); $w.Write([int32]0); $w.Write([uint32]0); $w.Write([uint32]0)
    for ($row = $s - 1; $row -ge 0; $row--) { $w.Write($pixels, $row * $s * 4, $s * 4) } # bottom-up BGRA
    $w.Write((New-Object byte[] ($maskRow * $s))) # AND mask unused: alpha decides
    , $ms.ToArray() # the comma keeps PowerShell from unrolling the bytes
}

$sizes = 16, 20, 24, 32, 40, 48, 64, 256
$pngs = foreach ($s in $sizes) {
    $bmp = Draw $s
    if ($s -eq 256) {
        $bmp.Save("$root\assets\island.png", [System.Drawing.Imaging.ImageFormat]::Png)
        $ms = New-Object System.IO.MemoryStream; $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png); $bytes = $ms.ToArray()
    } else {
        [byte[]]$bytes = Dib $bmp
    }
    $bmp.Dispose(); , $bytes
}
# ICO: 6-byte header, a 16-byte entry per image, then the images.
$out = New-Object System.IO.MemoryStream; $w = New-Object System.IO.BinaryWriter $out
$w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $s = $sizes[$i]; $len = $pngs[$i].Length
    $w.Write([byte]($s % 256)); $w.Write([byte]($s % 256)); $w.Write([byte]0); $w.Write([byte]0)
    $w.Write([uint16]1); $w.Write([uint16]32); $w.Write([uint32]$len); $w.Write([uint32]$offset)
    $offset += $len
}
foreach ($png in $pngs) { $w.Write($png) }
[System.IO.File]::WriteAllBytes("$root\assets\island.ico", $out.ToArray())
"wrote assets\island.ico ($((Get-Item "$root\assets\island.ico").Length) bytes)"
