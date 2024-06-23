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

### todo

- test aspect-aware window resize logic on native X11 (doesn't work on XWayland) and Windows
- mipmaps and SPD
- HDR question mark (I have no use for this, I neither have HDR images nor monitors)
- some Animated PNGs flicker because the `image` crate doesn't decode them right
- switch to nearest neighbor interpolation when zooming in beyond some threshold, like VS Code does

### license

https://github.com/SludgePhD/showimg/assets/96552222/b7c4d9ec-18f1-4a3d-9827-4522a84ce1b2
