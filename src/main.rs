use std::{env, fs, mem, path::Path, process, sync::Arc, time::Instant};

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use wgpu::util::{BufferInitDescriptor, DeviceExt};
use winit::{
    application::ApplicationHandler,
    dpi::{PhysicalPosition, PhysicalSize},
    event::{ElementState, KeyEvent, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    window::{CursorIcon, ResizeDirection, Window, WindowId, WindowLevel},
};

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

// Gray levels for the 2 checkerboard squares.
const CHECKERBOARD_COLOR_A: f32 = 0.3;
const CHECKERBOARD_COLOR_B: f32 = 0.6;
const SELECTION_COLOR: [f32; 4] = [0.2, 0.5, 0.5, 0.1];

fn main() -> anyhow::Result<()> {
    env_logger::builder()
        .filter_module(env!("CARGO_CRATE_NAME"), log::LevelFilter::Debug)
        .parse_default_env()
        .init();

    let args = env::args_os().skip(1).collect::<Vec<_>>();
    let path = match &*args {
        [path] if path != "--help" => Path::new(path),
        _ => {
            eprintln!("usage: showimg <path>");
            process::exit(1);
        }
    };

    log::info!("opening '{}'", path.display());
    let kb = fs::metadata(path)?.len() / 1024;

    let start = Instant::now();
    let image = image::open(path)?.into_rgba8();
    let image_aspect_ratio = image.width() as f32 / image.height() as f32;
    log::debug!(
        "loaded {}x{} image from {} KiB file in {:.02?} (aspect ratio {}; memsize {} KiB)",
        image.width(),
        image.height(),
        kb,
        start.elapsed(),
        image_aspect_ratio,
        (image.width() * image.height() * 4) / 1024,
    );

    let title = match path.file_name() {
        Some(name) => name.to_string_lossy(),
        None => path.to_string_lossy(),
    };

    let event_loop = EventLoop::builder().build()?;

    event_loop.run_app(&mut App {
        image_aspect_ratio,
        image,
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
    display_settings: wgpu::Buffer,
    display_bind_group: wgpu::BindGroup,
}

#[derive(Default)]
struct App {
    image_aspect_ratio: f32, // full image aspect ratio; never changes
    aspect_ratio: f32,       // selection aspect ratio
    image: image::RgbaImage,
    title: String,
    instance: wgpu::Instance,
    window: Option<Win>,
    min_uv: [f32; 2],
    max_uv: [f32; 2],
    cursor_pos: Option<PhysicalPosition<f64>>, // None = cursor left
    cursor_mode: CursorMode,
}

#[derive(Default, Clone, Copy)]
enum CursorMode {
    #[default]
    Move,
    Resize(ResizeDirection),
    Select(PhysicalPosition<f64>),
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            let win = self.create_window(event_loop);
            self.window = Some(win);

            self.reset_region();
        }
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
                        win.window.set_cursor(CursorIcon::Crosshair);
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
                    win.window.set_cursor(CursorIcon::Move);
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

                match self.cursor_mode {
                    CursorMode::Move => win.window.set_cursor(CursorIcon::Move),
                    CursorMode::Resize(dir) => win.window.set_cursor(CursorIcon::from(dir)),
                    _ => {}
                }
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
    // FIXME: does not work in X11, try getting rid of `drag_resize_window`
    fn enforce_aspect_ratio(&self, win: &Win, size: PhysicalSize<u32>) {
        // We use the `CursorMode` as a hint – if we're resizing vertically, respect the requested
        // height, if we're resizing horizontally, respect the requested width.
        let is_vertical = matches!(
            self.cursor_mode,
            CursorMode::Resize(ResizeDirection::North | ResizeDirection::South)
        );
        let fitted_size = if is_vertical {
            PhysicalSize::new((size.height as f32 * self.aspect_ratio) as u32, size.height)
        } else {
            PhysicalSize::new(size.width, (size.width as f32 / self.aspect_ratio) as u32)
        };
        log::trace!(
            "enforce_aspect_ratio: requested {}x{}, fitted size {}x{} (vertical={is_vertical})",
            size.width,
            size.height,
            fitted_size.width,
            fitted_size.height,
        );

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
                win.image_info.left as f32 / self.image.width() as f32,
                win.image_info.top as f32 / self.image.height() as f32,
            ];
            self.max_uv = [
                win.image_info.right as f32 / self.image.width() as f32,
                win.image_info.bottom as f32 / self.image.height() as f32,
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
        let size = win.window.inner_size();
        let mut u = (coords.x / f64::from(size.width)) as f32;
        let mut v = (coords.y / f64::from(size.height)) as f32;

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

    fn display_settings(&self, win: &Win) -> DisplaySettings {
        let mut display_settings = DisplaySettings {
            min_uv: self.min_uv,
            max_uv: self.max_uv,
            min_selection: [0.0, 0.0],
            max_selection: [0.0, 0.0],
            selection_color: SELECTION_COLOR,
            checkerboard_a: [
                CHECKERBOARD_COLOR_A,
                CHECKERBOARD_COLOR_A,
                CHECKERBOARD_COLOR_A,
                1.0,
            ],
            checkerboard_b: [
                CHECKERBOARD_COLOR_B,
                CHECKERBOARD_COLOR_B,
                CHECKERBOARD_COLOR_B,
                1.0,
            ],
            checkerboard_res: CHECKERBOARD_CELL_SIZE,
            padding: Default::default(),
        };

        let (min, max) = self.selection_region(win);
        display_settings.min_selection = min;
        display_settings.max_selection = max;

        if win.supports_alpha {
            if self.cursor_pos.is_some() {
                // Partially transparent checkerboard while hovered.
                let a = CHECKERBOARD_COLOR_A * CHECKERBOARD_HOVER_ALPHA;
                let b = CHECKERBOARD_COLOR_B * CHECKERBOARD_HOVER_ALPHA;
                display_settings.checkerboard_a = [a, a, a, CHECKERBOARD_HOVER_ALPHA];
                display_settings.checkerboard_b = [b, b, b, CHECKERBOARD_HOVER_ALPHA];
            } else {
                // Fully transparent.
                display_settings.checkerboard_a = [0.0; 4];
                display_settings.checkerboard_b = [0.0; 4];
            }
        }

        display_settings
    }

    fn create_window(&self, event_loop: &ActiveEventLoop) -> Win {
        // Create Window.
        let app_name = env!("CARGO_PKG_NAME");
        let res = event_loop.create_window(
            Window::default_attributes()
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
            .contains(&wgpu::CompositeAlphaMode::PreMultiplied);
        let surface_format = *surface_caps
            .formats
            .get(0)
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
        let size = wgpu::Extent3d {
            width: self.image.width(),
            height: self.image.height(),
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
            &self.image,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(4 * self.image.width()),
                rows_per_image: None,
            },
            size,
        );

        // Preprocess the image.
        let output_format = wgpu::TextureFormat::Rgba16Float;
        let output_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size,
            mip_level_count: 1, // TODO
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: output_format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
            view_formats: &[],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(include_str!("preprocess.wgsl").into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
                        format: output_format,
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
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: None,
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: None,
                    bind_group_layouts: &[&bgl],
                    push_constant_ranges: &[],
                }),
            ),
            module: &shader,
            entry_point: "preprocess",
            compilation_options: Default::default(),
        });
        let image_info = device.create_buffer_init(&BufferInitDescriptor {
            label: None,
            contents: bytemuck::bytes_of(&ImageInfo::default()),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &bgl,
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
                    resource: wgpu::BindingResource::Buffer(image_info.as_entire_buffer_binding()),
                },
            ],
        });
        const WORKGROUP_SIZE: u32 = 16;
        let workgroups_x = (self.image.width() + WORKGROUP_SIZE - 1) / WORKGROUP_SIZE;
        let workgroups_y = (self.image.height() + WORKGROUP_SIZE - 1) / WORKGROUP_SIZE;
        let mut enc = device.create_command_encoder(&Default::default());
        let mut pass = enc.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(workgroups_x, workgroups_y, 1);
        drop(pass);

        // Download the computed image info.
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

        if image_info.uses_alpha() && !supports_alpha {
            log::warn!(
                "compositor does not support premultiplied alpha; using checkerboard background"
            );
        }
        if image_info.uses_alpha() && !image_info.known_straight() {
            log::warn!("image uses alpha channel, but may already be premultiplied; artifacts are possible");
        }

        // Create the resources used for displaying the image.
        let display_settings = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: mem::size_of::<DisplaySettings>() as _,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
        let display_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &bgl,
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

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(include_str!("display.wgsl").into()),
        });
        let display_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: None,
                    bind_group_layouts: &[&bgl],
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
            display_bind_group,
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

        if caps
            .alpha_modes
            .contains(&wgpu::CompositeAlphaMode::PreMultiplied)
        {
            config.alpha_mode = wgpu::CompositeAlphaMode::PreMultiplied;
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
        pass.set_bind_group(0, &win.display_bind_group, &[]);
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
    min_uv: [f32; 2],
    max_uv: [f32; 2],
    min_selection: [f32; 2],
    max_selection: [f32; 2],
    selection_color: [f32; 4],
    checkerboard_a: [f32; 4],
    checkerboard_b: [f32; 4],
    checkerboard_res: u32,
    padding: [u32; 3],
}

#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
struct ImageInfo {
    uses_alpha: u32,
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

    fn known_straight(&self) -> bool {
        self.known_straight != 0
    }
}
