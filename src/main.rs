use std::{env, path::Path, process, sync::Arc};

use winit::{
    application::ApplicationHandler,
    dpi::{PhysicalPosition, PhysicalSize},
    event::{ElementState, KeyEvent, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    window::{CursorIcon, ResizeDirection, Window, WindowId, WindowLevel},
};

fn main() -> anyhow::Result<()> {
    env_logger::builder()
        .filter_module(env!("CARGO_CRATE_NAME"), log::LevelFilter::Debug)
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
    let image = image::open(path)?.into_rgba8();
    let aspect_ratio = image.width() as f32 / image.height() as f32;
    log::debug!(
        "loaded {}x{} image (aspect ratio {})",
        image.width(),
        image.height(),
        aspect_ratio,
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
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
}

#[derive(Default)]
struct App {
    aspect_ratio: f32,
    image: image::RgbaImage,
    title: String,
    instance: wgpu::Instance,
    window: Option<Win>,
    cursor_pos: PhysicalPosition<f64>,
    cursor_mode: CursorMode,
    prev_size: PhysicalSize<u32>,
}

#[derive(Default)]
enum CursorMode {
    #[default]
    Move,
    Resize(ResizeDirection),
}

const RESIZE_BORDER_WIDTH: f64 = 10.0;

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
                log::debug!(
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
                win.window.show_window_menu(self.cursor_pos);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_pos = position;

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
                power_preference: wgpu::PowerPreference::LowPower,
                ..Default::default()
            }));

        let Some(adapter) = adapter else {
            eprintln!("could not open any compatible graphics device");
            process::exit(1);
        };
        let surface_caps = surface.get_capabilities(&adapter);
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
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
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
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(include_str!("display.wgsl").into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
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
            window,
            surface,
            adapter,
            device,
            queue,
            pipeline,
            bind_group,
        };
        self.recreate_swapchain(&win);
        win
    }

    fn recreate_swapchain(&self, win: &Win) {
        let res = win.window.inner_size();

        let config = win
            .surface
            .get_default_config(&win.adapter, res.width, res.height)
            .expect("adapter does not support surface");
        log::debug!(
            "creating target surface at {}x{} (format: {:?})",
            res.width,
            res.height,
            config.format,
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
        pass.set_pipeline(&win.pipeline);
        pass.set_bind_group(0, &win.bind_group, &[]);
        pass.draw(0..3, 0..1);
        drop(pass);

        win.queue.submit([enc.finish()]);
        st.present();
    }
}
