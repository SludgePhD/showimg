use std::sync::OnceLock;

use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use winit::{dpi::PhysicalSize, window::Window};

pub fn enforce(win: &Window, aspect_ratio: f32, _size: PhysicalSize<u32>) {
    let Ok(wh) = win.window_handle() else { return };
    let Ok(dh) = win.display_handle() else { return };
    match (wh.as_raw(), dh.as_raw()) {
        // cfg predicate copied from winit, keep in sync with Cargo.toml
        #[cfg(all(
            unix,
            not(any(
                target_os = "redox",
                target_family = "wasm",
                target_os = "android",
                target_os = "ios",
                target_os = "macos"
            ))
        ))]
        (RawWindowHandle::Xlib(wh), RawDisplayHandle::Xlib(dh)) => {
            use x11_dl::error::OpenError;
            use x11_dl::xlib::{PAspect, Xlib};

            static XLIB: OnceLock<Result<Xlib, OpenError>> = OnceLock::new();
            let Ok(xlib) = XLIB.get_or_init(|| Xlib::open()).as_ref() else {
                return;
            };

            let Some(display) = dh.display else { return };

            let num = 65536;
            let denom = (aspect_ratio * num as f32).round() as _;
            unsafe {
                let size_hints = (xlib.XAllocSizeHints)();
                if size_hints.is_null() {
                    return;
                }

                let mut supplied_return = 0;
                let status = (xlib.XGetWMNormalHints)(
                    display.as_ptr().cast(),
                    wh.window,
                    size_hints,
                    &mut supplied_return,
                );
                if status == 0 {
                    log::error!("`XGetWMNormalHints` failed!");
                    return;
                }

                // XWayland ignores these, because XWayland is very cool! Thanks, XWayland!
                // So, this is mostly untested.
                (*size_hints).min_aspect.x = num;
                (*size_hints).min_aspect.y = denom;
                (*size_hints).max_aspect.x = num;
                (*size_hints).max_aspect.y = denom;
                (*size_hints).flags |= PAspect;

                (xlib.XSetWMNormalHints)(display.as_ptr().cast(), wh.window, size_hints);

                (xlib.XFree)(size_hints.cast());
            }

            log::debug!("set X11 aspect ratio to {num}/{denom}");
        }
        _ => {}
    }
}
