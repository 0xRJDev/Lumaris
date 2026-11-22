# Changelog

## Unreleased

- Fixed: dark fringes along transparent edges when zoomed (pixel art outlines, smooth zoom and
  zooming out). Images are now held on the GPU with premultiplied alpha
- Hidden images (name starting with a dot) are skipped while browsing; opening one directly still
  works. Setting: Navigation → Skip hidden images (on by default)
- Follows changes in the folder: images added or removed by other programs appear and disappear
  (Navigation → Follow changes in the folder)
- Set as wallpaper (right-click menu; KDE Plasma and GNOME)
- Faster browsing: the image you stop on is decoded first, images skipped past are not decoded
- Images larger than the GPU allows are reduced while decoding, so showing them no longer pauses
  the window
- Memory limit for images decoded in advance (Advanced → Memory for decoded images, 1 GB)
- The opened image starts decoding before its folder is listed (faster start in large folders)
- Faster sorting of large folders
- JPEG: decoded straight to RGBA, and on all cores at once when the file has restart markers at
  the end of rows (a 119 megapixel image: 370 ms to 50 ms)
- More file type names in the desktop entry (BMP, ICO, TGA, PSD, camera RAW variants), so file
  managers offer Lumaris for them

## 0.1.0

First release.

- GPU image viewer (wgpu: Vulkan, OpenGL fallback) for Wayland and X11, made for KDE Plasma
- Smooth zoom around the pointer, window that follows the zoom, pixel art and sharp SVG zoom
- Blurred see-through background (KWin), centred window placement (KWin script, nothing installed)
- Folder browsing in natural order, sort orders, slideshow with fades, comic archives
- Right-click menu in the desktop's style that can extend beyond the window
- Open with, open folder, copy image / path, move to trash, user commands
- Settings window, changeable keys, image info with EXIF and histogram, navigation panel
- Formats: JPEG, PNG, GIF, WebP, AVIF, HEIF, JPEG XL, SVG, camera RAW, EXR, HDR, PSD, XCF, Krita,
  OpenRaster, JPEG 2000, PDF, TIFF, BMP, QOI, ICO, TGA, PNM, DDS, farbfeld
