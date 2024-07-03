**showimg** is a funny little image viewer / overlay

it displays images without any window decorations and makes them stay on top of
other windows.

### controls

- escape: close window
- left mouse button: move window, or resize it at its border
- right click: open the OS context menu for the window
- middle click (hold): select a region to zoom into
- backspace: reset zoom region
- T: toggle window background for transparent images (transparent, light checkerboard, dark checkerboard)
- L: force linear interpolation even when each image pixel is larger than a screen pixel (by default, this transitions to pixel art friendly nearest-neighbor)

### bugs & todos

- test aspect-aware window resize logic on native X11 (doesn't work on XWayland) and Windows
- mipmaps and SPD
- HDR support? (I have no use for this, I neither have HDR images nor monitors)
- some Animated PNGs flicker because the `image` crate doesn't decode them right
- Ctrl + Drag should create a Drag&Drop source containing the image path (needs winit support first)

### license

https://github.com/SludgePhD/showimg/assets/96552222/b7c4d9ec-18f1-4a3d-9827-4522a84ce1b2
