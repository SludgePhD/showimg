mod ratio;

use std::{
    cmp, env,
    ffi::OsStr,
    fs::{self, File},
    io::BufReader,
    mem,
    path::Path,
    process,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{bail, Context};
use image::{
    codecs::{gif::GifDecoder, png::PngDecoder},
    AnimationDecoder, Delay, Frame, ImageFormat,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use wgpu::{
    util::{BufferInitDescriptor, DeviceExt},
    CompositeAlphaMode,
};
use winit::{
    application::ApplicationHandler,
    dpi::{PhysicalPosition, PhysicalSize},
    event::{ElementState, KeyEvent, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy},
    keyboard::{KeyCode, PhysicalKey},
    window::{CursorIcon, ResizeDirection, Window, WindowId, WindowLevel},
};

const WIN_WIDTH: u32 = 1280;
const WIN_HEIGHT: u32 = 720;

/// Width of the border around the window contents within which the window gets
/// resized instead of moved.
const RESIZE_BORDER_WIDTH: f64 = 15.0;

/// Size of the checkerboard pattern cells (in screen pixels).
const CHECKERBOARD_CELL_SIZE: u32 = 10;

/// Hovering over the window while it is displaying a transparent image will display the
/// checkerboard pattern with this alpha value.
///
/// Only does anything if the compositor supports compositing client surfaces that use premultiplied
/// alpha.
const CHECKERBOARD_HOVER_ALPHA: f32 = 0.2;

// Gray levels for the 2 checkerboard squares. Linear luminance.
const CHECKERBOARD_LIGHT_A: f32 = 0.75;
const CHECKERBOARD_LIGHT_B: f32 = 0.95;
const CHECKERBOARD_DARK_A: f32 = 0.01;
const CHECKERBOARD_DARK_B: f32 = 0.06;

const SELECTION_COLOR: [f32; 4] = [0.2, 0.5, 0.5, 0.1];

const SUPPORTED_ALPHA_MODES: &[CompositeAlphaMode] = if cfg!(windows) {
    // On Windows, wgpu only seems to support pre-multiplied alpha with the `Inherit` mode.
    // FIXME: remove this when wgpu fixes this https://github.com/gfx-rs/wgpu/issues/3486
    &[
        CompositeAlphaMode::PreMultiplied,
        CompositeAlphaMode::Inherit,
    ]
} else {
    &[CompositeAlphaMode::PreMultiplied]
};

/// Texture format used during rendering. Must match the format in `preprocess.wgsl`.
///
/// Since this needs to be a storage-compatible format, it can't be any of the `-srgb` formats.
const TEXTURE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

fn main() {
    match run() {
        Ok(()) => {}
        Err(e) => {
            eprintln!("Error: {e:#}");
            rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Error)
                .set_title(concat!(env!("CARGO_PKG_NAME"), " – error"))
                .set_description(format!("{e:#}"))
                .show();
            process::exit(1);
        }
    }
}

fn run() -> anyhow::Result<()> {
    env_logger::builder()
        .filter_module(env!("CARGO_CRATE_NAME"), log::LevelFilter::Debug)
        .parse_default_env()
        .init();

    let args = env::args_os().skip(1).collect::<Vec<_>>();
    let path = match &*args {
        [path] if path != "--help" => Path::new(path),
        _ => bail!(
            "Missing argument. Either drag an image file onto the application, register it as an \
            image file handler in your file manager, or invoke `{}` with a path on the command \
            line.",
            env!("CARGO_PKG_NAME"),
        ),
    };

    log::info!("opening '{}'", path.display());
    let metadata =
        fs::metadata(path).context(format!("Failed to open image file '{}'", path.display()))?;
    let kb = metadata.len() / 1024;

    let start = Instant::now();
    let reader = BufReader::new(File::open(path)?);
    // FIXME: `ImageFormat::from_path` doesn't recognize `.apng`
    // https://github.com/image-rs/image/pull/2264
    let format = if path.extension() == Some(OsStr::new("apng")) {
        ImageFormat::Png
    } else {
        ImageFormat::from_path(path)?
    };
    let frames = match format {
        ImageFormat::Png => {
            let dec = PngDecoder::new(reader)?;
            if dec.is_apng()? {
                dec.apng()?.into_frames().collect_frames()?
            } else {
                // It's awkward to get a normal fucking image from a `PngDecoder` for some reason,
                // so just use the `image::load` API.
                vec![Frame::new(image::open(path)?.into_rgba8())]
            }
        }
        ImageFormat::Gif => GifDecoder::new(reader)?.into_frames().collect_frames()?,
        // FIXME: https://github.com/image-rs/image/issues/2263
        //ImageFormat::WebP => WebPDecoder::new(reader)?.into_frames().collect_frames()?,
        _ => vec![Frame::new(image::open(path)?.into_rgba8())],
    };
    assert!(!frames.is_empty());

    for frame in &frames {
        if frame.top() != 0 || frame.left() != 0 {
            bail!("`showimg` does not support animations with per-frame pixel offsets");
        }
    }
    for win in frames.windows(2) {
        let (a, b) = (&win[0], &win[1]);
        if a.buffer().width() != b.buffer().width() || a.buffer().height() != b.buffer().height() {
            bail!("`showimg` does not support animations with dynamic frame sizes");
        }
    }

    let what = if frames.len() == 1 {
        "image"
    } else {
        "animation"
    };
    let image = frames[0].buffer();
    let image_width = image.width();
    let image_height = image.height();
    let image_aspect_ratio = image_width as f32 / image_height as f32;
    log::debug!(
        "loaded {}x{} {what} from {} KiB file in {:.02?} (aspect ratio {}; memsize {} KiB per frame; {} frames)",
        image.width(),
        image.height(),
        kb,
        start.elapsed(),
        image_aspect_ratio,
        (image.width() * image.height() * 4) / 1024,
        frames.len(),
    );
    let mut images = Vec::new();
    let mut delays = Vec::new();
    for frame in frames {
        delays.push(frame.delay());
        images.push(frame.into_buffer());
    }

    let title = match path.file_name() {
        Some(name) => name.to_string_lossy(),
        None => path.to_string_lossy(),
    };

    let event_loop = EventLoop::builder().build()?;
    let proxy = event_loop.create_proxy();

    event_loop.run_app(&mut App {
        frame_count: images.len(),
        image_aspect_ratio,
        image_width,
        image_height,
        images,
        delays: Some((proxy, delays)),
        title: title.into(),
        ..App::default()
    })?;

    Ok(())
}

struct Win {
    supports_alpha: bool,
    image_info: ImageInfo,
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,

    /// The main render pipeline that displays the viewed image.
    display_pipeline: wgpu::RenderPipeline,
    /// Uniform buffer containing the [`DisplaySettings`].
    display_settings: wgpu::Buffer,
    /// One `BindGroup` per animation frame.
    display_bind_groups: Vec<wgpu::BindGroup>,
}

#[derive(Default)]
struct App {
    image_aspect_ratio: f32, // full image aspect ratio; never changes
    aspect_ratio: f32,       // selection aspect ratio
    /// Frame data; cleared during startup.
    images: Vec<image::RgbaImage>,
    delays: Option<(EventLoopProxy<()>, Vec<Delay>)>,
    image_width: u32,
    image_height: u32,
    frame_index: usize,
    frame_count: usize,
    title: String,
    instance: wgpu::Instance,
    window: Option<Win>,
    min_uv: [f32; 2],
    max_uv: [f32; 2],
    cursor_pos: Option<PhysicalPosition<f64>>, // None = cursor left
    cursor_mode: CursorMode,
    transparency: TransparencyMode,
    filter: FilterMode,
}

#[derive(Default, Clone, Copy)]
enum CursorMode {
    #[default]
    Move,
    Resize(ResizeDirection),
    Select(PhysicalPosition<f64>),
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
enum TransparencyMode {
    #[default]
    TrueTransparency,
    LightCheckerboard,
    DarkCheckerboard,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
enum FilterMode {
    #[default]
    Smart,
    Linear,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            let images = mem::take(&mut self.images);
            let win = self.create_window(event_loop, images);
            if !win.supports_alpha {
                self.transparency = TransparencyMode::LightCheckerboard;
            }
            let window = win.window.clone();
            self.window = Some(win);

            self.reset_region();

            if let Some((proxy, delays)) = mem::take(&mut self.delays) {
                if delays.len() <= 1 {
                    return;
                }

                thread::spawn(move || {
                    log::debug!("starting animation thread");
                    for delay in delays.iter().cycle() {
                        thread::sleep(Duration::from(*delay));
                        let Ok(()) = proxy.send_event(()) else { break };
                        window.request_redraw();
                    }
                });
            }
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: ()) {
        // The animation thread sends a user event every time the current frame's delay expires.
        self.frame_index = (self.frame_index + 1) % self.frame_count;
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(win) = &self.window else { return };
        if window_id != win.window.id() {
            return;
        }

        match event {
            WindowEvent::Resized(size) => {
                // When the window is resized, we force it to have the same aspect ratio as the
                // image it is displaying.
                log::trace!("resized to {}x{}", size.width, size.height);
                self.enforce_aspect_ratio(win, size);
            }
            WindowEvent::RedrawRequested => {
                self.redraw(win);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => match self.cursor_mode {
                CursorMode::Move => {
                    if let Err(e) = win.window.drag_window() {
                        log::error!("failed to initiate window move: {e}");
                    }
                }
                CursorMode::Resize(dir) => {
                    if let Err(e) = win.window.drag_resize_window(dir) {
                        log::error!("failed to initiate window resize: {e}");
                    }
                }
                CursorMode::Select(_) => {}
            },
            WindowEvent::MouseInput {
                button: MouseButton::Middle,
                state,
                ..
            } => match state {
                ElementState::Pressed => {
                    if let Some(pos) = self.cursor_pos {
                        self.cursor_mode = CursorMode::Select(pos);
                        self.update_cursor();
                        win.window.request_redraw();
                    }
                }
                ElementState::Released => {
                    // Commit area selection, compute new aspect ratio, and enforce it.
                    let (min, max) = self.selection_region(win);
                    let range = [max[0] - min[0], max[1] - min[1]];
                    if range[0] > 0.0 && range[1] > 0.0 {
                        // Valid (ish?) range
                        self.min_uv = min;
                        self.max_uv = max;
                        self.aspect_ratio = self.image_aspect_ratio * (range[0] / range[1]);

                        // Also downsize the window, since this is largely intended to be a cropping tool.
                        if let (CursorMode::Select(start), Some(end)) =
                            (self.cursor_mode, self.cursor_pos)
                        {
                            // sort corners
                            let min = [f64::min(start.x, end.x), f64::min(start.y, end.y)];
                            let max = [f64::max(start.x, end.x), f64::max(start.y, end.y)];
                            let size = [max[0] - min[0], max[1] - min[1]];
                            let _ = win.window.request_inner_size(PhysicalSize::new(
                                size[0] as u32,
                                size[1] as u32,
                            ));
                        }
                    }

                    self.cursor_mode = CursorMode::Move;
                    self.update_cursor();
                    self.enforce_aspect_ratio(win, win.window.inner_size());
                    win.window.request_redraw();
                }
            },
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Right,
                ..
            } => {
                if let Some(pos) = self.cursor_pos {
                    win.window.show_window_menu(pos);
                }
            }
            WindowEvent::CursorLeft { .. } => {
                self.cursor_pos = None;
                win.window.request_redraw();
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_pos = Some(position);
                win.window.request_redraw();

                if let CursorMode::Select(_) = self.cursor_mode {
                    // We're already doing something, don't change to move/resize mode.
                    return;
                }

                let inner_size = win.window.inner_size().cast::<f64>();
                let (n, e, s, w) = (
                    position.y <= RESIZE_BORDER_WIDTH,
                    position.x >= inner_size.width - RESIZE_BORDER_WIDTH,
                    position.y >= inner_size.height - RESIZE_BORDER_WIDTH,
                    position.x <= RESIZE_BORDER_WIDTH,
                );

                self.cursor_mode = match (n, e, s, w) {
                    (false, false, false, false) => CursorMode::Move,
                    (true, false, false, false) => CursorMode::Resize(ResizeDirection::North),
                    (true, true, false, false) => CursorMode::Resize(ResizeDirection::NorthEast),
                    (true, false, false, true) => CursorMode::Resize(ResizeDirection::NorthWest),
                    (false, false, true, false) => CursorMode::Resize(ResizeDirection::South),
                    (false, true, true, false) => CursorMode::Resize(ResizeDirection::SouthEast),
                    (false, false, true, true) => CursorMode::Resize(ResizeDirection::SouthWest),
                    (false, true, false, false) => CursorMode::Resize(ResizeDirection::East),
                    (false, false, false, true) => CursorMode::Resize(ResizeDirection::West),
                    // Ambiguous cases. These can happen when the window is so small that the resize
                    // borders overlap. Result is mostly arbitrary.
                    (false, true, true, true) => CursorMode::Resize(ResizeDirection::South),
                    (false, true, false, true) => CursorMode::Resize(ResizeDirection::West),
                    (true, false, true, _) => CursorMode::Resize(ResizeDirection::South),
                    (true, true, true, _) => CursorMode::Resize(ResizeDirection::SouthEast),
                    (true, true, false, true) => CursorMode::Resize(ResizeDirection::NorthEast),
                };

                self.update_cursor();
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state: ElementState::Pressed,
                        physical_key: PhysicalKey::Code(code),
                        ..
                    },
                ..
            } => match code {
                KeyCode::Escape => {
                    log::info!("escape pressed -> exiting");
                    event_loop.exit();
                }
                KeyCode::Backspace => {
                    log::info!("backspace pressed -> resetting zoom region");
                    self.reset_region();
                }
                KeyCode::KeyT => {
                    self.transparency = match self.transparency {
                        TransparencyMode::TrueTransparency => TransparencyMode::LightCheckerboard,
                        TransparencyMode::LightCheckerboard => TransparencyMode::DarkCheckerboard,
                        TransparencyMode::DarkCheckerboard => {
                            if win.supports_alpha {
                                TransparencyMode::TrueTransparency
                            } else {
                                TransparencyMode::LightCheckerboard
                            }
                        }
                    };
                    log::debug!("T -> cycling transparency mode to {:?}", self.transparency);
                    win.window.request_redraw();
                }
                KeyCode::KeyL => {
                    self.filter = match self.filter {
                        FilterMode::Smart => FilterMode::Linear,
                        FilterMode::Linear => FilterMode::Smart,
                    };
                    log::debug!("T -> cycling filter mode to {:?}", self.filter);
                    win.window.request_redraw();
                }
                _ => {}
            },
            WindowEvent::CloseRequested => {
                log::info!("external close request");
                event_loop.exit();
            }
            _ => {}
        }
    }
}

impl App {
    fn update_cursor(&self) {
        let Some(win) = &self.window else { return };
        let cursor = match self.cursor_mode {
            CursorMode::Move => CursorIcon::Grab,
            CursorMode::Resize(dir) => CursorIcon::from(dir),
            CursorMode::Select(_) => CursorIcon::Crosshair,
        };
        win.window.set_cursor(cursor);
    }

    fn enforce_aspect_ratio(&self, win: &Win, size: PhysicalSize<u32>) {
        // We use the `CursorMode` as a hint – if we're resizing vertically, respect the requested
        // height, if we're resizing horizontally, respect the requested width.
        let is_vertical = matches!(
            self.cursor_mode,
            CursorMode::Resize(ResizeDirection::North | ResizeDirection::South)
        );
        let fitted_size = if is_vertical {
            PhysicalSize::new(
                (size.height as f32 * self.aspect_ratio).round() as u32,
                size.height,
            )
        } else {
            PhysicalSize::new(
                size.width,
                (size.width as f32 / self.aspect_ratio).round() as u32,
            )
        };
        log::trace!(
            "enforce_aspect_ratio: requested {}x{}, fitted size {}x{} (vertical={is_vertical})",
            size.width,
            size.height,
            fitted_size.width,
            fitted_size.height,
        );

        ratio::enforce(&win.window, self.aspect_ratio, size);

        if fitted_size != size {
            let _ = win.window.request_inner_size(fitted_size);
        }
        self.recreate_swapchain(win);
        win.window.request_redraw();
    }

    fn reset_region(&mut self) {
        let Some(win) = &self.window else { return };
        if win.image_info.top == u32::MAX {
            // Somehow not a single non-transparent pixel in the image? good luck finding the window, fucker
            self.min_uv = [0.0, 0.0];
            self.max_uv = [1.0, 1.0];
            self.aspect_ratio = self.image_aspect_ratio;
        } else {
            self.min_uv = [
                win.image_info.left as f32 / self.image_width as f32,
                win.image_info.top as f32 / self.image_height as f32,
            ];
            self.max_uv = [
                win.image_info.right as f32 / self.image_width as f32,
                win.image_info.bottom as f32 / self.image_height as f32,
            ];
            let range = [
                self.max_uv[0] - self.min_uv[0],
                self.max_uv[1] - self.min_uv[1],
            ];
            // UVs always go from 0-1, so their "native" aspect ratio is 1.0.
            self.aspect_ratio = self.image_aspect_ratio * (range[0] / range[1]);
        }

        self.enforce_aspect_ratio(win, win.window.inner_size());
    }

    fn window_to_uv(&self, win: &Win, coords: PhysicalPosition<f64>) -> [f32; 2] {
        let (min, max) = self.fb_coord_range(win);
        let mut u = (coords.x as f32 - min[0]) / (max[0] - min[0]);
        let mut v = (coords.y as f32 - min[1]) / (max[1] - min[1]);

        // Adjust the raw UVs to take `min_uv` and `max_uv` into account.
        let u_range = self.max_uv[0] - self.min_uv[0];
        let v_range = self.max_uv[1] - self.min_uv[1];
        u = (u * u_range) + self.min_uv[0];
        v = (v * v_range) + self.min_uv[1];

        [u, v]
    }

    fn selection_region(&self, win: &Win) -> ([f32; 2], [f32; 2]) {
        if let (CursorMode::Select(start), Some(end)) = (self.cursor_mode, self.cursor_pos) {
            let start = self.window_to_uv(win, start);
            let end = self.window_to_uv(win, end);

            // sort corners
            let min = [f32::min(start[0], end[0]), f32::min(start[1], end[1])];
            let max = [f32::max(start[0], end[0]), f32::max(start[1], end[1])];

            // clamp to visible area
            let min = [
                f32::max(min[0], self.min_uv[0]),
                f32::max(min[1], self.min_uv[1]),
            ];
            let max = [
                f32::min(max[0], self.max_uv[0]),
                f32::min(max[1], self.max_uv[1]),
            ];

            (min, max)
        } else {
            Default::default()
        }
    }

    fn fb_coord_range(&self, win: &Win) -> ([f32; 2], [f32; 2]) {
        let size = win.window.inner_size();
        let to_aspect = size.width as f32 / size.height as f32;
        let (y_min, x_min, w, h);
        if self.aspect_ratio > to_aspect {
            w = size.width as f32;
            h = size.width as f32 / self.aspect_ratio;

            x_min = 0.0;
            y_min = (size.height as f32 - h) / 2.0;
        } else {
            w = size.height as f32 * self.aspect_ratio;
            h = size.height as f32;

            x_min = (size.width as f32 - w) / 2.0;
            y_min = 0.0;
        }

        ([x_min, y_min], [x_min + w, y_min + h])
    }

    fn display_settings(&self, win: &Win) -> DisplaySettings {
        let mut display_settings = DisplaySettings {
            min_fb: [0.0, 0.0],
            max_fb: [0.0, 0.0],
            min_uv: self.min_uv,
            max_uv: self.max_uv,
            min_selection: [0.0, 0.0],
            max_selection: [0.0, 0.0],
            selection_color: SELECTION_COLOR,
            checkerboard_a: [0.0; 4],
            checkerboard_b: [0.0; 4],
            checkerboard_res: CHECKERBOARD_CELL_SIZE,
            force_linear: 0,
            padding: Default::default(),
        };

        let (min, max) = self.fb_coord_range(win);

        display_settings.min_fb = min;
        display_settings.max_fb = max;

        let (min, max) = self.selection_region(win);
        display_settings.min_selection = min;
        display_settings.max_selection = max;

        match self.transparency {
            TransparencyMode::TrueTransparency => {
                if self.cursor_pos.is_some() {
                    // Partially transparent checkerboard while hovered.
                    let a = CHECKERBOARD_LIGHT_A * CHECKERBOARD_HOVER_ALPHA;
                    let b = CHECKERBOARD_LIGHT_B * CHECKERBOARD_HOVER_ALPHA;
                    display_settings.checkerboard_a = [a, a, a, CHECKERBOARD_HOVER_ALPHA];
                    display_settings.checkerboard_b = [b, b, b, CHECKERBOARD_HOVER_ALPHA];
                } else {
                    // Fully transparent.
                    display_settings.checkerboard_a = [0.0; 4];
                    display_settings.checkerboard_b = [0.0; 4];
                }
            }
            TransparencyMode::LightCheckerboard => {
                let a = CHECKERBOARD_LIGHT_A;
                let b = CHECKERBOARD_LIGHT_B;
                display_settings.checkerboard_a = [a, a, a, 1.0];
                display_settings.checkerboard_b = [b, b, b, 1.0];
            }
            TransparencyMode::DarkCheckerboard => {
                let a = CHECKERBOARD_DARK_A;
                let b = CHECKERBOARD_DARK_B;
                display_settings.checkerboard_a = [a, a, a, 1.0];
                display_settings.checkerboard_b = [b, b, b, 1.0];
            }
        }

        match self.filter {
            FilterMode::Smart => display_settings.force_linear = 0,
            FilterMode::Linear => display_settings.force_linear = 1,
        }

        display_settings
    }

    fn create_window(&self, event_loop: &ActiveEventLoop, images: Vec<image::RgbaImage>) -> Win {
        // Compute initial window size; fit aspect ratio.
        let s1 = PhysicalSize::new(
            (WIN_HEIGHT as f32 * self.image_aspect_ratio).round() as u32,
            WIN_HEIGHT,
        );
        let s2 = PhysicalSize::new(
            WIN_WIDTH,
            (WIN_WIDTH as f32 / self.image_aspect_ratio).round() as u32,
        );
        let fit_size = if s1.width > WIN_WIDTH || s1.height > WIN_HEIGHT {
            s2
        } else {
            s1
        };

        let mut size = fit_size;
        size.width = cmp::min(size.width, self.image_width);
        size.height = cmp::min(size.height, self.image_height);
        log::debug!(
            "window size: fit={}x{}, clamped={}x{}",
            fit_size.width,
            fit_size.height,
            size.width,
            size.height,
        );

        // Create Window.
        let app_name = env!("CARGO_PKG_NAME");
        let res = event_loop.create_window(
            Window::default_attributes()
                .with_inner_size(size)
                .with_title(format!("{} – {app_name}", self.title))
                .with_transparent(true)
                .with_decorations(false)
                .with_window_level(WindowLevel::AlwaysOnTop), // NB: doesn't work on Wayland
        );
        let window = match res {
            Ok(win) => Arc::new(win),
            Err(e) => {
                eprintln!("failed to create window: {e}");
                process::exit(1);
            }
        };

        // Log backend info.
        match window.window_handle() {
            Ok(h) => {
                let api = match h.as_raw() {
                    RawWindowHandle::UiKit(_) => "UIKit",
                    RawWindowHandle::AppKit(_) => "AppKit",
                    RawWindowHandle::Orbital(_) => "Orbital",
                    RawWindowHandle::Xlib(_) => "Xlib",
                    RawWindowHandle::Xcb(_) => "Xcb",
                    RawWindowHandle::Wayland(_) => "Wayland",
                    RawWindowHandle::Drm(_) => "DRM",
                    RawWindowHandle::Gbm(_) => "GBM",
                    RawWindowHandle::Win32(_) => "Win32",
                    RawWindowHandle::WinRt(_) => "WinRT",
                    RawWindowHandle::Web(_) => "Web",
                    RawWindowHandle::WebCanvas(_) => "WebCanvas",
                    RawWindowHandle::WebOffscreenCanvas(_) => "OffscreenCanvas",
                    RawWindowHandle::AndroidNdk(_) => "NDK",
                    RawWindowHandle::Haiku(_) => "Haiku",
                    _ => "<unknown>",
                };
                log::info!("using windowing API: {api}");
            }
            Err(e) => log::warn!("couldn't obtain window handle: {e}"),
        }

        let surface = match self.instance.create_surface(window.clone()) {
            Ok(surface) => surface,
            Err(e) => {
                eprintln!("failed to create surface: {e}");
                process::exit(1);
            }
        };

        // Open GPU.
        let adapter =
            pollster::block_on(self.instance.request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                power_preference: wgpu::PowerPreference::LowPower, // no need to spin up a dGPU for this workload
                ..Default::default()
            }));

        let Some(adapter) = adapter else {
            eprintln!("could not open any compatible graphics device");
            process::exit(1);
        };
        let info = adapter.get_info();
        log::info!(
            "using {} via {} ({}) [api={}]",
            info.name,
            info.driver,
            info.driver_info,
            info.backend,
        );
        let surface_caps = surface.get_capabilities(&adapter);
        log::debug!("supported surface formats: {:?}", surface_caps.formats);
        log::debug!("supported present modes: {:?}", surface_caps.present_modes);
        log::debug!("supported alpha modes: {:?}", surface_caps.alpha_modes);
        let supports_alpha = surface_caps
            .alpha_modes
            .iter()
            .any(|m| SUPPORTED_ALPHA_MODES.contains(m));
        let surface_format = *surface_caps
            .formats
            .first()
            .expect("adapter cannot render to surface");

        let res = pollster::block_on(adapter.request_device(&Default::default(), None));
        let (device, queue) = match res {
            Ok((dev, q)) => (dev, q),
            Err(e) => {
                eprintln!("failed to request graphics device: {e}");
                process::exit(1);
            }
        };

        // Create GPU resources.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // A single `ImageInfo` is bound to the shader for every frame; this computes a conservative
        // result that takes all frames into account.
        let image_info = device.create_buffer_init(&BufferInitDescriptor {
            label: None,
            contents: bytemuck::bytes_of(&ImageInfo::default()),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        });
        let preprocess_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: TEXTURE_FORMAT,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let preprocess_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: None,
                layout: Some(
                    &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: None,
                        bind_group_layouts: &[&preprocess_bgl],
                        push_constant_ranges: &[],
                    }),
                ),
                module: &device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("preprocess.wgsl"),
                    source: wgpu::ShaderSource::Wgsl(include_str!("preprocess.wgsl").into()),
                }),
                entry_point: "preprocess",
                compilation_options: Default::default(),
                cache: None,
            });

        let display_settings = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: mem::size_of::<DisplaySettings>() as _,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let display_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        // Upload and preprocess frames.
        let mut display_bind_groups = Vec::new();
        let mut preprocess = Vec::new();
        for image in &images {
            let size = wgpu::Extent3d {
                width: image.width(),
                height: image.height(),
                depth_or_array_layers: 1,
            };
            let input_format = wgpu::TextureFormat::Rgba8UnormSrgb;
            let input_texture = device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: input_format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            queue.write_texture(
                input_texture.as_image_copy(),
                image,
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * self.image_width),
                    rows_per_image: None,
                },
                size,
            );

            let output_texture = device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: TEXTURE_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
                view_formats: &[],
            });
            let preprocess_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &preprocess_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(
                            &input_texture.create_view(&Default::default()),
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(
                            &output_texture.create_view(&Default::default()),
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Buffer(
                            image_info.as_entire_buffer_binding(),
                        ),
                    },
                ],
            });
            preprocess.push(preprocess_bind_group);

            let display_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &display_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(
                            &output_texture.create_view(&Default::default()),
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Buffer(
                            display_settings.as_entire_buffer_binding(),
                        ),
                    },
                ],
            });

            display_bind_groups.push(display_bind_group);
        }

        let mut enc = device.create_command_encoder(&Default::default());
        let mut pass = enc.begin_compute_pass(&Default::default());
        for (image, preprocess_bind_group) in images.iter().zip(&preprocess) {
            /// Must match `preprocess.wgsl`.
            const WORKGROUP_SIZE: u32 = 16;
            let workgroups_x = (image.width() + WORKGROUP_SIZE - 1) / WORKGROUP_SIZE;
            let workgroups_y = (image.height() + WORKGROUP_SIZE - 1) / WORKGROUP_SIZE;
            pass.set_pipeline(&preprocess_pipeline);
            pass.set_bind_group(0, preprocess_bind_group, &[]);
            pass.dispatch_workgroups(workgroups_x, workgroups_y, 1);
        }
        drop(pass);

        // Copy the computed image information to a staging buffer.
        let image_info_dl = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: image_info.size(),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        enc.copy_buffer_to_buffer(&image_info, 0, &image_info_dl, 0, image_info.size());

        let idx = queue.submit([enc.finish()]);

        image_info_dl
            .slice(..)
            .map_async(wgpu::MapMode::Read, Result::unwrap);
        device
            .poll(wgpu::Maintain::wait_for(idx))
            .panic_on_timeout();

        let image_info: ImageInfo =
            *bytemuck::from_bytes(&image_info_dl.slice(..).get_mapped_range());

        log::debug!(
            "left={} top={} right={} bottom={}",
            image_info.left,
            image_info.top,
            image_info.right,
            image_info.bottom,
        );
        log::debug!(
            "uses_alpha={} known_straight={}",
            image_info.uses_alpha(),
            image_info.known_straight(),
        );
        if image_info.uses_alpha() && !supports_alpha {
            log::warn!(
                "compositor does not support premultiplied alpha; using checkerboard background"
            );
        }
        if image_info.uses_partial_alpha() && !image_info.known_straight() {
            log::warn!("image uses alpha channel, but may already be premultiplied; artifacts are possible");
        }

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("display.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("display.wgsl").into()),
        });
        let display_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: None,
                    bind_group_layouts: &[&display_bgl],
                    push_constant_ranges: &[],
                }),
            ),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vertex",
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fragment",
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState::from(surface_format))],
            }),
            multiview: None,
            cache: None,
        });

        let win = Win {
            supports_alpha,
            image_info,
            window,
            surface,
            adapter,
            device,
            queue,
            display_pipeline,
            display_settings,
            display_bind_groups,
        };
        self.recreate_swapchain(&win);
        win
    }

    fn recreate_swapchain(&self, win: &Win) {
        let res = win.window.inner_size();

        let caps = win.surface.get_capabilities(&win.adapter);
        let mut config = win
            .surface
            .get_default_config(&win.adapter, res.width, res.height)
            .expect("adapter does not support surface");

        for mode in SUPPORTED_ALPHA_MODES {
            if caps.alpha_modes.contains(mode) {
                config.alpha_mode = *mode;
                break;
            }
        }

        log::trace!(
            "creating target surface at {}x{} (format: {:?}, present mode: {:?}, alpha mode: {:?})",
            res.width,
            res.height,
            config.format,
            config.present_mode,
            config.alpha_mode,
        );

        win.surface.configure(&win.device, &config);
    }

    fn redraw(&self, win: &Win) {
        let st = match win.surface.get_current_texture() {
            Ok(st) => st,
            Err(err @ (wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost)) => {
                log::debug!("surface error: {}", err);
                self.recreate_swapchain(win);
                win.surface
                    .get_current_texture()
                    .expect("failed to acquire next frame after recreating swapchain")
            }
            Err(e) => {
                panic!("failed to acquire frame: {}", e);
            }
        };
        let view = st.texture.create_view(&Default::default());

        let display_settings = self.display_settings(win);
        win.queue.write_buffer(
            &win.display_settings,
            0,
            bytemuck::bytes_of(&display_settings),
        );

        let mut enc = win.device.create_command_encoder(&Default::default());
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&win.display_pipeline);
        pass.set_bind_group(0, &win.display_bind_groups[self.frame_index], &[]);
        pass.draw(0..4, 0..1);
        drop(pass);

        win.queue.submit([enc.finish()]);
        win.window.pre_present_notify();
        st.present();
    }
}

