//! Manual probe: how far the live GPU-Detail preview (mode 5) sits from the
//! committed image on a real photo, with the proxies built the way the app
//! builds them, at fit and at 100 % — split by cause (colour proxies vs
//! Detail) so a preview/commit difference can be traced.
//!
//! IAI_PERF_IMAGE=<photo> cargo test --release --test detail_gpu_commit_probe \
//!   -- --ignored --nocapture

use iai::core::develop::{self, fast_preview_downsample, DevelopSettings, TONE_DOWNSAMPLE};
use iai::core::develop_scene::{self, apply_scene_to_tilemap, SceneSource};
use iai::core::layer::{Layer, LayerStack};
use iai::gpu::compositor::{
    ColorProxies, CompositorState, DevelopDetailGpu, DevelopGpuPreview, RegionLumaProxy,
};
use std::sync::Arc;

const DETAIL_HALO: u32 = 72;

struct Stats {
    max: u8,
    p99: u8,
    mean: f64,
}

fn stats(a: &[u8], b: &[u8]) -> Stats {
    let mut d: Vec<u8> = a
        .chunks_exact(4)
        .zip(b.chunks_exact(4))
        // Alpha 0 marks screen pixels outside the image on either side.
        .filter(|(p, q)| p[3] > 0 && q[3] > 0)
        .flat_map(|(p, q)| (0..3).map(move |c| p[c].abs_diff(q[c])))
        .collect();
    let mean = d.iter().map(|&v| v as f64).sum::<f64>() / d.len().max(1) as f64;
    d.sort_unstable();
    Stats {
        max: *d.last().unwrap_or(&0),
        p99: d[(d.len() * 99 / 100).min(d.len().saturating_sub(1))],
        mean,
    }
}

#[allow(clippy::too_many_arguments)]
fn preview(
    scene: &Arc<SceneSource>,
    settings: &DevelopSettings,
    raw: bool,
    color_s: usize,
    color_region: (u32, u32, u32, u32),
    detail: Option<DevelopDetailGpu>,
) -> DevelopGpuPreview {
    let tone = develop_scene::build_scene_tone_for_scene(settings, scene);
    let (ox, oy, rw, rh) = color_region;
    let (base, pw, ph) = develop_scene::build_scene_color_base_box(scene, ox, oy, rw, rh, color_s);
    let region = develop_scene::tone_lowpass_scene_region(&base, pw, ph, &tone, color_s);
    let (adjusted, guided) = if raw {
        let samples = develop_scene::tone_scene_color_samples(&base, &tone);
        (
            develop::guided_mixer_controls(&samples, settings, pw, ph).expect("guided"),
            true,
        )
    } else {
        (
            develop::apply_color_to_region(&region, settings, pw, ph),
            false,
        )
    };
    let (rbase, rpw, rph) = develop_scene::build_scene_region_base(scene, TONE_DOWNSAMPLE);
    let regional = develop_scene::finish_region_e(&rbase, rpw, rph, &tone, TONE_DOWNSAMPLE);
    DevelopGpuPreview {
        layer_id: 0,
        settings: settings.clone(),
        region_luma: settings.has_local_tone().then(|| RegionLumaProxy {
            data: Arc::new(regional),
            w: rpw,
            h: rph,
            downsample: TONE_DOWNSAMPLE as u32,
        }),
        // The app leaves an Identity scene's colour to the plane's own field.
        color: (settings.has_color() && (raw || detail.is_none())).then(|| ColorProxies {
            region: Arc::new(region),
            adjusted: Arc::new(adjusted),
            w: pw,
            h: ph,
            origin_x: ox,
            origin_y: oy,
            downsample: color_s as u32,
            fast_preview: false,
            guided_controls: guided,
            exact_detail: false,
        }),
        scene: Some(scene.clone()),
        detail,
    }
}

