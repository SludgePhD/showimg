# `showimg` – Image Overlay

**showimg** is a funny little image viewer that displays images without any window decorations and makes them stay on top of other windows.

### Controls

- Left Click: Move window, or resize it at its border
- Right Click: Open the OS context menu for the window
- Middle Click (hold): Select a region to zoom into
- <kbd>ESC</kbd> or <kbd>Q</kbd>: Close window
- <kbd>Backspace</kbd>: Reset zoom region
- <kbd>1</kbd>: Resize window to match image size exactly
- <kbd>T</kbd>: Toggle window background for transparent images (transparent, light checkerboard, dark checkerboard)
- <kbd>L</kbd>: Force linear interpolation even when each image pixel is larger than a screen pixel (by default, this transitions to pixel art friendly nearest-neighbor)
- <kbd>M</kbd>: Toggle the use of mipmaps

### Dependencies

On Linux, we (apparently!) need [`zenity`]. your distro should have it packaged.

[`zenity`]: https://gitlab.gnome.org/GNOME/zenity

### Limitations

- On Wayland, the window will not automatically stay on top of others.
  - Depending on your Wayland compositor, you can manually add a window rule that makes this work (eg. on KDE).
- On XWayland, the window cannot force its size to the image's aspect ratio, so there will be a transparent border if the aspect ratio doesn't match.
- No support for HDR images.

### License

https://github.com/SludgePhD/showimg/assets/96552222/b7c4d9ec-18f1-4a3d-9827-4522a84ce1b2
