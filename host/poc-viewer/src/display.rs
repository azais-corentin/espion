//! The image on the GPU: each new frame is uploaded as it came off the wire (a JPEG frame as
//! the decoding stage made it, RAW8), one byte per texel (`R8Uint`), and a fragment shader
//! turns it into grey: RAW8 directly, RAW10 unpacked from its CSI-2 5-byte groups at full
//! 10-bit depth. The UI thread does no per-pixel work; it only hands the frame's bytes to
//! `wgpu` (one copy into the staging buffer).
//!
//! The shader also applies the display gain (a brightness multiplier that leaves the data
//! alone) and samples nearest-neighbour, so 1:1 shows the sensor's pixels. With the threshold
//! mask on, it tints the pixels at or above the detection threshold of the frame shown
//! ([`Mask`]): compared at the texture's depth (RAW10: 10 bits), so the tinted pixels are the
//! ones the board's detector counted (a decoded JPEG frame: about them).

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use eframe::{
    egui,
    egui_wgpu::{self, wgpu},
};
use espion_proto::cam::Encoding;
use poc_viewer::net::Frame;

const SHADER: &str = r"
struct U {
    // uv of the viewport's corners: x0, y0, x1, y1
    uv: vec4<f32>,
    // display gain, RAW10 (1) or RAW8 (0), linear output target (1)
    opt: vec4<f32>,
    // image width and height in pixels, mask threshold (texture units)
    size: vec4<u32>,
    // mask tint (sRGB) and mask on (1)
    tint: vec4<f32>,
};
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var tex: texture_2d<u32>;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VOut {
    // One triangle over the viewport: p = (0,0), (2,0), (0,2).
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var o: VOut;
    o.pos = vec4<f32>(p.x * 2.0 - 1.0, 1.0 - p.y * 2.0, 0.0, 1.0);
    o.uv = u.uv.xy + (u.uv.zw - u.uv.xy) * p;
    return o;
}

fn to_linear(c: f32) -> f32 {
    if c <= 0.04045 { return c / 12.92; }
    return pow((c + 0.055) / 1.055, 2.4);
}

@fragment
fn fs(i: VOut) -> @location(0) vec4<f32> {
    let w = u.size.x;
    let h = u.size.y;
    let x = min(u32(max(i.uv.x, 0.0) * f32(w)), w - 1u);
    let y = min(u32(max(i.uv.y, 0.0) * f32(h)), h - 1u);
    var raw: u32;
    var v: f32;
    if u.opt.y > 0.5 {
        let g = (x / 4u) * 5u;
        let k = x % 4u;
        let hi = textureLoad(tex, vec2<u32>(g + k, y), 0).r;
        let lo = (textureLoad(tex, vec2<u32>(g + 4u, y), 0).r >> (2u * k)) & 3u;
        raw = (hi << 2u) | lo;
        v = f32(raw) / 1023.0;
    } else {
        raw = textureLoad(tex, vec2<u32>(x, y), 0).r;
        v = f32(raw) / 255.0;
    }
    v = clamp(v * u.opt.x, 0.0, 1.0);
    var c = vec3<f32>(v, v, v);
    if u.tint.w > 0.5 && raw >= u.size.z {
        c = mix(c, u.tint.rgb, 0.65);
    }
    if u.opt.z > 0.5 { c = vec3<f32>(to_linear(c.r), to_linear(c.g), to_linear(c.b)); }
    return vec4<f32>(c, 1.0);
}
";

/// Time spent handing frames to `wgpu` on the UI thread.
#[derive(Debug, Default, Clone, Copy)]
pub struct UploadTiming {
    /// Frames uploaded.
    pub count: u64,
    /// Total time in `write_texture`.
    pub total: Duration,
    /// Longest single upload.
    pub max: Duration,
    /// The last frame uploaded ([`Shown::serial`]) and when its upload ended.
    pub last: Option<(u64, Instant)>,
}

/// The GPU objects, kept in the renderer's callback resources.
struct Resources {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    uniforms: wgpu::Buffer,
    texture: Option<(wgpu::Texture, wgpu::BindGroup)>,
    uploaded: u64,
    linear_target: bool,
}

/// Creates the pipeline; call once with the app's render state.
pub fn init(rs: &egui_wgpu::RenderState) {
    let device = &rs.device;
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("poc-viewer image"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("poc-viewer image"),
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
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Uint,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
        ],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("poc-viewer image"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("poc-viewer image"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(rs.target_format.into())],
        }),
        multiview_mask: None,
        cache: None,
    });
    let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("poc-viewer image"),
        size: UNIFORM_BYTES as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    rs.renderer.write().callback_resources.insert(Resources {
        pipeline,
        layout,
        uniforms,
        texture: None,
        uploaded: 0,
        linear_target: rs.target_format.is_srgb(),
    });
}

