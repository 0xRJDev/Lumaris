# Lumae (Rust)

A lean and fast image viewer for Linux, written in Rust. Rewrite of
[Lumae](https://github.com/0xRJDev/Lumae) (C++/Qt), in progress.

- Native Wayland (and X11) through `winit`, GPU drawing through `wgpu` (Vulkan, OpenGL fallback)
- Transparent window with the blurred desktop behind it (KWin; the standard
  ext-background-effect protocol, or KDE's older blur protocol)
- The window opens centred and fitted to the image (at most 80% of the screen, 16:9 unless the
  image is wider). While zooming it grows and shrinks with the image up to the screen size,
  keeping its centre (KWin: a small script loaded over D-Bus while Lumae runs, nothing installed)
- Smooth zoom around the mouse pointer, panning by dragging
- Filters: bicubic when enlarging, mipmaps when reducing, sharp square pixels for pixel art
  (images up to 256 px), exact pixels at 100%
- Next / previous image of the folder in natural order; neighbours are decoded in advance
- Formats: JPEG, PNG, GIF, WebP, BMP, TIFF, QOI, ICO, TGA, PNM

## Build

```sh
cargo build --release
target/release/lumae <image or folder>
```

## Keys

| Key | Action |
|---|---|
| Right, Page Down, Space / Left, Page Up, Backspace | Next / previous image |
| Home / End | First / last image |
| Mouse wheel, + / - | Zoom |
| 0 or F / 1 | Fit to window / 100% |
| F11 | Full screen |
| Esc | Leave full screen, quit |
| Mouse back / forward | Previous / next image |

## License

GNU General Public License, version 2 or later (see `LICENSE`).
