// GPU drawing with wgpu (Vulkan, OpenGL fallback).
use std::sync::Arc;

use winit::window::Window;

use crate::decode::Decoded;

/// Images up to this size (both sides) are pixel art: enlarged with square pixels.
const PIXEL_ART_MAX: u32 = 256;

pub struct Gpu {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

impl Gpu {
    /// Created before the window exists, in parallel with decoding the first image.
    /// Vulkan first; OpenGL only when there is no Vulkan driver (starting both costs ~50 ms).
    pub async fn new() -> Result<Gpu, String> {
        let mut last_error = String::from("no GPU found");
        for backends in [wgpu::Backends::VULKAN, wgpu::Backends::GL] {
            match Self::with_backends(backends).await {
                Ok(g) => return Ok(g),
                Err(e) => last_error = e,
            }
        }
        Err(last_error)
    }

    async fn with_backends(backends: wgpu::Backends) -> Result<Gpu, String> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor { backends, ..Default::default() });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: None,
            })
            .await
            .map_err(|e| e.to_string())?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("lumae"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                trace: wgpu::Trace::Off,
            })
            .await
            .map_err(|e| e.to_string())?;
        Ok(Gpu { instance, adapter, device, queue })
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    rect: [f32; 4],
    viewport: [f32; 2],
    tex_size: [f32; 2],
    zoom: f32,
    mode: u32,
    lod: f32,
    pad: f32,
}

struct ImageTexture {
    bind_group: wgpu::BindGroup,
    width: u32,
    height: u32,
}

pub struct Renderer {
    gpu: Gpu,
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    mip_pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    mip_layout: wgpu::BindGroupLayout,
    uniforms: wgpu::Buffer,
    linear: wgpu::Sampler,
    nearest: wgpu::Sampler,
    image: Option<ImageTexture>,
    background: wgpu::Color,
    transparent: bool,
}