#[test]
#[ignore = "manual probe; needs a local photo and a GPU"]
fn gpu_detail_preview_vs_commit_on_a_photo() {
    let Ok(path) = std::env::var("IAI_PERF_IMAGE") else {
        eprintln!("IAI_PERF_IMAGE not set; skipping");
        return;
    };
    let Some((device, queue)) = iai::gpu::vector::renderer::headless_device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let img = image::open(&path).expect("open photo").to_rgba8();
    let (w, h) = img.dimensions();
    let px = img.into_raw();
    let tiles = iai::core::tile::TileMap::from_rgba(&px, w, h);
    let raw = std::env::var("IAI_PERF_LOOK").is_ok_and(|v| v == "raw");
    let mut scene = SceneSource::from_display_tiles(&tiles);
    if raw {
        scene.look = develop_scene::BaseLook::Raw;
    }
    let scene = Arc::new(scene);
    let neutral8: Vec<u8> = if raw {
        develop_scene::render_default_look(&scene)
            .iter()
            .map(|v| (v >> 8) as u8)
            .collect()
    } else {
        px.clone()
    };
    let (vw, vh) = (1520u32, 900u32);
    let max_texture = device.limits().max_texture_dimension_2d;

    let light = DevelopSettings {
        exposure: 15.0,
        contrast: 15.0,
        highlights: -30.0,
        shadows: 40.0,
        ..Default::default()
    };
    let light_sat = DevelopSettings {
        saturation: 12.0,
        ..light.clone()
    };
    let mut light_mixer = light.clone();
    light_mixer.mixer_hue[1] = -30.0;
    light_mixer.mixer_luminance[1] = 40.0;
    let mut mixer_only = DevelopSettings::default();
    mixer_only.mixer_hue[1] = -30.0;
    mixer_only.mixer_luminance[1] = 40.0;
    let mut full = light_sat.clone();
    full.mixer_hue[1] = -30.0;
    full.mixer_luminance[1] = 40.0;
    full.sharpening = 40.0;
    full.noise_reduction = 20.0;
    let fit = (vw as f32 / w as f32).min(vh as f32 / h as f32);
    let mut source_stack = LayerStack::new(w, h);
    source_stack.layers[0] = Layer::from_rgba(0, "Background", neutral8.clone(), w, h);
    let app_s = fast_preview_downsample(w, h);

    for (view_label, zoom) in [("100%", 1.0f32), ("fit", fit)] {
        // Canvas point at the viewport's top-left (centred view).
        let view = (
            (w as f32 - vw as f32 / zoom) / 2.0,
            (h as f32 - vh as f32 / zoom) / 2.0,
        );
        let lx0 = view.0.max(0.0).floor() as u32;
        let ly0 = view.1.max(0.0).floor() as u32;
        let lx1 = ((view.0 + vw as f32 / zoom).ceil() as u32).min(w);
        let ly1 = ((view.1 + vh as f32 / zoom).ceil() as u32).min(h);
        let ds = if zoom >= 1.0 {
            1
        } else {
            ((1.0 / zoom).floor() as u32).max(1)
        };
        let pad = DETAIL_HALO * ds;
        let plane = |settings: &DevelopSettings| DevelopDetailGpu {
            origin_x: lx0.saturating_sub(pad) / ds * ds,
            origin_y: ly0.saturating_sub(pad) / ds * ds,
            end_x: (lx1 + pad).min(w),
            end_y: (ly1 + pad).min(h),
            downsample: ds,
            linear: raw,
            luma_coeff: if raw {
                develop_scene::build_scene_tone_for_scene(settings, &scene)
                    .working_space
                    .render_luminance_coefficients()
            } else {
                [0.2126, 0.7152, 0.0722]
            },
            run_detail: true,
        };
        let composite = |stack: &LayerStack, dev: Option<DevelopGpuPreview>| {
            let mut compositor = CompositorState::new(&device, vw, vh, max_texture);
            compositor.develop_preview = dev;
            let is_ping = compositor.composite_layers(
                &device, &queue, stack, view.0, view.1, zoom, None, false, false,
            );
            compositor.readback_rgba8(&device, &queue, is_ping)
        };
        // What a filtered display of the committed image shows at this zoom:
        // the box average of each screen pixel's footprint.
        let box_display = |img: &[u8]| {
            let mut out = vec![0u8; (vw * vh * 4) as usize];
            for sy in 0..vh {
                for sx in 0..vw {
                    let fx0 = view.0 + sx as f32 / zoom;
                    let fy0 = view.1 + sy as f32 / zoom;
                    if fx0 < 0.0 || fy0 < 0.0 {
                        continue;
                    }
                    let cx0 = fx0.floor() as u32;
                    let cy0 = fy0.floor() as u32;
                    if cx0 >= w || cy0 >= h {
                        continue;
                    }
                    let cx1 = ((view.0 + (sx + 1) as f32 / zoom).ceil() as u32).clamp(cx0 + 1, w);
                    let cy1 = ((view.1 + (sy + 1) as f32 / zoom).ceil() as u32).clamp(cy0 + 1, h);
                    let mut acc = [0u32; 3];
                    let mut n = 0u32;
                    for y in cy0..cy1 {
                        for x in cx0..cx1 {
                            let i = ((y * w + x) * 4) as usize;
                            for c in 0..3 {
                                acc[c] += img[i + c] as u32;
                            }
                            n += 1;
                        }
                    }
                    let o = ((sy * vw + sx) * 4) as usize;
                    for c in 0..3 {
                        out[o + c] = ((acc[c] + n / 2) / n.max(1)) as u8;
                    }
                    out[o + 3] = 255;
                }
            }
            out
        };
        let light_detail = DevelopSettings {
            sharpening: 40.0,
            noise_reduction: 20.0,
            ..light.clone()
        };
        let mut sat_mixer = light_sat.clone();
        sat_mixer.mixer_hue[1] = -30.0;
        sat_mixer.mixer_luminance[1] = 40.0;
        let mut sat_mixer_only = DevelopSettings {
            saturation: 12.0,
            ..Default::default()
        };
        sat_mixer_only.mixer_hue[1] = -30.0;
        sat_mixer_only.mixer_luminance[1] = 40.0;
        let mixer_detail = DevelopSettings {
            sharpening: 40.0,
            noise_reduction: 20.0,
            ..light_mixer.clone()
        };
        let sat_detail = DevelopSettings {
            sharpening: 40.0,
            noise_reduction: 20.0,
            ..light_sat.clone()
        };
        let cases: Vec<(&str, &DevelopSettings, usize)> = if zoom < 1.0 {
            vec![("light + Detail", &light_detail, app_s)]
        } else {
            vec![
                ("light+sat+mixer", &sat_mixer, app_s),
                ("light+sat+mixer (grid 6)", &sat_mixer, 6),
                ("sat+mixer", &sat_mixer_only, app_s),
                ("light+mixer+Detail", &mixer_detail, app_s),
                ("light+sat+Detail", &sat_detail, app_s),
                ("all + Detail", &full, app_s),
                ("all + Detail (grid 6)", &full, 6),
            ]
        };
        for (label, settings, color_s) in cases {
            let committed = apply_scene_to_tilemap(&scene, settings, None).flatten();
            let mut commit_stack = LayerStack::new(w, h);
            commit_stack.layers[0] = Layer::from_rgba(0, "Background", committed.clone(), w, h);
            let after = composite(&commit_stack, None);
            let live = composite(
                &source_stack,
                Some(preview(
                    &scene,
                    settings,
                    raw,
                    color_s,
                    (0, 0, w, h),
                    settings.has_detail().then(|| plane(settings)),
                )),
            );
            let st = stats(&live, &after);
            let filtered = if zoom < 1.0 {
                let f = stats(&live, &box_display(&committed));
                format!(
                    " | vs filtered commit: max {:>3} p99 {:>2} mean {:.2}",
                    f.max, f.p99, f.mean
                )
            } else {
                String::new()
            };
            eprintln!(
                "{} {view_label:>4} {label:<25}: max {:>3}  p99 {:>2}  mean {:.2}{filtered}",
                if raw { "RAW " } else { "JPEG" },
                st.max,
                st.p99,
                st.mean
            );
        }
    }
}
