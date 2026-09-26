//! Manual probe: RAW Develop GPU preview vs the CPU commit on a real file,
//! with the preview payload built the way `build_develop_gpu_preview` builds it
//! for a RAW scene (Mode B viewport composite, Develop3 defaults).
//!
//! IAI_RAW_PROBE=<raw file> [IAI_RAW_PROBE_OUT=<dir>] [IAI_RAW_PROBE_ONLY=<name>]
//! cargo test --release --test raw_preview_commit_probe -- --ignored --nocapture

use iai::core::develop::{self, DevelopSettings, TONE_DOWNSAMPLE};
use iai::core::develop_scene::{self, SceneSource};
use iai::core::layer::LayerStack;
use iai::formats::{raw::RawImporter, Importer};
use iai::gpu::compositor::{
    ColorProxies, CompositorState, DevelopDetailGpu, DevelopGpuPreview, RegionLumaProxy,
};
use std::sync::Arc;

const DETAIL_HALO: u32 = 72;
const EFFECTS_APRON: u32 = 56;
const PLANE_BUDGET: u64 = 4_000_000;

#[derive(Clone, Copy)]
struct View {
    vw: u32,
    vh: u32,
    x: f32,
    y: f32,
    zoom: f32,
}

impl View {
    fn visible(&self, w: u32, h: u32) -> (u32, u32, u32, u32) {
        let x0 = self.x.floor().clamp(0.0, w as f32) as u32;
        let y0 = self.y.floor().clamp(0.0, h as f32) as u32;
        let x1 = (self.x + self.vw as f32 / self.zoom)
            .ceil()
            .clamp(0.0, w as f32) as u32;
        let y1 = (self.y + self.vh as f32 / self.zoom)
            .ceil()
            .clamp(0.0, h as f32) as u32;
        (x0, y0, x1, y1)
    }
}

fn plan_plane(
    (lx0, ly0, lx1, ly1): (u32, u32, u32, u32),
    w: u32,
    h: u32,
    zoom: f32,
    halo: u32,
) -> (u32, u32, u32, u32, u32) {
    let mut ds = if zoom >= 1.0 {
        1
    } else {
        ((1.0 / zoom.max(1e-4)).floor() as u32).max(1)
    };
    loop {
        let pad = halo * ds;
        let ox = lx0.saturating_sub(pad) / ds * ds;
        let oy = ly0.saturating_sub(pad) / ds * ds;
        let ex = lx1.saturating_add(pad).min(w).max(ox + 1);
        let ey = ly1.saturating_add(pad).min(h).max(oy + 1);
        let pw = (ex - ox).div_ceil(ds) as u64;
        let ph = (ey - oy).div_ceil(ds) as u64;
        if pw * ph <= PLANE_BUDGET || ds >= 64 {
            return (ox, oy, ex, ey, ds);
        }
        ds += 1;
    }
}

