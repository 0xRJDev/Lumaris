<p align="center"><img src="packaging/lumaris.svg" width="128" alt=""></p>

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
- Comic archives (.cbz / .zip): the pages are shown like the images of a folder
- The zoom is shown for a moment while zooming
- Slideshow (keys 1 to 9: seconds per image)
- Right-click menu in the desktop's style (KDE font and colours), which can extend beyond the
  window (Wayland layer surfaces): open with the applications
  associated with the file type (default first), open the containing folder, copy the image or
  its path, navigation, slideshow, zoom, full screen
- Image info box (key I): file, folder, size, format, dates and camera details (EXIF)
- Settings window (Ctrl+, or the menu): window, background (blurred / see-through desktop,
  solid colour, checkerboard), zoom, navigation (sort order), slideshow, formats; saved to
  `~/.config/lumaris/lumaris.conf`, which can also be edited by hand
- Formats: JPEG, PNG, GIF, WebP, AVIF, HEIF/HEIC, JPEG XL, SVG (sharp at any zoom), camera RAW
  (LibRaw), EXR and Radiance HDR (tone mapped), PSD/PSB, GIMP XCF, Krita, OpenRaster, JPEG 2000,
  PDF (first page, needs poppler), BMP, TIFF, QOI, ICO, TGA, PNM, DDS, farbfeld; animated GIF, WebP and PNG; embedded colour profiles
- Navigation panel (buttons and a zoom map), rotate / flip, move to trash, open dialog, drag and
  drop, single window mode, slideshow with fade, histogram, user commands, F1 key help

## Build and install

```sh
./install.sh                # builds and installs to ~/.local (binary, desktop entry, icon)
lumaris <image or folder>
```

Or only build: `cargo build --release`, then run `target/release/lumaris`.

## Keys

Press F1 in Lumaris for the full list. All keys can be changed in
`~/.config/lumaris/keys.conf` (Settings > Edit key bindings).

| Key | Action |
|---|---|
| Right, Page Down / Left, Page Up, Backspace | Next / previous image |
| Mouse wheel, + / - | Zoom around the pointer (Shift+wheel: the window keeps its size) |
| Space / Enter / 0 | Fit or 100% / fit / 100% |
| Up, Down, Ctrl+Left, Ctrl+Right | Move the image when zoomed in |
| R / Shift+R, H / V | Rotate right / left, flip horizontally / vertically |
| 1 to 9 | Slideshow, 1 to 9 seconds per image |
| Ctrl+C / Ctrl+Shift+C | Copy the image / its file path |
| Ctrl+O, Delete | Open image, move to trash |
| I, N, F1, Ctrl+, | Image info, navigation panel, keys, settings |
| F11, F, double click | Full screen |
| Esc | Close a panel, stop the slideshow, leave full screen, quit |

User commands for the menu and keys: `~/.config/lumaris/commands.conf` (Settings > Edit user
commands).

Copying needs `wl-clipboard` (Wayland) or `xclip` (X11).

## License

GNU General Public License, version 2 or later (see `LICENSE`).
