//! GPU uniform/texture pushes: canvas blit mapping, cursor ring, selection
//! mask and the soft-proof/channel-view display overrides.

use crate::app::state::App;
use crate::gpu::{CanvasUniforms, CursorUniforms, SelectionUniforms};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DisplayWindow {
    Main,
    Develop,
}

fn system_profile_auto_refresh_enabled(cms_enabled: bool, from_system: bool) -> bool {
    cms_enabled && from_system
}

/// Canvas-space bounds that must remain visible while Crop is pending. The
/// ordinary canvas remains visible (dimmed by the Crop overlay), while any part
/// of the crop frame outside it is added so the live preview matches the area
/// that commit will create.
fn canvas_preview_bounds(canvas_w: f32, canvas_h: f32, crop: Option<([f32; 4], f32)>) -> [f32; 4] {
    let mut bounds = [0.0, 0.0, canvas_w, canvas_h];
    let Some(([x0, y0, x1, y1], rotation)) = crop else {
        return bounds;
    };
    if ![x0, y0, x1, y1, rotation].iter().all(|v| v.is_finite()) {
        return bounds;
    }

    let left = x0.min(x1);
    let right = x0.max(x1);
    let top = y0.min(y1);
    let bottom = y0.max(y1);
    let cx = (left + right) * 0.5;
    let cy = (top + bottom) * 0.5;
    let (sin, cos) = rotation.sin_cos();
    for (x, y) in [(left, top), (right, top), (right, bottom), (left, bottom)] {
        let dx = x - cx;
        let dy = y - cy;
        let rx = cx + cos * dx - sin * dy;
        let ry = cy + sin * dx + cos * dy;
        bounds[0] = bounds[0].min(rx);
        bounds[1] = bounds[1].min(ry);
        bounds[2] = bounds[2].max(rx);
        bounds[3] = bounds[3].max(ry);
    }
    bounds
}

impl App {
    fn system_display_profile_for_window(
        &self,
        target: DisplayWindow,
    ) -> Option<(String, Vec<u8>)> {
        let window = match target {
            DisplayWindow::Main => self.win.window.as_deref(),
            DisplayWindow::Develop => self.win.develop_window.as_deref(),
        }?;
        let hwnd = crate::file_io::dialog_parent(window)?.hwnd();
        crate::core::cms::system_display_profile_for_hwnd(hwnd)
    }

    /// Refresh a system-managed monitor profile after an OS-window move. A
    /// byte-for-byte comparison avoids rebuilding the LUT for the many Moved
    /// events emitted while dragging inside one monitor.
    fn refresh_system_display_profile(&mut self, target: DisplayWindow) {
        if !system_profile_auto_refresh_enabled(
            self.shell.display_cms_enabled,
            self.shell.display_profile_from_system,
        ) {
            return;
        }
        let Some((name, bytes)) = self.system_display_profile_for_window(target) else {
            return;
        };
        let changed = match target {
            DisplayWindow::Main => self.shell.display_profile.as_deref() != Some(bytes.as_slice()),
            DisplayWindow::Develop => {
                self.shell.develop_display_profile.as_deref() != Some(bytes.as_slice())
            }
        };
        if !changed {
            return;
        }
        match target {
            DisplayWindow::Main => {
                self.shell.display_profile = Some(bytes);
                self.shell.display_profile_name = name;
            }
            DisplayWindow::Develop => {
                self.shell.develop_display_profile = Some(bytes);
                self.shell.develop_display_profile_name = name;
            }
        }
        self.apply_proof_settings();
    }

    pub(crate) fn enable_system_display_profiles(&mut self) -> Option<String> {
        let (main_name, main_bytes) =
            self.system_display_profile_for_window(DisplayWindow::Main)?;
        let (develop_name, develop_bytes) = self
            .system_display_profile_for_window(DisplayWindow::Develop)
            .unwrap_or_else(|| (main_name.clone(), main_bytes.clone()));
        self.shell.display_profile_from_system = true;
        self.shell.display_cms_enabled = true;
        self.shell.display_profile = Some(main_bytes);
        self.shell.display_profile_name = main_name.clone();
        self.shell.develop_display_profile = Some(develop_bytes);
        self.shell.develop_display_profile_name = develop_name;
        self.apply_proof_settings();
        Some(main_name)
    }