/// The RAW-scene payload `build_develop_gpu_preview` sends (hardware adapter,
/// GPU-hosted scene, colour proxy planned on the same view).
fn app_preview(
    scene: &Arc<SceneSource>,
    layer_id: u32,
    s: &DevelopSettings,
    v: View,
    region_base: &(Vec<[f32; 3]>, usize, usize),
) -> (DevelopGpuPreview, Option<u32>) {
    let tone = develop_scene::build_scene_tone_for_scene(s, scene);
    let raw = scene.look == develop_scene::BaseLook::Raw;
    let (w, h) = (scene.width, scene.height);
    let vis = v.visible(w, h);
    let mut color_ds = None;
    let plane_effects = s.has_spatial_effects() || s.vignette.abs() > 0.001;
    let gpu_plane = s.has_detail() || plane_effects;
    // RAW: only the guided mixer has a colour proxy; Identity: every colour
    // edit without a GPU plane (the plane builds its own colour field).
    let need_color = if raw {
        s.has_color() && develop::guided_mixer_active(s)
    } else {
        s.has_color() && !gpu_plane
    };
    let color = need_color.then(|| {
        let (lx0, ly0, lx1, ly1) = vis;
        let ds = develop::fast_preview_downsample(lx1 - lx0, ly1 - ly0) as u32;
        color_ds = Some(ds);
        let pad = (ds * 4).max(64);
        let ox = lx0.saturating_sub(pad).min(w - 1);
        let oy = ly0.saturating_sub(pad).min(h - 1);
        let ex = (lx1 + pad).min(w).max(ox + 1);
        let ey = (ly1 + pad).min(h).max(oy + 1);
        let (base, pw, ph) =
            develop_scene::build_scene_color_base_box(scene, ox, oy, ex - ox, ey - oy, ds as usize);
        let samples = develop_scene::tone_scene_color_samples(&base, &tone);
        let region = develop_scene::tone_lowpass_scene_region(&base, pw, ph, &tone, ds as usize);
        let controls = raw
            .then(|| develop::guided_mixer_controls_scaled(&samples, s, pw, ph, ds as usize))
            .flatten();
        let guided = controls.is_some();
        let adjusted =
            controls.unwrap_or_else(|| develop::apply_color_to_region(&region, s, pw, ph));
        ColorProxies {
            region: Arc::new(region),
            adjusted: Arc::new(adjusted),
            w: pw,
            h: ph,
            origin_x: ox,
            origin_y: oy,
            downsample: ds,
            fast_preview: false,
            guided_controls: guided,
            exact_detail: false,
        }
    });
    let region_luma = s.has_local_tone().then(|| {
        let (base, pw, ph) = region_base;
        RegionLumaProxy {
            data: Arc::new(develop_scene::finish_region_e(
                base,
                *pw,
                *ph,
                &tone,
                TONE_DOWNSAMPLE,
            )),
            w: *pw,
            h: *ph,
            downsample: TONE_DOWNSAMPLE as u32,
        }
    });
    let detail = gpu_plane.then(|| {
        let halo = if s.has_detail() { DETAIL_HALO } else { 0 }
            + if s.has_spatial_effects() {
                EFFECTS_APRON
            } else {
                0
            };
        let (origin_x, origin_y, end_x, end_y, downsample) = plan_plane(vis, w, h, v.zoom, halo);
        DevelopDetailGpu {
            origin_x,
            origin_y,
            end_x,
            end_y,
            downsample,
            linear: raw,
            luma_coeff: if raw {
                tone.working_space.render_luminance_coefficients()
            } else {
                [0.2126, 0.7152, 0.0722]
            },
            run_detail: s.has_detail(),
        }
    });
    (
        DevelopGpuPreview {
            layer_id,
            settings: s.clone(),
            region_luma,
            color,
            scene: Some(scene.clone()),
            detail,
        },
        color_ds,
    )
}

struct Stats {
    mean: [f32; 3],
    mean_abs: f32,
    p99: u8,
    max: u8,
    block_max: f32,
    block_worst: (usize, usize, [f32; 3]),
}

/// Compare `a` (preview) against `b` over `cw×ch` of two `stride`-wide RGBA
/// buffers; `block` px blocks measure regional colour shifts free of aliasing.
fn compare(a: &[u8], b: &[u8], stride: usize, cw: usize, ch: usize, block: usize) -> Stats {
    let mut sum = [0f64; 3];
    let mut sum_abs = 0f64;
    let mut hist = [0u64; 256];
    let mut max = 0u8;
    let n = (cw * ch) as f64;
    for y in 0..ch {
        for x in 0..cw {
            let i = (y * stride + x) * 4;
            for c in 0..3 {
                let d = a[i + c] as i32 - b[i + c] as i32;
                sum[c] += d as f64;
                sum_abs += d.unsigned_abs() as f64;
                let ad = d.unsigned_abs().min(255) as u8;
                hist[ad as usize] += 1;
                max = max.max(ad);
            }
        }
    }
    let total: u64 = hist.iter().sum();
    let mut acc = 0u64;
    let mut p99 = 0u8;
    for (v, count) in hist.iter().enumerate() {
        acc += count;
        if acc * 100 >= total * 99 {
            p99 = v as u8;
            break;
        }
    }
    let mut block_max = 0f32;
    let mut block_worst = (0, 0, [0f32; 3]);
    for by in 0..ch / block {
        for bx in 0..cw / block {
            let mut d = [0f64; 3];
            for y in by * block..(by + 1) * block {
                for x in bx * block..(bx + 1) * block {
                    let i = (y * stride + x) * 4;
                    for c in 0..3 {
                        d[c] += a[i + c] as f64 - b[i + c] as f64;
                    }
                }
            }
            let k = (block * block) as f64;
            let d = [(d[0] / k) as f32, (d[1] / k) as f32, (d[2] / k) as f32];
            let m = d[0].abs().max(d[1].abs()).max(d[2].abs());
            if m > block_max {
                block_max = m;
                block_worst = (bx * block, by * block, d);
            }
        }
    }
    Stats {
        mean: [
            (sum[0] / n) as f32,
            (sum[1] / n) as f32,
            (sum[2] / n) as f32,
        ],
        mean_abs: (sum_abs / (3.0 * n)) as f32,
        p99,
        max,
        block_max,
        block_worst,
    }
}

