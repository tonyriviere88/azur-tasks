# Draws the yatm logo and writes assets/yatm.ico + assets/logo.png.
#
# The mark is generated rather than hand-drawn so that every icon size is laid
# out for that size: below 48px the geometry is snapped to the pixel grid, and
# the decorative passes (grid lines, accent lift) are dropped where they would
# only add mush. Everything is supersampled and box-filtered down.
#
# Needs Windows PowerShell 5.1 for System.Drawing (GDI+); PowerShell 7 does not
# ship System.Drawing.Common.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\mklogo.ps1 `
#       -Out assets [-Sheet sheet.png]
#
# -Sheet writes a contact sheet of every size at 1x and 4x on both a light and
# a dark background, which is the only reliable way to judge the small ones.

param(
    [string]$Out = "assets",
    # "warm" gives the peak bar the app's load tint; "cool" keeps it all blue.
    [string]$Variant = "warm",
    [int]$Grid = 1,
    [string]$Sheet = ""
)

Add-Type -AssemblyName System.Drawing
$ErrorActionPreference = 'Stop'

# ---------------------------------------------------------------- palette ---
# Pulled from src/ui/theme.rs so the icon and the app agree.
$ACCENT = @(0x4C, 0xC2, 0xFF)   # theme.accent
function C([int]$a, [int]$r, [int]$g, [int]$b) {
    [System.Drawing.Color]::FromArgb($a, $r, $g, $b)
}
function Acc([int]$a) { C $a $ACCENT[0] $ACCENT[1] $ACCENT[2] }

# ------------------------------------------------------------------ paths ---
function RoundRect([double]$x, [double]$y, [double]$w, [double]$h, [double]$r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $r = [Math]::Min($r, [Math]::Min($w, $h) / 2)
    if ($r -le 0.01) {
        $p.AddRectangle((New-Object System.Drawing.RectangleF ([float]$x), ([float]$y), ([float]$w), ([float]$h)))
        return $p
    }
    $d = [float](2 * $r)
    $p.AddArc([float]$x,                 [float]$y,                 $d, $d, 180, 90)
    $p.AddArc([float]($x + $w - 2 * $r), [float]$y,                 $d, $d, 270, 90)
    $p.AddArc([float]($x + $w - 2 * $r), [float]($y + $h - 2 * $r), $d, $d, 0,   90)
    $p.AddArc([float]$x,                 [float]($y + $h - 2 * $r), $d, $d, 90,  90)
    $p.CloseFigure()
    $p
}

# A bar: rounded at the top, square on the baseline it stands on.
function BarPath([double]$x, [double]$y, [double]$w, [double]$h, [double]$r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $r = [Math]::Min($r, [Math]::Min($w / 2, $h / 2))
    if ($r -le 0.01) {
        $p.AddRectangle((New-Object System.Drawing.RectangleF ([float]$x), ([float]$y), ([float]$w), ([float]$h)))
        return $p
    }
    $d = [float](2 * $r)
    $p.AddArc([float]$x,                 [float]$y, $d, $d, 180, 90)
    $p.AddArc([float]($x + $w - 2 * $r), [float]$y, $d, $d, 270, 90)
    $p.AddLine([float]($x + $w), [float]($y + $r), [float]($x + $w), [float]($y + $h))
    $p.AddLine([float]($x + $w), [float]($y + $h), [float]$x,        [float]($y + $h))
    $p.CloseFigure()
    $p
}

function VGrad([double]$y0, [double]$y1, $c0, $c1) {
    New-Object System.Drawing.Drawing2D.LinearGradientBrush(
        (New-Object System.Drawing.PointF ([float]0, [float]$y0)),
        (New-Object System.Drawing.PointF ([float]0, [float]$y1)), $c0, $c1)
}