    pub(crate) fn refresh_main_system_display_profile(&mut self) {
        self.refresh_system_display_profile(DisplayWindow::Main);
    }

    pub(crate) fn refresh_develop_system_display_profile(&mut self) {
        self.refresh_system_display_profile(DisplayWindow::Develop);
    }

    /// Canvas rectangle on screen (physical px) used to scissor the canvas quad.
    /// Normally this is the committed canvas. During Crop it expands to the union
    /// of the old canvas and the pending crop frame, allowing transformed image
    /// pixels beyond the old edge to remain visible before commit.
    pub fn canvas_screen_clip(&self) -> Option<(u32, u32, u32, u32)> {
        let win = self.win.window.as_ref()?;
        let sz = win.inner_size();
        let sw = sz.width as f32;
        let sh = sz.height as f32;
        let cw = self.docs.documents[self.docs.active_doc_idx].canvas.width as f32;
        let ch = self.docs.documents[self.docs.active_doc_idx].canvas.height as f32;
        let crop = (self.edit.tools.active_id() == crate::tools::ToolId::Crop
            && self.edit.tools.crop().has_selection())
        .then(|| {
            let c = self.edit.tools.crop();
            ([c.crop_x0, c.crop_y0, c.crop_x1, c.crop_y1], c.rotation)
        });
        let [bx0, by0, bx1, by1] = canvas_preview_bounds(cw, ch, crop);
        let x0 = (self.edit.view.offset_x + bx0 * self.edit.view.zoom)
            .max(0.0)
            .min(sw)
            .floor();
        let y0 = (self.edit.view.offset_y + by0 * self.edit.view.zoom)
            .max(0.0)
            .min(sh)
            .floor();
        let x1 = (self.edit.view.offset_x + bx1 * self.edit.view.zoom)
            .max(0.0)
            .min(sw)
            .ceil();
        let y1 = (self.edit.view.offset_y + by1 * self.edit.view.zoom)
            .max(0.0)
            .min(sh)
            .ceil();
        let w = x1 - x0;
        let h = y1 - y0;
        if w < 1.0 || h < 1.0 {
            return None;
        }
        Some((x0 as u32, y0 as u32, w as u32, h as u32))
    }

    pub fn push_canvas_uniforms(&self) {
        if let (Some(gpu), Some(win)) = (&self.win.gpu, &self.win.window) {
            let sz = win.inner_size();
            // Mode A (canvas-space): vp_mode 0 → the blit positions the canvas-sized
            // texture with offset+zoom. Mode B (screen-space): vp_mode 1 → fullscreen
            // blit of the already-transformed viewport texture.
            let vp_mode = if gpu.compositor.canvas_space {
                0.0_f32
            } else {
                1.0_f32
            };
            // Channels panel plate view (0 = composite, 1..3 = R/G/B,
            // 4 = saved alpha plane). RGB plates are converted in the blit
            // shader; saved alpha and CMYK ink plates are uploaded as grayscale
            // override textures.
            let channel_view = match self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .channels
                .view
            {
                crate::core::channels::ChannelView::Single(c) => 1.0 + c.min(2) as f32,
                crate::core::channels::ChannelView::Alpha(_) => 4.0,
                _ => 0.0,
            };
            gpu.write_canvas_uniforms(&CanvasUniforms {
                offset: [self.edit.view.offset_x, self.edit.view.offset_y],
                zoom: self.edit.view.zoom,
                vp_mode,
                screen_size: [sz.width as f32, sz.height as f32],
                proof_enabled: if self.shell.proof_enabled
                    || self.shell.display_cms_enabled
                    || self.docs.documents[self.docs.active_doc_idx]
                        .canvas
                        .color_space
                        != crate::core::canvas::ColorSpace::SRGB
                {
                    1.0
                } else {
                    0.0
                },
                channel_view,
            });
        }
    }