fn print(label: &str, s: &Stats) {
    eprintln!(
        "  {label:<30} mean(R,G,B) {:+5.2} {:+5.2} {:+5.2}  |d| {:4.2}  p99 {:3}  max {:3}  worst-block {:5.1} at {:?} {:+.1?}",
        s.mean[0], s.mean[1], s.mean[2], s.mean_abs, s.p99, s.max, s.block_max,
        (s.block_worst.0, s.block_worst.1), s.block_worst.2
    );
}

/// Box-average `src` (full-res RGBA8) onto the fit view's screen grid.
fn box_to_view(src: &[u8], w: u32, h: u32, v: View) -> Vec<u8> {
    let mut out = vec![0u8; (v.vw * v.vh * 4) as usize];
    for sy in 0..v.vh {
        for sx in 0..v.vw {
            let x0 = (v.x + sx as f32 / v.zoom).floor() as i64;
            let x1 = (v.x + (sx + 1) as f32 / v.zoom).floor() as i64;
            let y0 = (v.y + sy as f32 / v.zoom).floor() as i64;
            let y1 = (v.y + (sy + 1) as f32 / v.zoom).floor() as i64;
            let (x0, x1) = (x0.clamp(0, w as i64), x1.max(x0 + 1).clamp(0, w as i64));
            let (y0, y1) = (y0.clamp(0, h as i64), y1.max(y0 + 1).clamp(0, h as i64));
            if x1 <= x0 || y1 <= y0 {
                continue;
            }
            let mut acc = [0u64; 4];
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = ((y as u32 * w + x as u32) * 4) as usize;
                    for c in 0..4 {
                        acc[c] += src[i + c] as u64;
                    }
                }
            }
            let k = ((x1 - x0) * (y1 - y0)) as u64;
            let o = ((sy * v.vw + sx) * 4) as usize;
            for c in 0..4 {
                out[o + c] = ((acc[c] + k / 2) / k) as u8;
            }
        }
    }
    out
}

fn crop(src: &[u8], w: u32, x: u32, y: u32, cw: u32, ch: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((cw * ch * 4) as usize);
    for row in y..y + ch {
        let i = ((row * w + x) * 4) as usize;
        out.extend_from_slice(&src[i..i + (cw * 4) as usize]);
    }
    out
}

fn save(dir: &Option<std::path::PathBuf>, name: &str, rgba: &[u8], w: u32, h: u32) {
    if let Some(dir) = dir {
        let mut px = rgba.to_vec();
        for p in px.chunks_exact_mut(4) {
            p[3] = 255;
        }
        image::RgbaImage::from_raw(w, h, px)
            .expect("image size")
            .save(dir.join(name))
            .expect("save png");
    }
}

