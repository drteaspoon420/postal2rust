# tools\release\make_icon.ps1 OUT.ico -- draws the app icon (a rust-orange gear, original art) at
# 16/32/48/256 px and writes a PNG-compressed .ico.
param([string]$Out)
Add-Type -AssemblyName System.Drawing
function Draw([int]$s) {
  $b = [Drawing.Bitmap]::new($s, $s, [Drawing.Imaging.PixelFormat]::Format32bppArgb)
  $g = [Drawing.Graphics]::FromImage($b)
  $g.SmoothingMode = 'AntiAlias'
  $g.Clear([Drawing.Color]::Transparent)
  $c = $s / 2.0
  $teeth = 10
  $rOut = $s * 0.48; $rIn = $s * 0.39
  $pts = New-Object 'System.Collections.Generic.List[System.Drawing.PointF]'
  for ($i = 0; $i -lt $teeth; $i++) {
    $a0 = 2 * [Math]::PI * $i / $teeth
    $w = [Math]::PI / $teeth
    foreach ($k in @(@(-0.95, $rIn), @(-0.55, $rOut), @(0.55, $rOut), @(0.95, $rIn))) {
      $a = $a0 + $k[0] * $w
      $pts.Add([Drawing.PointF]::new([float]($c + $k[1] * [Math]::Cos($a)), [float]($c + $k[1] * [Math]::Sin($a))))
    }
  }
  $gear = [Drawing.Drawing2D.GraphicsPath]::new()
  $gear.AddPolygon($pts.ToArray())
  $hole = $s * 0.17
  $gear.AddEllipse([float]($c - $hole), [float]($c - $hole), [float](2 * $hole), [float](2 * $hole))
  $gear.FillMode = 'Alternate'
  $rust = [Drawing.Drawing2D.LinearGradientBrush]::new([Drawing.PointF]::new(0, 0), [Drawing.PointF]::new($s, $s), [Drawing.Color]::FromArgb(255, 222, 120, 52), [Drawing.Color]::FromArgb(255, 140, 52, 20))
  $g.FillPath($rust, $gear)
  $pen = [Drawing.Pen]::new([Drawing.Color]::FromArgb(255, 70, 24, 8), [float]([Math]::Max(1.0, $s / 40.0)))
  $g.DrawPath($pen, $gear)
  # Rust speckles (pitting) on the larger sizes.
  if ($s -ge 48) {
    $rnd = [Random]::new(7)
    $dark = [Drawing.SolidBrush]::new([Drawing.Color]::FromArgb(120, 90, 30, 10))
    for ($i = 0; $i -lt [int]($s / 3); $i++) {
      $ang = $rnd.NextDouble() * 2 * [Math]::PI; $rad = $s * (0.2 + 0.18 * $rnd.NextDouble())
      $d = $s * (0.015 + 0.02 * $rnd.NextDouble())
      $g.FillEllipse($dark, [float]($c + $rad * [Math]::Cos($ang) - $d), [float]($c + $rad * [Math]::Sin($ang) - $d), [float](2 * $d), [float](2 * $d))
    }
  }
  $g.Dispose()
  $ms = [IO.MemoryStream]::new()
  $b.Save($ms, [Drawing.Imaging.ImageFormat]::Png)
  return ,$ms.ToArray()
}
$sizes = 16, 32, 48, 256
$imgs = foreach ($s in $sizes) { ,(Draw $s) }
$f = [IO.BinaryWriter]::new([IO.File]::Create($Out))
$f.Write([uint16]0); $f.Write([uint16]1); $f.Write([uint16]$sizes.Count)
$off = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
  $s = $sizes[$i]; $d = $imgs[$i]
  $f.Write([byte]($s % 256)); $f.Write([byte]($s % 256)); $f.Write([byte]0); $f.Write([byte]0)
  $f.Write([uint16]1); $f.Write([uint16]32); $f.Write([uint32]$d.Length); $f.Write([uint32]$off)
  $off += $d.Length
}
foreach ($d in $imgs) { $f.Write($d) }
$f.Close()
"wrote $Out ($($sizes -join ', ') px)"
