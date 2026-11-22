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
    /// Largest texture width or height
    pub fn max_texture_side(&self) -> u32 {
        self.device.limits().max_texture_dimension_2d
    }

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
                power_preference: if crate::settings::current().bool("PreferDedicatedGpu") {
                    wgpu::PowerPreference::HighPerformance
                } else {
                    wgpu::PowerPreference::LowPower
                },
                force_fallback_adapter: false,
                compatible_surface: None,
            })
            .await
            .map_err(|e| e.to_string())?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("lumaris"),
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
    /// checkerboard square size behind transparent parts (0 = none)
    checker: f32,
    /// rotation (bits 0-1, 90° steps clockwise) and horizontal flip (bit 2)
    orient: u32,
    alpha: f32,
    pad: [f32; 2],
}

/// An image drawn over the main image at a fixed position (menus)
struct Overlay {
    bind_group: wgpu::BindGroup,
    uniforms: wgpu::Buffer,
    rect: [f32; 4],
    size: (u32, u32),
}

struct ImageTexture {
    bind_group: wgpu::BindGroup,
    view: wgpu::TextureView,
    texture: wgpu::Texture,
    mips: u32,
    /// texture size
    tex_w: u32,
    tex_h: u32,
    /// image size it represents
    width: u32,
    height: u32,
    vector: bool,
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
    /// the previous image while fading to the new one: texture, position, zoom, orientation
    previous: Option<(ImageTexture, [f32; 4], f64, u32)>,
    previous_uniforms: wgpu::Buffer,
    last_draw: ([f32; 4], f64, u32),
    overlays: Vec<Overlay>,
    background: wgpu::Color,
    transparent: bool,
    pixel_art: (bool, u32),
    checkerboard: bool,
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
        if std::env::var_os("LUMARIS_DEBUG_TIMING").is_some() {
            eprintln!(
                "lumaris: {:?} {:?}, alpha modes {:?}",
                gpu.adapter.get_info().backend,
                gpu.adapter.get_info().name,
                caps.alpha_modes
            );
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
        let mip_layout = device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("mip"), entries: &[tex_entry(0), sampler_entry(1)] });

        let pipeline = make_pipeline(device, &layout, wgpu::include_wgsl!("shader.wgsl"), "vs_main", "fs_main", format, true);
        let mip_pipeline = make_pipeline(
            device,
            &mip_layout,
            wgpu::include_wgsl!("mip.wgsl"),
            "vs_full",
            "fs_mip",
            wgpu::TextureFormat::Rgba8Unorm,
            false,
        );

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
        let previous_uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("previous uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
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
            previous: None,
            previous_uniforms,
            last_draw: ([0.0; 4], 1.0, 0),
            overlays: Vec::new(),
            background: wgpu::Color::BLACK,
            transparent,
            pixel_art: (true, PIXEL_ART_MAX),
            checkerboard: false,
        })
    }

    /// Sharp square pixels when enlarging images up to `max` pixels wide and high
    pub fn set_pixel_art(&mut self, enabled: bool, max: u32) {
        self.pixel_art = (enabled, max);
    }

    /// Checkerboard behind transparent image parts
    pub fn set_checkerboard(&mut self, on: bool) {
        self.checkerboard = on;
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
        self.upload(img.pixel_width, img.pixel_height, &img.rgba, (img.width, img.height), img.vector.is_some());
    }

    /// Like set_image, but keeps the current image for a fade (render with `fade`).
    pub fn set_image_fading(&mut self, img: &Decoded) {
        if let Some(old) = self.image.take() {
            let bind_group = self.gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("previous"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: self.previous_uniforms.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&old.view) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.linear) },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.nearest) },
                ],
            });
            let (rect, zoom, orient) = self.last_draw;
            self.previous = Some((ImageTexture { bind_group, ..old }, rect, zoom, orient));
        }
        self.set_image(img);
    }

    pub fn end_fade(&mut self) {
        self.previous = None;
    }

    /// Next frame of an animation (same size as the current image).
    pub fn update_pixels(&mut self, rgba: &[u8]) {
        let Some(img) = &self.image else { return };
        if rgba.len() != (img.tex_w * img.tex_h * 4) as usize {
            return;
        }
        let size = wgpu::Extent3d { width: img.tex_w, height: img.tex_h, depth_or_array_layers: 1 };
        self.gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &img.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * img.tex_w), rows_per_image: Some(img.tex_h) },
            size,
        );
        self.generate_mips(&img.texture, img.mips);
    }

    /// `pixels`: width x height RGBA8; `logical`: the image size it represents (differs for
    /// vector images rendered at another scale).
    fn upload(&mut self, width: u32, height: u32, pixels: &[u8], logical: (u32, u32), vector: bool) {
        let max = self.gpu.device.limits().max_texture_dimension_2d;
        let scaled;
        let (w, h, pixels) = if width > max || height > max {
            // larger than the GPU supports: reduce once on the CPU
            let src = image::RgbaImage::from_raw(width, height, pixels.to_vec()).expect("image size");
            let s = (max as f64 / width.max(height) as f64).min(1.0);
            let (nw, nh) = (((width as f64 * s) as u32).max(1), ((height as f64 * s) as u32).max(1));
            scaled = image::imageops::resize(&src, nw, nh, image::imageops::FilterType::Triangle);
            (nw, nh, scaled.as_raw().as_slice())
        } else {
            (width, height, pixels)
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
        self.generate_mips(&texture, mips);
        let view = texture.create_view(&Default::default());
        let bind_group = self.gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("image"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.uniforms.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.linear) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.nearest) },
            ],
        });
        self.image =
            Some(ImageTexture { bind_group, view, texture, mips, tex_w: w, tex_h: h, width: logical.0, height: logical.1, vector });
    }

    /// Re-rendered vector image (same logical size, other pixel size).
    pub fn replace_vector_pixels(&mut self, img: &Decoded) {
        if self.image.as_ref().is_some_and(|i| i.vector && i.width == img.width && i.height == img.height) {
            self.set_image(img);
        }
    }

    /// Pixel size of the current image texture
    pub fn texture_size(&self) -> Option<(u32, u32)> {
        self.image.as_ref().map(|i| (i.tex_w, i.tex_h))
    }

    fn generate_mips(&self, texture: &wgpu::Texture, mips: u32) {
        let device = &self.gpu.device;
        let mut encoder = device.create_command_encoder(&Default::default());
        for level in 1..mips {
            let view =
                |l| texture.create_view(&wgpu::TextureViewDescriptor { base_mip_level: l, mip_level_count: Some(1), ..Default::default() });
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
    }

    /// Images drawn on top: premultiplied RGBA (pixels, width, height, x, y). Empty: none.
    pub fn set_overlays(&mut self, layers: &[(&[u8], u32, u32, i32, i32)]) {
        let device = &self.gpu.device;
        self.overlays = layers
            .iter()
            .map(|&(pixels, w, h, x, y)| {
                let size = wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 };
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("overlay"),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                self.gpu.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    pixels,
                    wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * w), rows_per_image: Some(h) },
                    size,
                );
                let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("overlay uniforms"),
                    size: std::mem::size_of::<Uniforms>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let view = texture.create_view(&Default::default());
                let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("overlay"),
                    layout: &self.layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: uniforms.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                        wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.linear) },
                        wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.nearest) },
                    ],
                });
                Overlay { bind_group, uniforms, rect: [x as f32, y as f32, (x + w as i32) as f32, (y + h as i32) as f32], size: (w, h) }
            })
            .collect();
    }

    pub fn clear_image(&mut self) {
        self.image = None;
    }

    /// Draws one frame. `rect`: image rectangle in window pixels, `zoom`: display zoom.
    /// `fade`: progress of a fade from the previous image (0..1), None: no fade
    pub fn render(&mut self, rect: [f32; 4], zoom: f64, orient: u32, fade: Option<f32>) {
        self.last_draw = (rect, zoom, orient);
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
            if let (Some((prev, prect, pzoom, porient)), Some(_)) = (&self.previous, fade) {
                let u = self.uniforms_for(prev, *prect, *pzoom, *porient, 1.0);
                self.gpu.queue.write_buffer(&self.previous_uniforms, 0, bytemuck::bytes_of(&u));
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &prev.bind_group, &[]);
                pass.draw(0..6, 0..1);
            }
            if let Some(img) = &self.image {
                let u = self.uniforms_for(img, rect, zoom, orient, fade.unwrap_or(1.0));
                self.gpu.queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&u));
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &img.bind_group, &[]);
                pass.draw(0..6, 0..1);
            }
            for o in &self.overlays {
                let u = Uniforms {
                    rect: o.rect,
                    viewport: [self.config.width as f32, self.config.height as f32],
                    tex_size: [o.size.0 as f32, o.size.1 as f32],
                    zoom: 1.0,
                    mode: 4,
                    lod: 0.0,
                    checker: 0.0,
                    orient: 0,
                    alpha: 1.0,
                    pad: [0.0; 2],
                };
                self.gpu.queue.write_buffer(&o.uniforms, 0, bytemuck::bytes_of(&u));
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &o.bind_group, &[]);
                pass.draw(0..6, 0..1);
            }
        }
        self.gpu.queue.submit([encoder.finish()]);
        self.window.pre_present_notify();
        frame.present();
    }
}

impl Renderer {
    fn uniforms_for(&self, img: &ImageTexture, rect: [f32; 4], zoom: f64, orient: u32, alpha: f32) -> Uniforms {
        let pixel_art = self.pixel_art.0 && !img.vector && img.width.max(img.height) <= self.pixel_art.1;
        // zoom relative to the texture pixels
        let zoom = zoom * img.width as f64 / img.tex_w as f64;
        let aligned = rect[0].fract() == 0.0 && rect[1].fract() == 0.0;
        let (mode, lod) = if (zoom - 1.0).abs() < 1e-6 && aligned {
            (3, 0.0)
        } else if zoom > 1.0 {
            (if pixel_art { 2 } else { 1 }, 0.0)
        } else {
            (0, ((-zoom.log2()) - 0.25).max(0.0) as f32)
        };
        Uniforms {
            rect,
            viewport: [self.config.width as f32, self.config.height as f32],
            tex_size: [img.tex_w as f32, img.tex_h as f32],
            zoom: zoom as f32,
            mode,
            lod,
            checker: if self.checkerboard { (8.0 * self.window.scale_factor()).round() as f32 } else { 0.0 },
            orient,
            alpha,
            pad: [0.0; 2],
        }
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
