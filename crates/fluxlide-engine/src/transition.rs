//! Transitions: a WGSL function that turns the outgoing and the incoming
//! picture into one picture, given how far the transition has gone.
//!
//! The contract for transition shaders (also in `docs/spike-captions-transitions.md`):
//!
//! ```wgsl
//! fn transition(uv: vec2<f32>, progress: f32) -> vec4<f32> {
//!     return mix(from_color(uv), to_color(uv), progress);
//! }
//! ```
//!
//! The author writes only `transition`. The engine adds, after the
//! author's code, the vertex and fragment entry points and these names:
//!
//! - `from_color(uv: vec2<f32>) -> vec4<f32>` and `to_color(...)`: the
//!   outgoing and the incoming picture at `uv`.
//! - `fx.resolution: vec2<f32>`: output size in pixels.
//! - `fx.aspect: f32`: width divided by height.
//! - `fx.params: vec4<f32>`: four numbers set with [`Transition::with_params`].
//! - `fx.progress: f32`: the same value as the `progress` argument.
//!
//! Coordinates: `uv` is (0, 0) at the top left and (1, 1) at the bottom
//! right of the output. `progress` is 0 at the start and 1 at the end and
//! is not eased; easing is the shader's choice. Colors are linear light
//! (not sRGB-encoded). Both pictures are already fitted to the output, so
//! they are opaque. `from_color` and `to_color` return opaque black outside
//! the 0..1 range. The returned alpha is ignored; the result is opaque.
//!
//! Reserved names: `fx`, `FxUniforms`, `from_color`, `to_color` and
//! everything starting with `fx_`. The author's code comes first in the
//! module, so line numbers in error messages are the author's own.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::num::NonZeroU64;
use std::sync::Arc;

use crate::{EngineError, Renderer};

/// Size of the uniform data a transition pass reads.
pub(crate) const TRANSITION_UNIFORM_SIZE: u64 = 32;

/// The four ways something can move on a flat output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    /// Unit vector in `uv` space (y points down).
    fn vector(self) -> [f32; 2] {
        match self {
            Self::Left => [-1.0, 0.0],
            Self::Right => [1.0, 0.0],
            Self::Up => [0.0, -1.0],
            Self::Down => [0.0, 1.0],
        }
    }
}

/// A transition shader and its parameters. Cheap to clone.
#[derive(Clone, Debug)]
pub struct Transition {
    name: Arc<str>,
    source: Arc<str>,
    /// Identifies the source, so pipelines are built once per shader.
    key: u64,
    params: [f32; 4],
}

impl Transition {
    fn builtin(name: &'static str, source: &'static str, params: [f32; 4]) -> Self {
        Self {
            name: name.into(),
            source: source.into(),
            key: source_key(source),
            params,
        }
    }

    /// Crossfade: the incoming picture replaces the outgoing one by
    /// mixing the two in linear light.
    pub fn fade() -> Self {
        Self::builtin("fade", FADE, [0.0; 4])
    }

    /// Push: both pictures move together in `direction` (so a new picture
    /// enters from the opposite side).
    pub fn slide(direction: Direction) -> Self {
        let [x, y] = direction.vector();
        Self::builtin("slide", SLIDE, [x, y, 0.0, 0.0])
    }

    /// The outgoing picture grows and fades out while the incoming one
    /// settles into place from a larger size.
    pub fn zoom() -> Self {
        Self::builtin("zoom", ZOOM, [0.0; 4])
    }

    /// A soft edge travels in `direction` and reveals the incoming picture
    /// behind it.
    pub fn wipe(direction: Direction) -> Self {
        let [x, y] = direction.vector();
        Self::builtin("wipe", WIPE, [x, y, 0.0, 0.0])
    }

    /// Builds a transition from WGSL source that defines
    /// `fn transition(uv: vec2<f32>, progress: f32) -> vec4<f32>`.
    ///
    /// The source is compiled and checked against the engine's pipeline on
    /// `renderer`'s device, so a broken shader fails here with the
    /// compiler's message and never reaches a frame.
    pub fn from_wgsl(renderer: &Renderer, name: &str, source: &str) -> Result<Self, EngineError> {
        let transition = Self {
            name: name.into(),
            source: source.into(),
            key: source_key(source),
            params: [0.0; 4],
        };
        let device = renderer.device();
        let layouts = TransitionLayouts::new(device);
        transition.build_pipeline(device, &layouts, crate::FRAME_FORMAT)?;
        Ok(transition)
    }

    /// Sets the four numbers a shader reads as `fx.params`.
    pub fn with_params(mut self, params: [f32; 4]) -> Self {
        self.params = params;
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn key(&self) -> u64 {
        self.key
    }

    pub(crate) fn params(&self) -> [f32; 4] {
        self.params
    }

    /// Compiles the shader into a pipeline writing to `format`.
    /// Fails with the compiler's message if the source is invalid.
    pub(crate) fn build_pipeline(
        &self,
        device: &wgpu::Device,
        layouts: &TransitionLayouts,
        format: wgpu::TextureFormat,
    ) -> Result<wgpu::RenderPipeline, EngineError> {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        // Author's code first, so that line numbers in messages match it.
        let source = format!("{}\n{}", self.source, TRANSITION_ENGINE_SIDE);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(&self.name),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(&self.name),
            layout: Some(&layouts.pipeline),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("fx_vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fx_fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        // Errors of an invalid shader module and of the pipeline made from
        // it are both caught by the scope; the first one is the real cause.
        match pollster::block_on(scope.pop()) {
            None => Ok(pipeline),
            Some(err) => Err(EngineError::Shader {
                name: self.name.to_string(),
                message: format!(
                    "{err}\nnote: lines after {} belong to the engine, which calls \
                     `fn transition(uv: vec2<f32>, progress: f32) -> vec4<f32>`",
                    self.source.lines().count()
                ),
            }),
        }
    }
}

fn source_key(source: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    source.hash(&mut hasher);
    hasher.finish()
}

/// Bind group and pipeline layout shared by every transition shader.
pub(crate) struct TransitionLayouts {
    pub bind_group: wgpu::BindGroupLayout,
    pipeline: wgpu::PipelineLayout,
}

impl TransitionLayouts {
    pub fn new(device: &wgpu::Device) -> Self {
        let texture_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let bind_group = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fluxlide-transition-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: NonZeroU64::new(TRANSITION_UNIFORM_SIZE),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                texture_entry(2),
                texture_entry(3),
            ],
        });
        let pipeline = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fluxlide-transition-pipeline-layout"),
            bind_group_layouts: &[Some(&bind_group)],
            immediate_size: 0,
        });
        Self {
            bind_group,
            pipeline,
        }
    }
}

/// Appended after the author's code: the names the contract promises.
const TRANSITION_ENGINE_SIDE: &str = include_str!("transition_engine.wgsl");

const FADE: &str = include_str!("transitions/fade.wgsl");
const SLIDE: &str = include_str!("transitions/slide.wgsl");
const ZOOM: &str = include_str!("transitions/zoom.wgsl");
const WIPE: &str = include_str!("transitions/wipe.wgsl");
