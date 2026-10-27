# Irori brand

| File | Use |
| --- | --- |
| `irori-mark.svg` | The app icon: Linux, Windows, the tray, favicons. Also `packaging/irori.svg`. |
| `irori-macos.svg` | The app icon on Apple's icon grid (824 pt tile on a 1024 canvas, with shadow). Source of `packaging/irori.icns`. |
| `irori-glyph.svg` | The mark without its tile. The frame takes the text colour (`currentColor`). |
| `irori-wordmark.svg` | The name, drawn in strokes. Letters take the text colour; the dots stay flame-coloured. |
| `irori-lockup-dark.svg`, `irori-lockup-light.svg` | Mark and name side by side, for dark and light backgrounds. |

**The mark.** 囲 (the *i* of 囲炉裏, irori) draws an enclosure: the square frame of the
hearth. A flame rises inside it and breaks through the top of the frame. The flame has
three parts, a heart and two tongues behind it, in the way 火 (fire) is written.

**Colours.** Ash `#100b0a`–`#2a1f1a`, hearth wood `#c4ac90`–`#f5e9d9`, flame
`#c92a1c` → `#ff5f2a` → `#ff963a` → `#ffcf6a`.

**Rebuilding the icons** (needs `resvg`; `iconutil` is on macOS):

```bash
cp assets/brand/irori-mark.svg packaging/irori.svg
mkdir -p /tmp/irori.iconset
for s in 16 32 128 256 512; do
  resvg -w $s -h $s assets/brand/irori-macos.svg /tmp/irori.iconset/icon_${s}x${s}.png
  resvg -w $((s*2)) -h $((s*2)) assets/brand/irori-macos.svg /tmp/irori.iconset/icon_${s}x${s}@2x.png
done
iconutil -c icns /tmp/irori.iconset -o packaging/irori.icns
```

`packaging/irori.ico` holds PNG frames of `irori-mark.svg` at 16, 24, 32, 48, 64, 128 and
256 px.

All of these were drawn for Irori from basic shapes; none is derived from another logo.