/// Composite `stack` at view `v` (recompositing until stable, as LOD proxies
/// land asynchronously) and read it back.
fn render_view(
    comp: &mut CompositorState,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    stack: &LayerStack,
    v: View,
    p: Option<DevelopGpuPreview>,
) -> Vec<u8> {
    comp.configure_viewport(device, v.vw, v.vh, 1);
    comp.develop_preview = p;
    let mut last = Vec::new();
    for _ in 0..8 {
        let ping =
            comp.composite_layers(device, queue, stack, v.x, v.y, v.zoom, None, false, false);
        let px = comp.readback_rgba8(device, queue, ping);
        if px == last {
            break;
        }
        last = px;
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
    last
}

fn scenarios() -> Vec<(&'static str, DevelopSettings)> {
    let light = |s: &mut DevelopSettings| {
        s.exposure = 20.0;
        s.contrast = 15.0;
        s.highlights = -35.0;
        s.shadows = 35.0;
        s.whites = 10.0;
        s.blacks = -10.0;
    };
    let colour = |s: &mut DevelopSettings| {
        s.vibrance = 25.0;
        s.saturation = 10.0;
    };
    let mixer = |s: &mut DevelopSettings| {
        s.mixer_saturation[1] = -15.0;
        s.mixer_luminance[1] = 20.0;
        s.mixer_saturation[5] = 20.0;
        s.mixer_hue[3] = -10.0;
    };
    let effects = |s: &mut DevelopSettings| {
        s.clarity = 25.0;
        s.texture = 20.0;
        s.dehaze = 10.0;
    };
    let detail = |s: &mut DevelopSettings| {
        s.sharpening = 40.0;
        s.noise_reduction = 20.0;
        s.color_noise_reduction = 25.0;
    };
    let mut out = Vec::new();
    let mut s = DevelopSettings::default();
    s.exposure = 1.0;
    out.push(("near-neutral", s));
    let mut s = DevelopSettings::default();
    s.temperature = 15.0;
    s.tint = 5.0;
    out.push(("wb", s));
    let mut s = DevelopSettings::default();
    light(&mut s);
    out.push(("light", s));
    let mut s = DevelopSettings::default();
    s.curve_points = vec![[0.0, 0.0], [0.25, 0.20], [0.75, 0.82], [1.0, 1.0]];
    out.push(("curve", s));
    let mut s = DevelopSettings::default();
    colour(&mut s);
    out.push(("vibrance+sat", s));
    let mut s = DevelopSettings::default();
    mixer(&mut s);
    out.push(("mixer", s));
    let mut s = DevelopSettings::default();
    effects(&mut s);
    out.push(("clarity+texture+dehaze", s));
    let mut s = DevelopSettings::default();
    detail(&mut s);
    out.push(("sharpen+nr", s));
    let mut s = DevelopSettings::default();
    light(&mut s);
    colour(&mut s);
    mixer(&mut s);
    effects(&mut s);
    detail(&mut s);
    out.push(("all", s));
    let mut s = DevelopSettings::default();
    mixer(&mut s);
    detail(&mut s);
    out.push(("mixer+sharpen", s));
    let mut s = DevelopSettings::default();
    mixer(&mut s);
    effects(&mut s);
    out.push(("mixer+clarity", s));
    let mut s = DevelopSettings::default();
    light(&mut s);
    detail(&mut s);
    out.push(("light+sharpen", s));
    let mut s = DevelopSettings::default();
    light(&mut s);
    effects(&mut s);
    out.push(("light+clarity", s));
    let mut s = DevelopSettings::default();
    colour(&mut s);
    detail(&mut s);
    out.push(("vibrance+sharpen", s));
    let mut s = DevelopSettings::default();
    s.sharpening = 40.0;
    out.push(("sharpen-only", s));
    let mut s = DevelopSettings::default();
    s.clarity = 25.0;
    out.push(("clarity-only", s));
    // A near-invisible Vignette only routes the preview through the GPU plane
    // (no Detail, no spatial Effects): isolates the plane's own sampling.
    let mut s = DevelopSettings::default();
    s.vignette = -1.0;
    out.push(("plane-only", s));
    let mut s = DevelopSettings::default();
    s.vignette = -1.0;
    s.exposure = 1.0;
    out.push(("plane-vs-shader-ref", s));
    out
}

#[test]
#[ignore = "manual probe; needs a local RAW, a GPU and --release"]
fn raw_preview_commit_probe() {
    let Ok(path) = std::env::var("IAI_RAW_PROBE") else {
        eprintln!("IAI_RAW_PROBE not set; skipping");
        return;
    };
    let out_dir = std::env::var("IAI_RAW_PROBE_OUT")
        .ok()
        .map(std::path::PathBuf::from);
    if let Some(dir) = &out_dir {
        std::fs::create_dir_all(dir).ok();
    }
    let only = std::env::var("IAI_RAW_PROBE_ONLY").ok();
    let Some((device, queue)) = iai::gpu::vector::renderer::headless_device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let canvas = RawImporter
        .import(std::path::Path::new(&path))
        .expect("RAW decode");
    let mut scene = canvas.develop_source.clone().expect("RAW scene master");
    if std::env::var("IAI_RAW_PROBE_IDENTITY").is_ok_and(|v| v == "1") {
        // JPEG-like session on the same picture: the default look as a display
        // source, linearized the way Develop opens a JPEG.
        scene = Arc::new(SceneSource::from_display_tiles(
            &canvas.layer_stack.layers[0].tiles,
        ));
        eprintln!("IDENTITY (JPEG-like) session on the default look");
    }
    let (w, h) = (scene.width, scene.height);
    let stack: LayerStack = canvas.layer_stack.clone();
    let layer_id = stack.layers[0].id;
    let default_look = stack.layers[0].tiles.flatten();
    let max_texture = device.limits().max_texture_dimension_2d;
    eprintln!("RAW {w}x{h}, max texture {max_texture}");
    assert!(
        w.max(h) <= max_texture,
        "scene does not fit the headless texture limit"
    );
    let region_base = develop_scene::build_scene_region_base(&scene, TONE_DOWNSAMPLE);

    // Develop window at fit (whole image) and a 100 % crop of the centre.
    let (fvw, fvh) = (1400u32, 930u32);
    let zoom = (fvw as f32 / w as f32).min(fvh as f32 / h as f32);
    let fit = View {
        vw: ((w as f32 * zoom).floor() as u32).min(fvw),
        vh: ((h as f32 * zoom).floor() as u32).min(fvh),
        x: 0.0,
        y: 0.0,
        zoom,
    };
    let (cw, ch) = (1400u32.min(w), 930u32.min(h));
    let one = View {
        vw: cw,
        vh: ch,
        x: ((w - cw) / 2) as f32,
        y: ((h - ch) / 2) as f32,
        zoom: 1.0,
    };
    eprintln!(
        "fit view {}x{} zoom {zoom:.4}; 100% crop {cw}x{ch}",
        fit.vw, fit.vh
    );

    // One compositor (one scene texture) resized per view: two would hold two
    // 16-bit float copies of a large RAW and run a small GPU out of memory.
    let mut comp = CompositorState::new(&device, fit.vw, fit.vh, max_texture);

    // What the Develop window shows before any slider moves (the decoded tiles).
    let neutral_fit = render_view(&mut comp, &device, &queue, &stack, fit, None);
    save(
        &out_dir,
        "fit_neutral_tiles.png",
        &neutral_fit,
        fit.vw,
        fit.vh,
    );
    save(
        &out_dir,
        "fit_neutral_box.png",
        &box_to_view(&default_look, w, h, fit),
        fit.vw,
        fit.vh,
    );
    if let Some(p) = iai::formats::raw_preview::extract(std::path::Path::new(&path)) {
        let cam = resample(&p.rgba, p.width, p.height, fit.vw, fit.vh);
        save(&out_dir, "fit_camera_jpeg.png", &cam, fit.vw, fit.vh);
    }

    for (name, settings) in scenarios() {
        if only
            .as_deref()
            .is_some_and(|o| !o.split(',').any(|n| n == name))
        {
            continue;
        }
        eprintln!("\n== {name} ==");
        let t = std::time::Instant::now();
        let committed_tiles = develop_scene::apply_scene_to_tilemap(&scene, &settings, None);
        let committed = committed_tiles.flatten();
        eprintln!("  commit {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
        let mut committed_stack = stack.clone();
        committed_stack.layers[0].tiles = committed_tiles;

        // 100 %: per-pixel parity of the shader against the commit.
        let (p, ds) = app_preview(&scene, layer_id, &settings, one, &region_base);
        let prev_one = render_view(&mut comp, &device, &queue, &stack, one, Some(p));
        let commit_one = crop(&committed, w, one.x as u32, one.y as u32, cw, ch);
        if let Some(ds) = ds {
            eprintln!("  colour proxy downsample at 100%: {ds}");
        }
        print(
            "100% preview vs commit",
            &compare(
                &prev_one,
                &commit_one,
                cw as usize,
                cw as usize,
                ch as usize,
                16,
            ),
        );

        // Fit: preview vs the commit as the main window composites it, and vs
        // an exact box-average of the commit.
        let (p, ds) = app_preview(&scene, layer_id, &settings, fit, &region_base);
        let prev_fit = render_view(&mut comp, &device, &queue, &stack, fit, Some(p));
        if let Some(ds) = ds {
            eprintln!("  colour proxy downsample at fit: {ds}");
        }
        if std::env::var("IAI_RAW_PROBE_TIME").is_ok_and(|v| v == "1") {
            // Slider-frame cost at fit (display grid on) against the same
            // viewport at 100 % (one sample per pixel), settings nudged every
            // frame so nothing is reused.
            let crop_view = View {
                vw: fit.vw,
                vh: fit.vh,
                x: ((w - fit.vw) / 2) as f32,
                y: ((h - fit.vh) / 2) as f32,
                zoom: 1.0,
            };
            for (label, v, taps) in [
                ("fit at rest", fit, 3),
                ("fit dragging", fit, 1),
                ("100% same size", crop_view, 3),
            ] {
                comp.develop_grid_taps = taps;
                let mut times = Vec::new();
                for frame in 0..12 {
                    let mut nudged = settings.clone();
                    nudged.exposure += 0.01 * frame as f32;
                    let (p, _) = app_preview(&scene, layer_id, &nudged, v, &region_base);
                    comp.configure_viewport(&device, v.vw, v.vh, 1);
                    comp.develop_preview = Some(p);
                    let t = std::time::Instant::now();
                    comp.composite_layers(
                        &device, &queue, &stack, v.x, v.y, v.zoom, None, false, false,
                    );
                    device.poll(wgpu::PollType::wait_indefinitely()).ok();
                    if frame >= 2 {
                        times.push(t.elapsed().as_secs_f64() * 1e3);
                    }
                }
                comp.develop_grid_taps = 3;
                times.sort_by(f64::total_cmp);
                eprintln!(
                    "  frame {label:<15} best {:.1} ms  median {:.1} ms",
                    times[0],
                    times[times.len() / 2]
                );
            }
        }
        let main_fit = render_view(&mut comp, &device, &queue, &committed_stack, fit, None);
        let truth_fit = box_to_view(&committed, w, h, fit);
        let (vw, vh) = (fit.vw as usize, fit.vh as usize);
        print(
            "fit preview vs main-window",
            &compare(&prev_fit, &main_fit, vw, vw, vh, 8),
        );
        print(
            "fit preview vs box(commit)",
            &compare(&prev_fit, &truth_fit, vw, vw, vh, 8),
        );
        print(
            "fit main-window vs box(commit)",
            &compare(&main_fit, &truth_fit, vw, vw, vh, 8),
        );
        if name == "near-neutral" {
            print(
                "fit preview vs neutral tiles",
                &compare(&prev_fit, &neutral_fit, vw, vw, vh, 8),
            );
            let neutral_box = box_to_view(&default_look, w, h, fit);
            print(
                "fit tiles vs box(default)",
                &compare(&neutral_fit, &neutral_box, vw, vw, vh, 8),
            );
        }
        save(
            &out_dir,
            &format!("{name}_fit_preview.png"),
            &prev_fit,
            fit.vw,
            fit.vh,
        );
        save(
            &out_dir,
            &format!("{name}_fit_main.png"),
            &main_fit,
            fit.vw,
            fit.vh,
        );
        save(
            &out_dir,
            &format!("{name}_100_preview.png"),
            &prev_one,
            cw,
            ch,
        );
        save(
            &out_dir,
            &format!("{name}_100_commit.png"),
            &commit_one,
            cw,
            ch,
        );
    }
}

/// Box-resample an RGBA8 image to `dw×dh`.
fn resample(src: &[u8], w: u32, h: u32, dw: u32, dh: u32) -> Vec<u8> {
    let mut out = vec![0u8; (dw * dh * 4) as usize];
    for y in 0..dh {
        let y0 = (y as u64 * h as u64 / dh as u64) as u32;
        let y1 = (((y + 1) as u64 * h as u64 / dh as u64) as u32).max(y0 + 1);
        for x in 0..dw {
            let x0 = (x as u64 * w as u64 / dw as u64) as u32;
            let x1 = (((x + 1) as u64 * w as u64 / dw as u64) as u32).max(x0 + 1);
            let mut acc = [0u64; 4];
            for yy in y0..y1 {
                for xx in x0..x1 {
                    let i = ((yy * w + xx) * 4) as usize;
                    for c in 0..4 {
                        acc[c] += src[i + c] as u64;
                    }
                }
            }
            let k = ((x1 - x0) * (y1 - y0)) as u64;
            let o = ((y * dw + x) * 4) as usize;
            for c in 0..4 {
                out[o + c] = ((acc[c] + k / 2) / k) as u8;
            }
        }
    }
    out
}

/// (mean luma, luma std-dev, mean OKLab chroma, mean R, G, B) of an RGBA8 image.
fn look_stats(px: &[u8]) -> [f32; 6] {
    let lin = |v: u8| {
        let c = v as f32 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let n = (px.len() / 4) as f32;
    let (mut sl, mut sl2, mut sc) = (0f32, 0f32, 0f32);
    let mut rgb = [0f32; 3];
    for p in px.chunks_exact(4) {
        let l = (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) / 255.0;
        sl += l;
        sl2 += l * l;
        let lab =
            iai::core::perceptual_color::linear_srgb_to_oklab([lin(p[0]), lin(p[1]), lin(p[2])]);
        sc += (lab.a * lab.a + lab.b * lab.b).sqrt();
        for c in 0..3 {
            rgb[c] += p[c] as f32;
        }
    }
    let ml = sl / n;
    [
        ml * 255.0,
        ((sl2 / n - ml * ml).max(0.0)).sqrt() * 255.0,
        sc / n * 1000.0,
        rgb[0] / n,
        rgb[1] / n,
        rgb[2] / n,
    ]
}

/// The open-time switch from the camera's embedded JPEG to iAi's default RAW
/// render. IAI_RAW_LOOK=<file>[;<file>…] [IAI_RAW_PROBE_OUT=<dir>]
#[test]
#[ignore = "manual probe; needs local RAW files and --release"]
fn raw_open_look_probe() {
    let Ok(files) = std::env::var("IAI_RAW_LOOK") else {
        eprintln!("IAI_RAW_LOOK not set; skipping");
        return;
    };
    let out_dir = std::env::var("IAI_RAW_PROBE_OUT")
        .ok()
        .map(std::path::PathBuf::from);
    if let Some(dir) = &out_dir {
        std::fs::create_dir_all(dir).ok();
    }
    for (k, file) in files.split(';').filter(|f| !f.is_empty()).enumerate() {
        let path = std::path::Path::new(file);
        let Some(preview) = iai::formats::raw_preview::extract(path) else {
            eprintln!("{file}: no embedded preview");
            continue;
        };
        let canvas = RawImporter.import(path).expect("RAW decode");
        let scene = canvas.develop_source.clone().expect("scene");
        let look = canvas.layer_stack.layers[0].tiles.flatten();
        let dw = 400u32;
        let dh = (dw as f32 * canvas.height as f32 / canvas.width as f32).round() as u32;
        let a = resample(&preview.rgba, preview.width, preview.height, dw, dh);
        let b = resample(&look, canvas.width, canvas.height, dw, dh);
        let (sa, sb) = (look_stats(&a), look_stats(&b));
        eprintln!(
            "\n{}\n  camera JPEG : luma {:5.1}  contrast {:5.1}  chroma {:5.1}  RGB {:5.1} {:5.1} {:5.1}\n  iAi default : luma {:5.1}  contrast {:5.1}  chroma {:5.1}  RGB {:5.1} {:5.1} {:5.1}",
            path.file_name().unwrap().to_string_lossy(),
            sa[0], sa[1], sa[2], sa[3], sa[4], sa[5], sb[0], sb[1], sb[2], sb[3], sb[4], sb[5]
        );
        if let Some(p) = &scene.camera_profile {
            eprintln!(
                "  jpeg match {:?}; recipe {:?}",
                p.jpeg_match, p.raw_render_recipe
            );
            eprintln!("  profile {:?}", p.resolution.selected);
        }
        let mut both = vec![0u8; (dw * 2 * dh * 4) as usize];
        for y in 0..dh as usize {
            let row = dw as usize * 4;
            both[y * row * 2..y * row * 2 + row].copy_from_slice(&a[y * row..(y + 1) * row]);
            both[y * row * 2 + row..(y + 1) * row * 2].copy_from_slice(&b[y * row..(y + 1) * row]);
        }
        save(&out_dir, &format!("open_look_{k}.png"), &both, dw * 2, dh);
    }
}