    /// Rebuild + upload the display 3D LUT = soft-proof (if on) composed with the
    /// monitor profile (display CMS, if on), and push the LUT-active flag. Cheap (a
    /// few thousand lcms2 samples); called whenever a View ▸ Proof or Display
    /// Profile setting changes. Display-only — never alters document pixels.
    pub fn apply_proof_settings(&mut self) {
        let document_profile = {
            let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
            (canvas.color_space != crate::core::canvas::ColorSpace::SRGB
                && !canvas.icc_profile.data.is_empty())
            .then(|| canvas.icc_profile.data.clone())
        };
        let active = self.shell.proof_enabled
            || self.shell.display_cms_enabled
            || document_profile.is_some();
        let proof = self
            .shell
            .proof_enabled
            .then(|| self.shell.proof_target.icc_bytes());
        let main_monitor = self
            .shell
            .display_cms_enabled
            .then(|| self.shell.display_profile.as_deref())
            .flatten();
        let develop_monitor = self
            .shell
            .display_cms_enabled
            .then(|| {
                self.shell
                    .develop_display_profile
                    .as_deref()
                    .or(main_monitor)
            })
            .flatten();
        let build_lut = |monitor: Option<&[u8]>| {
            if !active {
                return crate::core::cms::identity_lut(crate::core::cms::PROOF_LUT_SIZE);
            }
            crate::core::cms::build_document_display_lut(
                document_profile.as_deref(),
                proof.as_deref(),
                self.shell.proof_gamut_warn,
                monitor,
                crate::core::cms::PROOF_LUT_SIZE,
            )
            .unwrap_or_else(|| crate::core::cms::identity_lut(crate::core::cms::PROOF_LUT_SIZE))
        };
        let main_lut = build_lut(main_monitor);
        let develop_lut = build_lut(develop_monitor);
        if let Some(gpu) = &self.win.gpu {
            gpu.upload_proof_lut(&main_lut);
            gpu.upload_develop_proof_lut(&develop_lut);
        }
        self.push_canvas_uniforms();
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
        if let Some(w) = &self.win.develop_window {
            w.request_redraw();
        }
    }

    pub fn refresh_channel_view_display(&mut self) {
        self.upload_viewed_channel_plane(None);
    }

    pub(in crate::app) fn active_view_uses_canvas_override(&self) -> bool {
        let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
        matches!(
            canvas.channels.view,
            crate::core::channels::ChannelView::Alpha(_)
        ) || (canvas.is_cmyk()
            && matches!(
                canvas.channels.view,
                crate::core::channels::ChannelView::Single(_)
            ))
    }

    pub fn upload_viewed_channel_plane(&mut self, dirty: Option<(u32, u32, u32, u32)>) {
        self.push_canvas_uniforms();

        let (view, is_cmyk) = {
            let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
            (canvas.channels.view, canvas.is_cmyk())
        };
        if !self.active_view_uses_canvas_override() {
            if let Some(gpu) = &mut self.win.gpu {
                gpu.clear_canvas_override();
            }
            return;
        }

        let view_offset_x = self.edit.view.offset_x;
        let view_offset_y = self.edit.view.offset_y;
        let zoom = self.edit.view.zoom;
        match view {
            crate::core::channels::ChannelView::Alpha(_) => {
                let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
                let Some(channel) = canvas.channels.viewed_alpha() else {
                    if let Some(gpu) = &mut self.win.gpu {
                        gpu.clear_canvas_override();
                    }
                    return;
                };

                if let Some(gpu) = &mut self.win.gpu {
                    gpu.upload_alpha_plane(
                        &channel.mask,
                        channel.width,
                        channel.height,
                        view_offset_x,
                        view_offset_y,
                        zoom,
                        dirty,
                    );
                }
                canvas.plane_dirty.clear();
            }
            crate::core::channels::ChannelView::Single(c) if is_cmyk => {
                let (mask, canvas_w, canvas_h) = {
                    let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
                    let Some(mask) = canvas.cmyk_plate_preview_mask(c) else {
                        if let Some(gpu) = &mut self.win.gpu {
                            gpu.clear_canvas_override();
                        }
                        return;
                    };
                    (mask, canvas.width, canvas.height)
                };

                if let Some(gpu) = &mut self.win.gpu {
                    gpu.upload_alpha_plane(
                        &mask,
                        canvas_w,
                        canvas_h,
                        view_offset_x,
                        view_offset_y,
                        zoom,
                        dirty,
                    );
                }
            }
            _ => {
                if let Some(gpu) = &mut self.win.gpu {
                    gpu.clear_canvas_override();
                }
            }
        }
    }