impl Renderer {
    pub fn new(gpu: Gpu, window: Arc<Window>) -> Result<Renderer, String> {
        let surface = gpu.instance.create_surface(window.clone()).map_err(|e| e.to_string())?;
        let caps = surface.get_capabilities(&gpu.adapter);
        if caps.formats.is_empty() {
            return Err("the GPU cannot draw to this window".into());
        }
        // filter in the image's own (gamma) space, like other image viewers: no sRGB conversion
        let format = caps.formats.iter().copied().find(|f| !f.is_srgb()).unwrap_or(caps.formats[0]);
        // see-through window: needs a compositor that takes premultiplied alpha from Vulkan/GL
        if std::env::var_os("LUMAE_DEBUG_TIMING").is_some() {
            eprintln!("lumae: {:?} {:?}, alpha modes {:?}", gpu.adapter.get_info().backend, gpu.adapter.get_info().name, caps.alpha_modes);
        }
        let transparent = caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::PreMultiplied);
        let alpha_mode = if transparent { wgpu::CompositeAlphaMode::PreMultiplied } else { caps.alpha_modes[0] };
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode,
            view_formats: vec![],
        };
        surface.configure(&gpu.device, &config);

        let device = &gpu.device;
        let tex_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let sampler_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("image"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                tex_entry(1),
                sampler_entry(2),
                sampler_entry(3),
            ],
        });
        let mip_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mip"),
            entries: &[tex_entry(0), sampler_entry(1)],
        });

        let pipeline = make_pipeline(device, &layout, wgpu::include_wgsl!("shader.wgsl"), "vs_main", "fs_main", format, true);
        let mip_pipeline = make_pipeline(device, &mip_layout, wgpu::include_wgsl!("mip.wgsl"), "vs_full", "fs_mip", wgpu::TextureFormat::Rgba8Unorm, false);

        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = |filter| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                mag_filter: filter,
                min_filter: filter,
                mipmap_filter: filter,
                ..Default::default()
            })
        };
        let linear = sampler(wgpu::FilterMode::Linear);
        let nearest = sampler(wgpu::FilterMode::Nearest);

        Ok(Renderer {
            gpu,
            window,
            surface,
            config,
            pipeline,
            mip_pipeline,
            layout,
            mip_layout,
            uniforms,
            linear,
            nearest,
            image: None,
            background: wgpu::Color::BLACK,
            transparent,
        })
    }

    /// true when the window can be see-through
    pub fn transparent(&self) -> bool {
        self.transparent
    }

    /// Background colour (0..1) and how much it covers the desktop (0 = see-through, 1 = solid).
    /// Without transparency support the background is solid.
    pub fn set_background(&mut self, rgb: [f64; 3], opacity: f64) {
        let a = if self.transparent { opacity.clamp(0.0, 1.0) } else { 1.0 };
        self.background = wgpu::Color { r: rgb[0] * a, g: rgb[1] * a, b: rgb[2] * a, a };
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 || (width == self.config.width && height == self.config.height) {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.gpu.device, &self.config);
    }

    /// Uploads the image and builds its mipmaps on the GPU.
    pub fn set_image(&mut self, img: &Decoded) {
        let max = self.gpu.device.limits().max_texture_dimension_2d;
        let scaled;
        let (w, h, pixels) = if img.width > max || img.height > max {
            // larger than the GPU supports: reduce once on the CPU
            let src = image::RgbaImage::from_raw(img.width, img.height, img.rgba.clone()).expect("image size");
            let s = (max as f64 / img.width.max(img.height) as f64).min(1.0);
            let (nw, nh) = (((img.width as f64 * s) as u32).max(1), ((img.height as f64 * s) as u32).max(1));
            scaled = image::imageops::resize(&src, nw, nh, image::imageops::FilterType::Triangle);
            (nw, nh, scaled.as_raw().as_slice())
        } else {
            (img.width, img.height, img.rgba.as_slice())
        };
        let device = &self.gpu.device;
        let mips = 32 - w.max(h).leading_zeros();
        let size = wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("image"),
            size,
            mip_level_count: mips,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        self.gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            pixels,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * w), rows_per_image: Some(h) },
            size,
        );
        let mut encoder = device.create_command_encoder(&Default::default());
        for level in 1..mips {
            let view = |l| {
                texture.create_view(&wgpu::TextureViewDescriptor { base_mip_level: l, mip_level_count: Some(1), ..Default::default() })
            };
            let (src, dst) = (view(level - 1), view(level));
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &self.mip_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&src) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.linear) },
                ],
            });
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &dst,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.mip_pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        self.gpu.queue.submit([encoder.finish()]);

        let view = texture.create_view(&Default::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("image"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.uniforms.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.linear) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.nearest) },
            ],
        });
        // width/height: the original size, so the pixel art rule and zoom refer to the file
        self.image = Some(ImageTexture { bind_group, width: img.width, height: img.height });
    }

    pub fn clear_image(&mut self) {
        self.image = None;
    }

    /// Draws one frame. `rect`: image rectangle in window pixels, `zoom`: display zoom.
    pub fn render(&mut self, rect: [f32; 4], zoom: f64) {
        let frame = match self.surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost) => {
                self.surface.configure(&self.gpu.device, &self.config);
                self.window.request_redraw();
                return;
            }
            Err(_) => return,
        };
        let target = frame.texture.create_view(&Default::default());
        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(self.background), store: wgpu::StoreOp::Store },
                })],
                ..Default::default()
            });
            if let Some(img) = &self.image {
                let pixel_art = img.width.max(img.height) <= PIXEL_ART_MAX;
                let (mode, lod) = if (zoom - 1.0).abs() < 1e-9 {
                    (3, 0.0)
                } else if zoom > 1.0 {
                    (if pixel_art { 2 } else { 1 }, 0.0)
                } else {
                    (0, ((-zoom.log2()) - 0.25).max(0.0) as f32)
                };
                let u = Uniforms {
                    rect,
                    viewport: [self.config.width as f32, self.config.height as f32],
                    tex_size: [img.width as f32, img.height as f32],
                    zoom: zoom as f32,
                    mode,
                    lod,
                    pad: 0.0,
                };
                self.gpu.queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&u));
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &img.bind_group, &[]);
                pass.draw(0..6, 0..1);
            }
        }
        self.gpu.queue.submit([encoder.finish()]);
        self.window.pre_present_notify();
        frame.present();
    }
}

fn make_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    shader: wgpu::ShaderModuleDescriptor,
    vs: &str,
    fs: &str,
    format: wgpu::TextureFormat,
    blend: bool,
) -> wgpu::RenderPipeline {
    let module = device.create_shader_module(shader);
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &[layout],
        push_constant_ranges: &[],
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: None,
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState { module: &module, entry_point: Some(vs), compilation_options: Default::default(), buffers: &[] },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some(fs),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: if blend { Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING) } else { None },
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview: None,
        cache: None,
    })
}
