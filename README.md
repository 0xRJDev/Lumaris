# Lumaris

A lean and fast image viewer for Linux, written in Rust.

- Native Wayland (and X11) through `winit`, GPU drawing through `wgpu` (Vulkan, OpenGL fallback)
- Transparent window with the blurred desktop behind it (KWin; the standard
  ext-background-effect protocol, or KDE's older blur protocol)
- The window opens centred and fitted to the image (at most 80% of the screen, 16:9 unless the
  image is wider). While zooming it grows and shrinks with the image up to the screen size,
  keeping its centre (KWin: a small script loaded over D-Bus while Lumaris runs, nothing installed)
- Smooth zoom around the mouse pointer, panning by dragging
- Filters: bicubic when enlarging, mipmaps when reducing, sharp square pixels for pixel art
  (images up to 256 px), exact pixels at 100%
- Next / previous image of the folder in natural order (looping); neighbours are decoded in advance
- Slideshow (keys 1 to 9: seconds per image)
- Right-click menu in the desktop's style (KDE font and colours): open with the applications
  associated with the file type (default first), open the containing folder, copy the image or
  its path, navigation, slideshow, zoom, full screen
- Formats: JPEG, PNG, GIF, WebP, AVIF, HEIF/HEIC, JPEG XL, SVG (sharp at any zoom), camera RAW
  (embedded preview, LibRaw), BMP, TIFF, QOI, ICO, TGA, PNM; animated GIF, WebP and PNG

## Build

```sh
cargo build --release
target/release/lumaris <image or folder>
```

## Keys

| Key | Action |
|---|---|
| Right, Page Down / Left, Page Up, Backspace | Next / previous image (loops through the folder) |
| Home / End | First / last image |
| Mouse wheel, + / - | Zoom around the pointer |
| Space / Enter | Fit to window or 100% / fit to window |
| 1 to 9 | Slideshow, 1 to 9 seconds per image (Ctrl: tenths of a second) |
| Ctrl+C / Ctrl+Shift+C | Copy the image / its file path |
| F11 | Full screen |
| Esc | Stop the slideshow, leave full screen, quit |
| Right click | Menu |
| Mouse back / forward | Previous / next image |

Copying needs `wl-clipboard` (Wayland) or `xclip` (X11).

## License

GNU General Public License, version 2 or later (see `LICENSE`).