    pub fn push_cursor_uniforms(&self) {
        if let (Some(gpu), Some(win)) = (&self.win.gpu, &self.win.window) {
            let sz = win.inner_size();

            let is_native_ring_tool = matches!(
                self.edit.tools.active_id(),
                crate::tools::ToolId::Brush
                    | crate::tools::ToolId::Eraser
                    | crate::tools::ToolId::Pencil
                    | crate::tools::ToolId::Clone
                    | crate::tools::ToolId::Repair
                    | crate::tools::ToolId::SmartSelect
                    | crate::tools::ToolId::RefineBrush
                    | crate::tools::ToolId::Smudge
                    | crate::tools::ToolId::Dodge
                    | crate::tools::ToolId::Burn
                    // Vector Brush uses the OS ring like the pixel brushes; without
                    // this it would get the OS ring AND the GPU ring (two cursors).
                    | crate::tools::ToolId::VectorBrush
            );
            let uses_os_ring = matches!(
                self.edit.tools.active_id(),
                crate::tools::ToolId::Brush
                    | crate::tools::ToolId::Eraser
                    | crate::tools::ToolId::Pencil
                    | crate::tools::ToolId::Clone
                    | crate::tools::ToolId::Repair
                    | crate::tools::ToolId::SmartSelect
                    | crate::tools::ToolId::RefineBrush
                    | crate::tools::ToolId::Smudge
                    | crate::tools::ToolId::Dodge
                    | crate::tools::ToolId::Burn
                    | crate::tools::ToolId::VectorBrush
            );
            let ring_screen_radius = self.edit.tools.cursor_size() * self.edit.view.zoom;
            let os_ring_too_big = uses_os_ring
                && ring_screen_radius.round() > crate::app::state::MAX_NATIVE_RING_RADIUS as f32;
            let eyedrop_cursor = self.edit.tools.active_id() == crate::tools::ToolId::Eyedropper
                || (self.edit.input.alt_held
                    && !self.edit.input.alt_right_dragging
                    && matches!(
                        self.edit.tools.active_id(),
                        crate::tools::ToolId::Brush | crate::tools::ToolId::Pencil
                    ));
            let over_color_dialog = self.shell.ui.show_paint_color_dialog;
            // Warp draws its own egui brush ring; suppress the tool's GPU ring so
            // the two don't stack (the active tool is still "a brush" underneath).
            let use_gpu_ring = self.edit.warp_state.is_none()
                && (self.edit.input.alt_right_dragging
                    || (!eyedrop_cursor
                        && !over_color_dialog
                        && !self.edit.input.was_over_ui
                        && (!is_native_ring_tool || os_ring_too_big)));

            let radius = if use_gpu_ring {
                let r = ring_screen_radius;
                if r < 0.5 {
                    0.0
                } else {
                    r
                }
            } else {
                0.0
            };

            let (cx, cy) = if self.edit.input.alt_right_dragging {
                (
                    self.edit.input.alt_drag_start_x,
                    self.edit.input.alt_drag_start_y,
                )
            } else {
                (self.edit.input.mouse_x, self.edit.input.mouse_y)
            };
            gpu.write_cursor_uniforms(&CursorUniforms {
                cursor_pos: [cx, cy],
                brush_size: radius,
                _pad: 0.0,
                screen_size: [sz.width as f32, sz.height as f32],
                _pad2: [0.0, 0.0],
            });
        }
    }

