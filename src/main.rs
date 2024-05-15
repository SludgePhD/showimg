use std::{env, fs, mem, path::Path, process, sync::Arc, time::Instant};

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
/// Only does anything if the compositor supports premultiplied alpha windows.
const CHECKERBOARD_HOVER_ALPHA: f32 = 0.2;

// Gray levels for the 2 checkerboard squares.
const CHECKERBOARD_COLOR_A: f32 = 0.3;
const CHECKERBOARD_COLOR_B: f32 = 0.6;

fn main() -> anyhow::Result<()> {
    env_logger::builder()
        .filter_module(env!("CARGO_CRATE_NAME"), log::LevelFilter::Debug)
        .parse_default_env()
        .init();

    let args = env::args_os().skip(1).collect::<Vec<_>>();
    let path = match &*args {
        [path] if path != "--help" => Path::new(path),
        _ => {
            eprintln!("usage: viewimg <path>");
            process::exit(1);
        }
    };

    log::info!("opening '{}'", path.display());
    let kb = fs::metadata(path)?.len() / 1024;

    let start = Instant::now();
    let image = image::open(path)?.into_rgba8();
    let aspect_ratio = image.width() as f32 / image.height() as f32;
    log::debug!(
        "loaded {}x{} image from {} KiB file in {:.02?} (aspect ratio {}; memsize {} KiB)",
        image.width(),
        image.height(),
        kb,
        start.elapsed(),
        aspect_ratio,
        (image.width() * image.height() * 4) / 1024,
    );

    let title = match path.file_name() {
        Some(name) => name.to_string_lossy(),
        None => path.to_string_lossy(),
    };

    let event_loop = EventLoop::builder().build()?;

    event_loop.run_app(&mut App {
        aspect_ratio,
        image,
        title: title.into(),
        ..App::default()
    })?;

    Ok(())
}

struct Win {
    supports_alpha: bool,
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
    aspect_ratio: f32,
    image: image::RgbaImage,
    title: String,
    instance: wgpu::Instance,
    window: Option<Win>,
    cursor_pos: Option<PhysicalPosition<f64>>, // None = cursor left
    cursor_mode: CursorMode,
    prev_size: PhysicalSize<u32>,
}

#[derive(Default)]
enum CursorMode {
    #[default]
    Move,
    Resize(ResizeDirection),
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            self.window = Some(self.create_window(event_loop));
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
                log::trace!(
                    "resized from {}x{} to {}x{}",
                    self.prev_size.width,
                    self.prev_size.height,
                    size.width,
                    size.height,
                );

                let ideal_size = if self.prev_size.height != size.height {
                    PhysicalSize::new((size.height as f32 * self.aspect_ratio) as u32, size.height)
                } else {
                    PhysicalSize::new(size.width, (size.width as f32 / self.aspect_ratio) as u32)
                };

                let _ = win.window.request_inner_size(ideal_size);
                self.recreate_swapchain(win);
                win.window.request_redraw();
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
                    self.prev_size = win.window.inner_size();
                    if let Err(e) = win.window.drag_resize_window(dir) {
                        log::error!("failed to initiate window resize: {e}");
                    }
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
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state: ElementState::Pressed,
                        physical_key: PhysicalKey::Code(KeyCode::Escape),
                        ..
                    },
                ..
            } => {
                log::info!("ESC pressed -> exiting");
                event_loop.exit();
            }
            _ => {}
        }
    }
}

impl App {
    fn create_window(&self, event_loop: &ActiveEventLoop) -> Win {
        // Create Window.
        let app_name = env!("CARGO_PKG_NAME");
        let res = event_loop.create_window(
            Window::default_attributes()
                .with_title(format!("{} – {app_name}", self.title))
                .with_transparent(true)
                .with_decorations(false)
                .with_window_level(WindowLevel::AlwaysOnTop),
        );
        let window = match res {
            Ok(win) => Arc::new(win),
            Err(e) => {
                eprintln!("failed to create window: {e}");
                process::exit(1);
            }
        };

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
        if !supports_alpha {
            log::info!(
                "compositor does not support premultiplied alpha; transparent images will not work"
            );
        }
        let surface_format = *surface_caps
            .formats
            .get(0)
            .expect("adapter cannot render to surface");

        let res =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default(), None));
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
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size,
            mip_level_count: 1, // TODO
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            texture.as_image_copy(),
            &self.image,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(4 * self.image.width()),
                rows_per_image: None,
            },
            size,
        );

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
                    visibility: wgpu::ShaderStages::FRAGMENT,
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
                        &texture.create_view(&Default::default()),
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
                entry_point: "vert",
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "frag",
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState::from(surface_format))],
            }),
            multiview: None,
        });

        let win = Win {
            supports_alpha,
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

        let mut display_settings = DisplaySettings {
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
        pass.draw(0..3, 0..1);
        drop(pass);

        win.queue.submit([enc.finish()]);
        win.window.pre_present_notify();
        st.present();
    }
}

#[derive(Clone, Copy, bytemuck::NoUninit)]
#[repr(C)]
struct DisplaySettings {
    checkerboard_a: [f32; 4],
    checkerboard_b: [f32; 4],
    checkerboard_res: u32,
    padding: [u32; 3],
}