# ----------------------------------------------------------------- render ---
function Render([int]$S, [string]$variant, [int]$grid) {
    # Supersample enough that the rounded corners and bar caps resolve, while
    # the geometry itself stays snapped so small sizes stay crisp (see below).
    $ss = [Math]::Min(16, [Math]::Max(4, [Math]::Ceiling(640.0 / $S)))
    $R  = $S * $ss

    $detail = $S -ge 32     # grid lines and the accent lift
    $bars   = 4

    # --- geometry, in target pixels ---------------------------------------
    $m    = 0.055 * $S                    # margin around the tile
    $tw   = $S - 2 * $m                   # tile edge
    $rad  = 0.190 * $tw                   # tile corner radius
    $pad  = 0.150 * $tw                   # tile edge -> chart box
    $cw   = $tw - 2 * $pad                # chart width
    $base = $S - $m - 0.135 * $tw         # baseline the bars stand on
    $top  = $m + 0.165 * $tw              # top of the tallest bar
    $chh  = $base - $top                  # chart height

    $gapK = 0.34                          # gap as a fraction of bar width
    $bw   = $cw / ($bars + ($bars - 1) * $gapK)
    $gap  = $bw * $gapK
    $hK   = @(0.38, 0.58, 0.79, 1.00)     # bar heights, fraction of $chh
    $hs   = @($hK | ForEach-Object { $chh * $_ })

    # Below ~48px a half-pixel bar edge turns into a grey column, so snap the
    # whole chart to the pixel grid and let the supersampler only smooth the
    # curves. Above that the exact proportions matter more than edge contrast.
    if ($S -le 48) {
        $bw   = [Math]::Max(2, [Math]::Round($bw))
        $gap  = [Math]::Max(1, [Math]::Round($gap))
        $base = [Math]::Round($base)
        $hs   = @($hs | ForEach-Object { [Math]::Max(2, [Math]::Round($_)) })
        # Keep every bar visibly taller than its neighbour after rounding.
        for ($i = 1; $i -lt $bars; $i++) {
            if ($hs[$i] -le $hs[$i - 1]) { $hs[$i] = $hs[$i - 1] + 1 }
        }
        $total = $bars * $bw + ($bars - 1) * $gap
        $x0 = [Math]::Floor(($S - $total) / 2)
    } else {
        $total = $bars * $bw + ($bars - 1) * $gap
        $x0 = ($S - $total) / 2
    }

    # --- surface ----------------------------------------------------------
    $bmp = New-Object System.Drawing.Bitmap ($R), ($R), ([System.Drawing.Imaging.PixelFormat]::Format32bppPArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = 'AntiAlias'
    $g.PixelOffsetMode = 'HighQuality'
    $g.CompositingQuality = 'HighQuality'
    $g.Clear([System.Drawing.Color]::Transparent)

    $tile = RoundRect ($m * $ss) ($m * $ss) ($tw * $ss) ($tw * $ss) ($rad * $ss)

    # Tile: cool slate lit from the top-left. Dark enough to sit beside
    # Windows 11's own dark app tiles, blue enough not to vanish on black.
    $lg = New-Object System.Drawing.Drawing2D.LinearGradientBrush(
        (New-Object System.Drawing.PointF ([float]($m * $ss), [float]($m * $ss))),
        (New-Object System.Drawing.PointF ([float](($m + $tw) * $ss), [float](($m + $tw) * $ss))),
        (C 255 0x30 0x42 0x57), (C 255 0x12 0x1A 0x25))
    $g.FillPath($lg, $tile)
    $lg.Dispose()

    # Accent lift off the bottom edge. Deliberately spans the whole tile so
    # there is no hard seam at the baseline -- an earlier version cut the
    # gradient at the baseline and the dark remainder read as a second panel.
    if ($detail) {
        $g.SetClip($tile)
        $lift = VGrad (($m + $tw) * $ss) (($m + $tw * 0.42) * $ss) (Acc 40) (Acc 0)
        $g.FillRectangle($lift, [float]0, [float](($m + $tw * 0.42) * $ss),
                         [float]$R, [float]($tw * 0.58 * $ss + 2))
        $lift.Dispose()
        $g.ResetClip()
    }

    # --- chart furniture --------------------------------------------------
    # The baseline is meant to read as a rule; the grid behind it as barely
    # there. Scaling both off S/64 made the grid 8px thick at 512.
    $hair = [Math]::Max(1.0, $S / 88.0) * $ss
    $gridW = [Math]::Max(1.0, $S / 200.0) * $ss
    if ($detail -and $grid -ne 0) {
        # Two grid lines, echoing the Performance tab's graphs. Held to the
        # chart's own span plus a short overhang: run them to the tile edge
        # and they read as scratches across the artwork rather than as a grid.
        $gp = New-Object System.Drawing.Pen ((C 34 0xD6 0xE9 0xF7)), ([float]$gridW)
        $gx0 = ($x0 - $gap * 1.3) * $ss
        $gx1 = ($x0 + $total + $gap * 1.3) * $ss
        foreach ($f in 0.36, 0.72) {
            $y = ($base - $chh * $f) * $ss
            $g.DrawLine($gp, [float]$gx0, [float]$y, [float]$gx1, [float]$y)
        }
        $gp.Dispose()
    }

    # --- bars -------------------------------------------------------------
    # One gradient, four alphas: the ramp stays in tune by construction and
    # the tallest bar is unambiguously the subject.
    $alphas = @(160, 198, 226, 255)
    for ($i = 0; $i -lt $bars; $i++) {
        $x = $x0 + $i * ($bw + $gap)
        $h = $hs[$i]
        $y = $base - $h
        $a = $alphas[$i]
        $bp = BarPath ($x * $ss) ($y * $ss) ($bw * $ss) ($h * $ss) ($bw * 0.44 * $ss)

        # The gradient is anchored to the tallest bar, not to each bar, so the
        # four of them read as one object lit from above rather than four
        # independently shaded ones.
        if ($variant -eq 'warm' -and $i -eq $bars - 1) {
            # The peak bar carries the app's warm load tint (theme.warn), the
            # same signal the CPU column uses. Turns a growth chart into a
            # load meter.
            $c0 = C $a 0xFF 0xD1 0x6A; $c1 = C $a 0xF0 0x8E 0x2A
        } else {
            $c0 = C $a 0xA7 0xE9 0xFF; $c1 = C $a 0x3F 0x96 0xD2
        }
        $bg = VGrad ($top * $ss - 1) ($base * $ss + 1) $c0 $c1
        $g.FillPath($bg, $bp)
        $bg.Dispose()
        $bp.Dispose()
    }

    # Baseline: the one fully saturated line in the mark, and what the bars
    # stand on. Without it they float.
    $blc = if ($variant -eq 'warm') { Acc 235 } else { Acc 235 }
    $bl = New-Object System.Drawing.Pen ($blc), ([float]$hair)
    $g.DrawLine($bl, [float]($x0 * $ss - $hair * 0.5), [float]($base * $ss + $hair * 0.5),
                     [float](($x0 + $total) * $ss + $hair * 0.5), [float]($base * $ss + $hair * 0.5))
    $bl.Dispose()

    # --- rim --------------------------------------------------------------
    # A pen fed by a gradient brush: a lit top edge on dark backgrounds, a
    # definition border on light ones. Kept to ~1px at the target size; at
    # 5px it read as a chunky grey stroke.
    $rimW = [Math]::Max(1.0, $S / 200.0) * $ss
    $rimB = VGrad ($m * $ss) (($m + $tw) * $ss) (C 66 0xFF 0xFF 0xFF) (C 20 0x9F 0xC4 0xDE)
    $rim = New-Object System.Drawing.Pen ($rimB), ([float]$rimW)
    $rim.Alignment = 'Inset'
    $g.DrawPath($rim, $tile)
    $rim.Dispose(); $rimB.Dispose()
    $tile.Dispose()
    $g.Dispose()

    # --- downscale --------------------------------------------------------
    $outb = New-Object System.Drawing.Bitmap ($S), ($S), ([System.Drawing.Imaging.PixelFormat]::Format32bppPArgb)
    $g2 = [System.Drawing.Graphics]::FromImage($outb)
    $g2.InterpolationMode = 'HighQualityBicubic'
    $g2.PixelOffsetMode = 'HighQuality'
    $g2.CompositingQuality = 'HighQuality'
    $g2.Clear([System.Drawing.Color]::Transparent)
    $g2.DrawImage($bmp, (New-Object System.Drawing.Rectangle 0, 0, $S, $S),
                  0, 0, $R, $R, [System.Drawing.GraphicsUnit]::Pixel)
    $g2.Dispose()
    $bmp.Dispose()

    # Clone to straight alpha: the PNG encoder and the ICO's 32bpp DIB both
    # want un-premultiplied bytes.
    $final = $outb.Clone((New-Object System.Drawing.Rectangle 0, 0, $S, $S),
                         [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $outb.Dispose()
    $final
}

# Raw top-down BGRA, straight (un-premultiplied) alpha -- the layout a 32bpp
# icon DIB wants, minus the row flip.
function Get-Bgra($bmp) {
    $w = $bmp.Width; $h = $bmp.Height
    $rect = New-Object System.Drawing.Rectangle 0, 0, $w, $h
    $d = $bmp.LockBits($rect, [System.Drawing.Imaging.ImageLockMode]::ReadOnly,
                       [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    if ($d.Stride -ne $w * 4) { throw "unexpected stride $($d.Stride) for width $w" }
    $buf = New-Object byte[] ($w * $h * 4)
    [System.Runtime.InteropServices.Marshal]::Copy($d.Scan0, $buf, 0, $buf.Length)
    $bmp.UnlockBits($d)
    $buf
}

function Get-PngBytes($bmp) {
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $ms.ToArray()
}

# ------------------------------------------------------------------- .ico ---
# A BMP icon image: BITMAPINFOHEADER with a doubled height, then the colour
# rows bottom-up, then the 1bpp AND mask bottom-up. The mask is legacy -- the
# alpha channel is what actually composites -- but it has to agree with the
# alpha or some shell code paths punch holes in the wrong places.
function Ico-BmpEntry([byte[]]$bgra, [int]$s) {
    $ms = New-Object System.IO.MemoryStream
    $bw = New-Object System.IO.BinaryWriter $ms
    $bw.Write([int]40)                      # biSize
    $bw.Write([int]$s)                      # biWidth
    $bw.Write([int](2 * $s))                # biHeight: colour rows + mask rows
    $bw.Write([int16]1)                     # biPlanes
    $bw.Write([int16]32)                    # biBitCount
    $bw.Write([int]0)                       # biCompression = BI_RGB
    $bw.Write([int]0)                       # biSizeImage (ignored for icons)
    $bw.Write([int]0); $bw.Write([int]0)    # pels per metre
    $bw.Write([int]0); $bw.Write([int]0)    # biClrUsed / biClrImportant

    for ($y = $s - 1; $y -ge 0; $y--) { $bw.Write($bgra, $y * $s * 4, $s * 4) }

    $stride = [int]([Math]::Floor(($s + 31) / 32) * 4)
    for ($y = $s - 1; $y -ge 0; $y--) {
        $row = New-Object byte[] $stride
        for ($x = 0; $x -lt $s; $x++) {
            if ($bgra[($y * $s + $x) * 4 + 3] -eq 0) {
                # -shr, not [int]($x / 8): PowerShell's int cast rounds to
                # nearest, so x=63 would land in byte 8 of an 8-byte row.
                $i = $x -shr 3
                $row[$i] = $row[$i] -bor (0x80 -shr ($x % 8))
            }
        }
        $bw.Write($row, 0, $stride)
    }
    $bw.Flush()
    $ms.ToArray()
}

function Write-Ico($entries, [string]$path) {
    $ms = New-Object System.IO.MemoryStream
    $bw = New-Object System.IO.BinaryWriter $ms
    $bw.Write([int16]0)                     # reserved
    $bw.Write([int16]1)                     # type: icon
    $bw.Write([int16]$entries.Count)
    $off = 6 + 16 * $entries.Count
    foreach ($e in $entries) {
        $bw.Write([byte]($e.S % 256))       # 256 is encoded as 0
        $bw.Write([byte]($e.S % 256))
        $bw.Write([byte]0)                  # palette entries: none at 32bpp
        $bw.Write([byte]0)                  # reserved
        $bw.Write([int16]1)                 # planes
        $bw.Write([int16]32)                # bit count
        $bw.Write([int]$e.Data.Length)
        $bw.Write([int]$off)
        $off += $e.Data.Length
    }
    foreach ($e in $entries) { $bw.Write($e.Data, 0, $e.Data.Length) }
    $bw.Flush()
    [System.IO.File]::WriteAllBytes($path, $ms.ToArray())
}

# --- contact sheet: real size and 4x nearest-neighbour, on both themes ------
function Sheet($imgs, [string]$path) {
    $strip = 16, 20, 24, 32, 40, 48, 64, 128
    $pad = 10
    $w = $pad; foreach ($s in $strip) { $w += ($s * 4) + $pad }
    $halfH = 128 + $pad * 2 + 4 * 128
    $sheet = New-Object System.Drawing.Bitmap ($w), (2 * $halfH)
    $sh = [System.Drawing.Graphics]::FromImage($sheet)
    $sh.Clear((C 255 0x1b 0x1b 0x1b))
    $sh.FillRectangle((New-Object System.Drawing.SolidBrush (C 255 0xF3 0xF3 0xF3)),
                      0, $halfH, $sheet.Width, $halfH)
    $sh.InterpolationMode = 'NearestNeighbor'
    $sh.PixelOffsetMode = 'HighQuality'
    foreach ($half in 0, 1) {
        $yTop = $half * $halfH + $pad
        $x = $pad
        foreach ($s in $strip) {
            $sh.DrawImage($imgs[$s], (New-Object System.Drawing.Rectangle $x, $yTop, $s, $s))
            $sh.DrawImage($imgs[$s], (New-Object System.Drawing.Rectangle $x, ($yTop + 140), ($s * 4), ($s * 4)))
            $x += ($s * 4) + $pad
        }
    }
    $sh.Dispose()
    $sheet.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    $sheet.Dispose()
}

# ------------------------------------------------------------------- main ---
if (-not (Test-Path $Out)) { New-Item -ItemType Directory -Path $Out | Out-Null }
$Out = (Resolve-Path $Out).Path

# BMP below 128 and PNG at 128/256. The small sizes are the ones legacy shell
# paths ask for, and at 128+ an uncompressed DIB costs 66 KB against a 9 KB PNG.
$bmpSizes = 16, 20, 24, 32, 40, 48, 64
$pngSizes = 128, 256

$imgs = @{}
$entries = @()
foreach ($s in $bmpSizes) {
    $b = Render $s $Variant $Grid
    $imgs[$s] = $b
    $entries += @{ S = $s; Data = (Ico-BmpEntry (Get-Bgra $b) $s) }
}
foreach ($s in $pngSizes) {
    $b = Render $s $Variant $Grid
    $imgs[$s] = $b
    $entries += @{ S = $s; Data = (Get-PngBytes $b) }
}

$ico = Join-Path $Out 'yatm.ico'
Write-Ico $entries $ico

# Hero image for the README.
$hero = Render 512 $Variant $Grid
$hero.Save((Join-Path $Out 'logo.png'), [System.Drawing.Imaging.ImageFormat]::Png)

if ($Sheet) { Sheet $imgs $Sheet }

"$ico  $((Get-Item $ico).Length) bytes, $($entries.Count) images"
foreach ($e in $entries) { "  {0,3}px  {1,7} bytes" -f $e.S, $e.Data.Length }