    pub fn push_selection_uniforms(&mut self) {
        self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .selection
            .refresh_bbox();
        if let (Some(gpu), Some(win)) = (&self.win.gpu, &self.win.window) {
            let sz = win.inner_size();
            let sw = sz.width as f32;
            let sh = sz.height as f32;
            let sel = &self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .selection;
            let elapsed = self.win.start_time.elapsed().as_secs_f32();

            if gpu.is_large_canvas {
                let (x0, y0, x1, y1) = if sel.active {
                    let (cx0, cy0, cx1, cy1) = sel.bounding_box_cached();
                    let ox = self.edit.view.offset_x;
                    let oy = self.edit.view.offset_y;
                    let z = self.edit.view.zoom;
                    (ox + cx0 * z, oy + cy0 * z, ox + cx1 * z, oy + cy1 * z)
                } else {
                    (0.0, 0.0, 0.0, 0.0)
                };
                gpu.write_selection_uniforms(&SelectionUniforms {
                    rect: [x0, y0, x1, y1],
                    offset: [0.0, 0.0],
                    zoom: 1.0,
                    time: elapsed,
                    screen_size: [sw, sh],
                    canvas_size: [sw, sh],
                    sel_offset: [0.0, 0.0],
                    _pad: [0.0, 0.0],
                });
            } else {
                let (x0, y0, x1, y1) = if sel.active {
                    sel.bounding_box_cached()
                } else {
                    (0.0, 0.0, 0.0, 0.0)
                };
                gpu.write_selection_uniforms(&SelectionUniforms {
                    rect: [x0, y0, x1, y1],
                    offset: [self.edit.view.offset_x, self.edit.view.offset_y],
                    zoom: self.edit.view.zoom,
                    time: elapsed,
                    screen_size: [sw, sh],
                    canvas_size: [
                        self.docs.documents[self.docs.active_doc_idx].canvas.width as f32,
                        self.docs.documents[self.docs.active_doc_idx].canvas.height as f32,
                    ],
                    sel_offset: [sel.offset.0 as f32, sel.offset.1 as f32],
                    _pad: [0.0, 0.0],
                });
            }
        }
    }

    pub fn upload_selection_mask(&mut self) {
        let Some(gpu) = &self.win.gpu else {
            return;
        };
        let sel = &self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .selection;

        let mut key = sel.mask_revision
            ^ ((sel.width as u64) << 32)
            ^ sel.height as u64
            ^ ((self.docs.active_doc_idx as u64) << 48);
        if gpu.is_large_canvas {
            let mut mix = |v: u64| {
                key ^= v
                    .wrapping_add(0x9e37_79b9_7f4a_7c15)
                    .wrapping_add(key << 6)
                    .wrapping_add(key >> 2);
            };
            let screen = self.win.window.as_ref().map(|w| w.inner_size());
            if let Some(sz) = screen {
                mix(sz.width as u64);
                mix((sz.height as u64) << 32);
            }
            mix(self.edit.view.offset_x.to_bits() as u64);
            mix((self.edit.view.offset_y.to_bits() as u64) << 32);
            mix(self.edit.view.zoom.to_bits() as u64);
            mix(sel.offset.0 as i64 as u64);
            mix((sel.offset.1 as i64 as u64).rotate_left(32));
        }

        if key == self.win.last_uploaded_mask_key {
            return;
        }
        self.win.last_uploaded_mask_key = key;

        gpu.update_selection_mask(
            &sel.mask,
            sel.width,
            sel.height,
            self.edit.view.offset_x,
            self.edit.view.offset_y,
            self.edit.view.zoom,
            sel.offset.0,
            sel.offset.1,
        );
    }
}

#[cfg(test)]
mod display_profile_tests {
    use super::{canvas_preview_bounds, system_profile_auto_refresh_enabled};

    #[test]
    fn only_an_enabled_system_profile_follows_window_moves() {
        assert!(system_profile_auto_refresh_enabled(true, true));
        assert!(!system_profile_auto_refresh_enabled(true, false));
        assert!(!system_profile_auto_refresh_enabled(false, true));
    }

    #[test]
    fn crop_preview_bounds_include_area_beyond_original_canvas() {
        assert_eq!(
            canvas_preview_bounds(400.0, 400.0, Some(([40.0, -80.0, 360.0, 240.0], 0.0))),
            [0.0, -80.0, 400.0, 400.0]
        );
    }

    #[test]
    fn normal_preview_bounds_remain_the_committed_canvas() {
        assert_eq!(
            canvas_preview_bounds(400.0, 300.0, None),
            [0.0, 0.0, 400.0, 300.0]
        );
    }
}