#[derive(Clone, Copy, bytemuck::NoUninit)]
#[repr(C)]
struct DisplaySettings {
    min_fb: [f32; 2],
    max_fb: [f32; 2],
    min_uv: [f32; 2],
    max_uv: [f32; 2],
    min_selection: [f32; 2],
    max_selection: [f32; 2],
    selection_color: [f32; 4],
    checkerboard_a: [f32; 4],
    checkerboard_b: [f32; 4],
    checkerboard_res: u32,
    force_linear: u32,
    padding: [u32; 2],
}

#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
struct ImageInfo {
    uses_alpha: u32,
    uses_partial_alpha: u32,
    known_straight: u32,
    // X/Y pixel coordinates where the image's content begins
    top: u32,
    right: u32,
    bottom: u32,
    left: u32,
}

impl Default for ImageInfo {
    fn default() -> Self {
        Self {
            uses_alpha: 0,
            uses_partial_alpha: 0,
            known_straight: 0,
            top: u32::MAX,
            right: 0,
            bottom: 0,
            left: u32::MAX,
        }
    }
}

impl ImageInfo {
    fn uses_alpha(&self) -> bool {
        self.uses_alpha != 0
    }

    fn uses_partial_alpha(&self) -> bool {
        self.uses_partial_alpha != 0
    }

    fn known_straight(&self) -> bool {
        self.known_straight != 0
    }
}