/// A frame shown by the UI, with a serial number that tells the GPU side a new one came.
pub struct Shown {
    /// Increases with every frame shown.
    pub serial: u64,
    /// The frame (RAW8 or RAW10).
    pub frame: Frame,
    /// For a decoded JPEG frame: the file's length, bytes.
    pub jpeg_len: Option<usize>,
}

/// The threshold mask: pixels at or above `threshold` are tinted.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mask {
    /// In the texture's units: 10-bit for a RAW10 frame, 8-bit otherwise.
    pub threshold: u16,
    /// The tint.
    pub tint: egui::Color32,
}

/// Bytes of the shader's uniforms.
const UNIFORM_BYTES: usize = 64;

/// One paint of the image: the part of the frame inside `rect`.
pub struct ImageCallback {
    /// The frame to show.
    pub shown: Arc<Shown>,
    /// uv of the painted rectangle's corners in the image: x0, y0, x1, y1.
    pub uv: [f32; 4],
    /// Display gain.
    pub gain: f32,
    /// The threshold mask, if on.
    pub mask: Option<Mask>,
    /// Upload times, for the statistics.
    pub timing: Arc<Mutex<UploadTiming>>,
}

impl ImageCallback {
    /// The paint callback for `rect` (points).
    pub fn paint(self, rect: egui::Rect) -> egui::PaintCallback {
        egui_wgpu::Callback::new_paint_callback(rect, self)
    }
}

impl egui_wgpu::CallbackTrait for ImageCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(res) = resources.get_mut::<Resources>() else {
            return Vec::new();
        };
        let f = &self.shown.frame;
        // JPEG frames are decoded to RAW8 before they get here.
        let tw = f.encoding.line_bytes(f.width).unwrap_or(f.width as u32);
        let th = f.height as u32;
        if res.uploaded != self.shown.serial {
            let size = wgpu::Extent3d {
                width: tw,
                height: th,
                depth_or_array_layers: 1,
            };
            if res.texture.as_ref().is_none_or(|(t, _)| t.size() != size) {
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("poc-viewer frame"),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::R8Uint,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("poc-viewer frame"),
                    layout: &res.layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: res.uniforms.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&view),
                        },
                    ],
                });
                res.texture = Some((texture, group));
            }
            let (texture, _) = res.texture.as_ref().expect("created above");
            let t = Instant::now();
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &f.data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(tw),
                    rows_per_image: Some(th),
                },
                size,
            );
            let end = Instant::now();
            let dt = end - t;
            if let Ok(mut timing) = self.timing.lock() {
                timing.count += 1;
                timing.total += dt;
                timing.max = timing.max.max(dt);
                timing.last = Some((self.shown.serial, end));
            }
            res.uploaded = self.shown.serial;
        }
        let raw10 = f32::from(u8::from(f.encoding == Encoding::Raw10));
        let linear = f32::from(u8::from(res.linear_target));
        let mut u = [0u8; UNIFORM_BYTES];
        let floats = [
            self.uv[0], self.uv[1], self.uv[2], self.uv[3], self.gain, raw10, linear, 0.0,
        ];
        for (b, v) in u.as_chunks_mut::<4>().0.iter_mut().zip(floats) {
            b.copy_from_slice(&v.to_le_bytes());
        }
        let threshold = self.mask.map_or(0, |m| u32::from(m.threshold));
        for (i, v) in [u32::from(f.width), u32::from(f.height), threshold, 0]
            .into_iter()
            .enumerate()
        {
            u[32 + 4 * i..36 + 4 * i].copy_from_slice(&v.to_le_bytes());
        }
        let tint = self.mask.map_or([0.0; 4], |m| {
            let c = |v: u8| f32::from(v) / 255.0;
            [c(m.tint.r()), c(m.tint.g()), c(m.tint.b()), 1.0]
        });
        for (i, v) in tint.into_iter().enumerate() {
            u[48 + 4 * i..52 + 4 * i].copy_from_slice(&v.to_le_bytes());
        }
        queue.write_buffer(&res.uniforms, 0, &u);
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        let Some(res) = resources.get::<Resources>() else {
            return;
        };
        let Some((_, group)) = &res.texture else {
            return;
        };
        pass.set_pipeline(&res.pipeline);
        pass.set_bind_group(0, group, &[]);
        pass.draw(0..3, 0..1);
    }
}
