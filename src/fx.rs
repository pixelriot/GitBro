//! Thin compositor: Rune draws the UI, and effect slots get a rune-fx layer.
//!
//! `FIELD` is a still Bayer dither in the Omarchy palette, used on the welcome and
//! "not a repository" screens. It does not read the backdrop. The commit celebration
//! is pixel squares drawn by the view, not a rune-fx ring.

use rune::compose::{Compositor, DeviceKey, Frame};
use rune::render::KIND_EFFECT;
use rune_fx::{
    DitherParams, DitherPattern, FillInput, Fx, FxFrame, FxLayer, FxOptions, FxRenderer, FxSlot,
    Shape, Stops,
};

use crate::GitBro;

pub(crate) const FIELD: u32 = 1;

/// How long the commit celebration stays up.
pub(crate) const BURST_LIFE: f64 = 0.9;

struct Gpu {
    key: DeviceKey,
    format: wgpu::TextureFormat,
    fx: FxRenderer,
}

pub(crate) struct Gfx {
    gpu: Option<Gpu>,
    live: bool,
}

impl Default for Gfx {
    fn default() -> Self {
        Self {
            gpu: None,
            live: false,
        }
    }
}

#[derive(Clone, Copy)]
struct Slot {
    index: u32,
    rect: [f32; 4],
    scale: f32,
    clip: [f32; 4],
    fade: [u32; 4],
    mark: u32,
}

impl Compositor<GitBro> for Gfx {
    fn animating(&self) -> bool {
        self.live
    }

    fn render(&mut self, f: &mut Frame<'_, GitBro>) {
        let key = f.device_key();
        let format = f.format;
        let fresh = self
            .gpu
            .as_ref()
            .is_none_or(|g| g.key != key || g.format != format);
        if fresh {
            self.gpu = Some(Gpu {
                key,
                format,
                fx: FxRenderer::new(f.device, format, FxOptions { dither: true }),
            });
        }
        let live = {
            let gpu = self.gpu.as_mut().unwrap();
            gpu.fx.set_srgb_blend(f.srgb_blend());
            gpu.fx
                .add_format(f.device, wgpu::TextureFormat::Rgba16Float);

            let (slots, layers) = layers_of(
                &slots_of(f),
                &f.rt.state,
                f.rt.t as f32,
                f.rt.reduced_motion,
            );
            let placements: Vec<FxSlot> = slots
                .iter()
                .map(|s| FxSlot::default().scale(s.scale).clip(s.clip, s.fade))
                .collect();
            let live = layers.iter().any(|l| l.is_animating());
            gpu.fx.prepare_with_slots(
                f.device,
                f.queue,
                &FxFrame {
                    viewport: [f.size.0, f.size.1],
                    scale: 1.0,
                    layers: &layers,
                    backdrop: None,
                    sources: &[],
                },
                &placements,
            );

            let clear = f.rt.clear_color();
            f.ui.prepare(
                f.device,
                f.queue,
                &mut f.rt.text.atlas,
                &f.rt.instances,
                f.size,
            );
            let fx = &gpu.fx;
            f.ui.draw_with(f.encoder, f.target, clear, &mut |pass, site| {
                if let Some(k) = slots.iter().position(|s| s.index == site.index) {
                    fx.draw_layer_in(pass, site.format, site.origin, k);
                }
            });
            live
        };
        self.live = live;
    }
}

fn slots_of(f: &Frame<'_, GitBro>) -> Vec<Slot> {
    f.rt.instances
        .iter()
        .enumerate()
        .filter(|(_, i)| i.params[1] == KIND_EFFECT)
        .map(|(n, i)| Slot {
            index: n as u32,
            rect: i.rect,
            scale: i.params[3],
            clip: i.clip,
            fade: i.fade,
            mark: f.rt.marks.get(n).copied().unwrap_or(0),
        })
        .collect()
}

fn layers_of(slots: &[Slot], state: &GitBro, t: f32, reduced: bool) -> (Vec<Slot>, Vec<FxLayer>) {
    let theme = state.fx_theme();
    let mut kept = Vec::new();
    let mut layers = Vec::new();
    for slot in slots {
        let k = slot.scale.max(0.001);
        let [x, y, w, h] = slot.rect.map(|v| v / k);
        let shape = Shape::rounded(x, y, w, h, 0.0);
        let layer = match slot.mark {
            FIELD => Some(
                Fx::fill(field_dither(theme))
                    .on(shape)
                    .at(t)
                    .reduced(reduced),
            ),
            _ => None,
        };
        if let Some(layer) = layer {
            kept.push(*slot);
            layers.push(layer);
        }
    }
    (kept, layers)
}

/// Frozen noise, quantised into the theme. Bias keeps it mostly the dark stops,
/// with the accent only on the brightest peaks.
fn field_dither(theme: crate::theme::Theme) -> DitherParams {
    DitherParams {
        pattern: DitherPattern::Bayer8,
        input: FillInput::Noise,
        colors: Stops::hex(&[theme.surface, theme.bg, theme.border, theme.accent]),
        pixel: 3.0,
        scale: 48.0,
        speed: 0.0,
        bias: -0.32,
        contrast: 1.15,
        ..DitherParams::bayer()
    }
}
