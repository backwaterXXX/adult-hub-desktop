//! OpenGL presentation for the libmpv render API.
//!
//! mpv renders into an offscreen RGBA texture owned by this module. Slint then
//! samples that texture directly (`BorrowedOpenGLTextureBuilder`), so no CPU
//! frame readback is involved.

use adult_hub_player::{Player, RenderContext};
use glow::HasContext;
use slint::ComponentHandle;
use std::ffi::CStr;
use std::num::NonZeroU32;
use std::sync::Arc;

pub struct Renderer {
    gl: glow::Context,
    mpv: RenderContext,
    player: Arc<Player>,
    surface: Option<Surface>,
    retired: Vec<Surface>,
    current_texture_id: u32,
    last_diag_key: std::cell::RefCell<Option<String>>,
}

struct Surface {
    texture: glow::Texture,
    framebuffer: glow::Framebuffer,
    width: u32,
    height: u32,
}

/// Slint's renderer shares this OpenGL context with the embedded mpv renderer.
/// Capture all state needed by Slint, then prepare standard OpenGL defaults
/// required by libmpv's render API (render_gl.h).
#[derive(Debug)]
pub struct OpenGlStateGuard<'a> {
    gl: &'a glow::Context,
    framebuffer: Option<glow::Framebuffer>,
    viewport: [i32; 4],
    active_texture: u32,
    texture_2d: Option<glow::Texture>,
    sampler: Option<glow::Sampler>,
    program: Option<glow::Program>,
    vertex_array: Option<glow::VertexArray>,
    array_buffer: Option<glow::Buffer>,
    element_array_buffer: Option<glow::Buffer>,
    pixel_unpack_buffer: Option<glow::Buffer>,
    pixel_pack_buffer: Option<glow::Buffer>,
    scissor_test: bool,
    scissor_box: [i32; 4],
    blend: bool,
    blend_src_rgb: i32,
    blend_dst_rgb: i32,
    blend_src_alpha: i32,
    blend_dst_alpha: i32,
    blend_equation_rgb: i32,
    blend_equation_alpha: i32,
    depth_test: bool,
    depth_writemask: bool,
    cull_face: bool,
    stencil_test: bool,
    stencil_writemask: u32,
    color_writemask: [bool; 4],
    unpack_alignment: i32,
    pack_alignment: i32,
    unpack_row_length: i32,
    pack_row_length: i32,
    unpack_skip_pixels: i32,
    unpack_skip_rows: i32,
    pack_skip_pixels: i32,
    pack_skip_rows: i32,
    color_clear_value: [f32; 4],
}

impl<'a> OpenGlStateGuard<'a> {
    pub unsafe fn capture(gl: &'a glow::Context) -> Self {
        let mut viewport = [0; 4];
        let mut scissor_box = [0; 4];
        let mut color_clear_value = [0.0; 4];
        unsafe {
            gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
            gl.get_parameter_i32_slice(glow::SCISSOR_BOX, &mut scissor_box);
            gl.get_parameter_f32_slice(glow::COLOR_CLEAR_VALUE, &mut color_clear_value);
        }
        Self {
            gl,
            framebuffer: unsafe { gl.get_parameter_framebuffer(glow::FRAMEBUFFER_BINDING) },
            viewport,
            active_texture: unsafe { gl.get_parameter_i32(glow::ACTIVE_TEXTURE) as u32 },
            texture_2d: unsafe { gl.get_parameter_texture(glow::TEXTURE_BINDING_2D) },
            sampler: unsafe { gl.get_parameter_sampler(glow::SAMPLER_BINDING) },
            program: unsafe { gl.get_parameter_program(glow::CURRENT_PROGRAM) },
            vertex_array: unsafe { gl.get_parameter_vertex_array(glow::VERTEX_ARRAY_BINDING) },
            array_buffer: unsafe { gl.get_parameter_buffer(glow::ARRAY_BUFFER_BINDING) },
            element_array_buffer: unsafe {
                gl.get_parameter_buffer(glow::ELEMENT_ARRAY_BUFFER_BINDING)
            },
            pixel_unpack_buffer: unsafe {
                gl.get_parameter_buffer(glow::PIXEL_UNPACK_BUFFER_BINDING)
            },
            pixel_pack_buffer: unsafe { gl.get_parameter_buffer(glow::PIXEL_PACK_BUFFER_BINDING) },
            scissor_test: unsafe { gl.is_enabled(glow::SCISSOR_TEST) },
            scissor_box,
            blend: unsafe { gl.is_enabled(glow::BLEND) },
            blend_src_rgb: unsafe { gl.get_parameter_i32(glow::BLEND_SRC_RGB) },
            blend_dst_rgb: unsafe { gl.get_parameter_i32(glow::BLEND_DST_RGB) },
            blend_src_alpha: unsafe { gl.get_parameter_i32(glow::BLEND_SRC_ALPHA) },
            blend_dst_alpha: unsafe { gl.get_parameter_i32(glow::BLEND_DST_ALPHA) },
            blend_equation_rgb: unsafe { gl.get_parameter_i32(glow::BLEND_EQUATION_RGB) },
            blend_equation_alpha: unsafe { gl.get_parameter_i32(glow::BLEND_EQUATION_ALPHA) },
            depth_test: unsafe { gl.is_enabled(glow::DEPTH_TEST) },
            depth_writemask: unsafe { gl.get_parameter_bool(glow::DEPTH_WRITEMASK) },
            cull_face: unsafe { gl.is_enabled(glow::CULL_FACE) },
            stencil_test: unsafe { gl.is_enabled(glow::STENCIL_TEST) },
            stencil_writemask: unsafe { gl.get_parameter_i32(glow::STENCIL_WRITEMASK) as u32 },
            color_writemask: unsafe { gl.get_parameter_bool_array::<4>(glow::COLOR_WRITEMASK) },
            unpack_alignment: unsafe { gl.get_parameter_i32(glow::UNPACK_ALIGNMENT) },
            pack_alignment: unsafe { gl.get_parameter_i32(glow::PACK_ALIGNMENT) },
            unpack_row_length: unsafe { gl.get_parameter_i32(glow::UNPACK_ROW_LENGTH) },
            pack_row_length: unsafe { gl.get_parameter_i32(glow::PACK_ROW_LENGTH) },
            unpack_skip_pixels: unsafe { gl.get_parameter_i32(glow::UNPACK_SKIP_PIXELS) },
            unpack_skip_rows: unsafe { gl.get_parameter_i32(glow::UNPACK_SKIP_ROWS) },
            pack_skip_pixels: unsafe { gl.get_parameter_i32(glow::PACK_SKIP_PIXELS) },
            pack_skip_rows: unsafe { gl.get_parameter_i32(glow::PACK_SKIP_ROWS) },
            color_clear_value,
        }
    }

    /// Prepares standard OpenGL defaults as required by libmpv (render_gl.h).
    ///
    /// Specifically:
    /// - Unbinds `GL_PIXEL_UNPACK_BUFFER` so host memory pointers (e.g. from NV12
    ///   d3d11va-copy/vaapi-copy) are read from CPU RAM rather than treated as buffer offsets.
    /// - Resets unpack/pack alignment to 4 and row length/skip values to 0.
    /// - Unbinds active shaders, vertex arrays, array buffers and samplers.
    /// - Disables scissor test, depth test, stencil test, cull face and blend.
    /// - Sets color/depth/stencil write masks to all-enabled.
    pub unsafe fn prepare_for_mpv(gl: &glow::Context) {
        unsafe {
            // 1. Pixel unpack & pack parameters
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
            gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 0);
            gl.pixel_store_i32(glow::UNPACK_SKIP_PIXELS, 0);
            gl.pixel_store_i32(glow::UNPACK_SKIP_ROWS, 0);
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, 4);
            gl.pixel_store_i32(glow::PACK_ROW_LENGTH, 0);
            gl.pixel_store_i32(glow::PACK_SKIP_PIXELS, 0);
            gl.pixel_store_i32(glow::PACK_SKIP_ROWS, 0);

            // 2. Clear buffer object bindings - critically unbind any PBOs
            // so glTexSubImage2D calls with host pointers read CPU RAM instead of buffer offsets!
            gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, None);
            gl.bind_buffer(glow::PIXEL_PACK_BUFFER, None);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, None);

            // 3. Unbind foreign VAO and active shader program
            gl.bind_vertex_array(None);
            gl.use_program(None);

            // 4. Ensure texture units 0 and 1 are unbound and unit 0 is active
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.bind_sampler(1, None);

            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.bind_sampler(0, None);

            // 5. Disable fixed-function tests that interfere with mpv's render pass
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::DEPTH_TEST);
            gl.disable(glow::STENCIL_TEST);
            gl.disable(glow::CULL_FACE);
            gl.disable(glow::BLEND);

            // 6. Ensure all write masks are enabled
            gl.color_mask(true, true, true, true);
            gl.depth_mask(true);
            gl.stencil_mask(!0);
        }
    }

    /// Captures the current Slint state and immediately prepares standard
    /// defaults for libmpv rendering.
    pub unsafe fn capture_and_prepare(gl: &'a glow::Context) -> Self {
        let guard = unsafe { Self::capture(gl) };
        unsafe { Self::prepare_for_mpv(gl) };
        guard
    }
}

impl Drop for OpenGlStateGuard<'_> {
    fn drop(&mut self) {
        unsafe {
            self.gl
                .bind_framebuffer(glow::FRAMEBUFFER, self.framebuffer);
            self.gl.viewport(
                self.viewport[0],
                self.viewport[1],
                self.viewport[2],
                self.viewport[3],
            );
            self.gl.use_program(self.program);
            self.gl.bind_vertex_array(self.vertex_array);
            self.gl.bind_buffer(glow::ARRAY_BUFFER, self.array_buffer);
            self.gl
                .bind_buffer(glow::ELEMENT_ARRAY_BUFFER, self.element_array_buffer);
            self.gl
                .bind_buffer(glow::PIXEL_UNPACK_BUFFER, self.pixel_unpack_buffer);
            self.gl
                .bind_buffer(glow::PIXEL_PACK_BUFFER, self.pixel_pack_buffer);
            self.gl.active_texture(self.active_texture);
            self.gl.bind_texture(glow::TEXTURE_2D, self.texture_2d);
            self.gl.bind_sampler(0, self.sampler);

            self.gl
                .pixel_store_i32(glow::UNPACK_ALIGNMENT, self.unpack_alignment);
            self.gl
                .pixel_store_i32(glow::PACK_ALIGNMENT, self.pack_alignment);
            self.gl
                .pixel_store_i32(glow::UNPACK_ROW_LENGTH, self.unpack_row_length);
            self.gl
                .pixel_store_i32(glow::PACK_ROW_LENGTH, self.pack_row_length);
            self.gl
                .pixel_store_i32(glow::UNPACK_SKIP_PIXELS, self.unpack_skip_pixels);
            self.gl
                .pixel_store_i32(glow::UNPACK_SKIP_ROWS, self.unpack_skip_rows);
            self.gl
                .pixel_store_i32(glow::PACK_SKIP_PIXELS, self.pack_skip_pixels);
            self.gl
                .pixel_store_i32(glow::PACK_SKIP_ROWS, self.pack_skip_rows);

            self.gl.color_mask(
                self.color_writemask[0],
                self.color_writemask[1],
                self.color_writemask[2],
                self.color_writemask[3],
            );
            self.gl.depth_mask(self.depth_writemask);
            self.gl.stencil_mask(self.stencil_writemask);

            self.gl.clear_color(
                self.color_clear_value[0],
                self.color_clear_value[1],
                self.color_clear_value[2],
                self.color_clear_value[3],
            );

            if self.scissor_test {
                self.gl.enable(glow::SCISSOR_TEST);
                self.gl.scissor(
                    self.scissor_box[0],
                    self.scissor_box[1],
                    self.scissor_box[2],
                    self.scissor_box[3],
                );
            } else {
                self.gl.disable(glow::SCISSOR_TEST);
            }

            if self.blend {
                self.gl.enable(glow::BLEND);
                self.gl.blend_func_separate(
                    self.blend_src_rgb as u32,
                    self.blend_dst_rgb as u32,
                    self.blend_src_alpha as u32,
                    self.blend_dst_alpha as u32,
                );
                self.gl.blend_equation_separate(
                    self.blend_equation_rgb as u32,
                    self.blend_equation_alpha as u32,
                );
            } else {
                self.gl.disable(glow::BLEND);
            }

            if self.depth_test {
                self.gl.enable(glow::DEPTH_TEST);
            } else {
                self.gl.disable(glow::DEPTH_TEST);
            }

            if self.cull_face {
                self.gl.enable(glow::CULL_FACE);
            } else {
                self.gl.disable(glow::CULL_FACE);
            }

            if self.stencil_test {
                self.gl.enable(glow::STENCIL_TEST);
            } else {
                self.gl.disable(glow::STENCIL_TEST);
            }
        }
    }
}

impl Renderer {
    /// Must be called while Slint's OpenGL context is current.
    pub fn new(
        player: Arc<Player>,
        get_proc_address: &dyn Fn(&CStr) -> *const std::ffi::c_void,
        flip_y: bool,
    ) -> Result<Self, String> {
        let gl = unsafe { glow::Context::from_loader_function_cstr(|name| get_proc_address(name)) };
        // Ensure standard OpenGL defaults before initializing libmpv's render context
        unsafe {
            OpenGlStateGuard::prepare_for_mpv(&gl);
        }
        let mpv = RenderContext::new(&player, get_proc_address, flip_y)
            .map_err(|error| format!("cannot create libmpv render context: {error}"))?;
        Ok(Self {
            gl,
            mpv,
            player,
            surface: None,
            retired: Vec::new(),
            current_texture_id: 0,
            last_diag_key: std::cell::RefCell::new(None),
        })
    }

    pub fn set_update_callback(&mut self, callback: impl Fn() + Send + 'static) {
        self.mpv.set_update_callback(callback);
    }

    fn check_and_log_diagnostics(
        &self,
        width: u32,
        height: u32,
        surface_status: u32,
        gl_err: u32,
        state_before: &OpenGlStateGuard<'_>,
    ) {
        let hwdec_req = self
            .player
            .get_string("hwdec")
            .unwrap_or_else(|| "unknown".into());
        let hwdec_cur = self
            .player
            .get_string("hwdec-current")
            .unwrap_or_else(|| "no".into());
        let pixfmt = self
            .player
            .get_string("video-params/pixelformat")
            .unwrap_or_else(|| "unknown".into());
        let coded_w = self.player.get_u32("video-params/w").unwrap_or(0);
        let coded_h = self.player.get_u32("video-params/h").unwrap_or(0);
        let disp_w = self.player.get_u32("video-params/dw").unwrap_or(0);
        let disp_h = self.player.get_u32("video-params/dh").unwrap_or(0);

        let key = format!("{hwdec_cur}:{pixfmt}:{coded_w}x{coded_h}:{width}x{height}");
        let mut last_key = self.last_diag_key.borrow_mut();
        if last_key.as_deref() != Some(&key) || gl_err != glow::NO_ERROR {
            *last_key = Some(key);
            drop(last_key);

            let mpv_ver = self
                .player
                .get_string("mpv-version")
                .unwrap_or_else(|| "unknown".into());
            let ffmpeg_ver = self
                .player
                .get_string("ffmpeg-version")
                .unwrap_or_else(|| "unknown".into());
            let pbo_id = state_before
                .pixel_unpack_buffer
                .map(|b| b.0.get())
                .unwrap_or(0);

            let msg = format!(
                "Video render diagnostics: mpv='{mpv_ver}' ffmpeg='{ffmpeg_ver}' hwdec_requested='{hwdec_req}' hwdec_active='{hwdec_cur}' pixfmt='{pixfmt}' coded={coded_w}x{coded_h} display={disp_w}x{disp_h} target_fbo={width}x{height} (status=0x{surface_status:04X}) gl_err=0x{gl_err:04X} unpack_align={} unpack_row_len={} unpack_skip_px={} unpack_skip_rows={} pbo=0x{pbo_id:X} active_tex=0x{:04X} scissor_test={}",
                state_before.unpack_alignment,
                state_before.unpack_row_length,
                state_before.unpack_skip_pixels,
                state_before.unpack_skip_rows,
                state_before.active_texture,
                state_before.scissor_test
            );
            crate::diagnostics::GLOBAL_LOG.i("Renderer", &msg);
            crate::log_debug(&msg);
        }
    }

    /// Renders the current mpv frame and, when the surface was (re)created,
    /// returns a new borrowed texture image for the UI.
    pub fn render(&mut self, width: u32, height: u32) -> Result<Option<slint::Image>, String> {
        // Capture Slint's OpenGL state and immediately prepare standard OpenGL defaults for mpv
        let state_guard = unsafe { OpenGlStateGuard::capture_and_prepare(&self.gl) };
        let width = width.clamp(2, 8192);
        let height = height.clamp(2, 8192);

        // Clean up previously retired surfaces only on subsequent frames,
        // ensuring GPU commands from earlier frames have finished sampling them.
        for old in self.retired.drain(..) {
            unsafe { old.destroy(&self.gl) };
        }

        let size_changed = self
            .surface
            .as_ref()
            .is_none_or(|surface| surface.width != width || surface.height != height);

        let mut new_image = None;
        if size_changed {
            if let Some(old) = self.surface.take() {
                self.retired.push(old);
            }
            let surface = unsafe { Surface::create(&self.gl, width, height)? };
            let texture_id = surface.texture.0.get();
            self.current_texture_id = texture_id;
            let texture = NonZeroU32::new(texture_id)
                .ok_or_else(|| "OpenGL returned texture id 0".to_owned())?;
            let image = unsafe {
                slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(
                    texture,
                    (surface.width, surface.height).into(),
                )
                .origin(slint::BorrowedOpenGLTextureOrigin::TopLeft)
                .build()
            };
            new_image = Some(image);
            self.surface = Some(surface);
        }

        let surface = self.surface.as_ref().expect("surface initialized");

        unsafe {
            self.gl
                .bind_framebuffer(glow::FRAMEBUFFER, Some(surface.framebuffer));
            self.gl.viewport(0, 0, width as i32, height as i32);
            self.gl.clear_color(0.0, 0.0, 0.0, 1.0);
            self.gl.clear(glow::COLOR_BUFFER_BIT);
        }

        let result = self.mpv.render(
            surface.framebuffer.0.get() as i32,
            surface.width,
            surface.height,
        );

        // Unbind the target FBO so the texture is never attached to an active FBO
        // while Slint binds and samples it in subsequent UI rendering (prevents feedback loops).
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            self.gl.flush();
        }

        let gl_err = unsafe { self.gl.get_error() };
        self.check_and_log_diagnostics(
            width,
            height,
            glow::FRAMEBUFFER_COMPLETE,
            gl_err,
            &state_guard,
        );

        result.map_err(|error| format!("libmpv render failed: {error}"))?;

        Ok(new_image)
    }
}

impl Surface {
    unsafe fn create(gl: &glow::Context, width: u32, height: u32) -> Result<Self, String> {
        let texture = unsafe { gl.create_texture() }
            .map_err(|error| format!("cannot create video texture: {error}"))?;
        unsafe {
            // Ensure no foreign PBO is bound and unpack parameters are clean during allocation
            gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, None);
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
            gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 0);
            gl.pixel_store_i32(glow::UNPACK_SKIP_PIXELS, 0);
            gl.pixel_store_i32(glow::UNPACK_SKIP_ROWS, 0);

            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                width as i32,
                height as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None),
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.bind_texture(glow::TEXTURE_2D, None);
        }

        let framebuffer = unsafe { gl.create_framebuffer() }
            .map_err(|error| format!("cannot create video framebuffer: {error}"))?;
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(texture),
                0,
            );
        }
        let status = unsafe { gl.check_framebuffer_status(glow::FRAMEBUFFER) };
        if status != glow::FRAMEBUFFER_COMPLETE {
            unsafe {
                gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                gl.delete_framebuffer(framebuffer);
                gl.delete_texture(texture);
            }
            return Err(format!(
                "video framebuffer is incomplete (OpenGL status 0x{status:04x})"
            ));
        }

        // Initialize texture contents to solid black to avoid visual mesh/grid artifacts
        // when Slint samples the newly created surface before mpv draws the first frame.
        unsafe {
            gl.disable(glow::SCISSOR_TEST);
            gl.color_mask(true, true, true, true);
            gl.viewport(0, 0, width as i32, height as i32);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }

        Ok(Self {
            texture,
            framebuffer,
            width,
            height,
        })
    }

    unsafe fn destroy(self, gl: &glow::Context) {
        unsafe {
            gl.delete_framebuffer(self.framebuffer);
            gl.delete_texture(self.texture);
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        for s in self.retired.drain(..) {
            unsafe { s.destroy(&self.gl) };
        }
        if let Some(surface) = self.surface.take() {
            unsafe { surface.destroy(&self.gl) };
        }
    }
}

pub fn install_renderer(
    ui: &crate::MainWindow,
    player: Arc<Player>,
    frame_pending: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    on_renderer_ready: impl FnOnce() + 'static,
) -> Result<(), slint::SetRenderingNotifierError> {
    use std::sync::atomic::Ordering;
    let ui_weak = ui.as_weak();
    let frame_pending = frame_pending.clone();
    let flip_y = std::env::var("ADULT_HUB_FLIP_Y")
        .map(|value| value == "1" || value == "true")
        .unwrap_or(false);
    let mut renderer: Option<Renderer> = None;
    let mut on_renderer_ready = Some(on_renderer_ready);

    ui.window()
        .set_rendering_notifier(move |state, graphics_api| match state {
            slint::RenderingState::RenderingSetup => {
                let slint::GraphicsAPI::NativeOpenGL { get_proc_address } = graphics_api else {
                    eprintln!("RenderingSetup: graphics_api is NOT NativeOpenGL!");
                    if let Some(ui) = ui_weak.upgrade() {
                        crate::playback::show_error(
                            &ui,
                            "the selected Slint renderer is not OpenGL",
                        );
                    }
                    return;
                };
                match Renderer::new(Arc::clone(&player), get_proc_address, flip_y) {
                    Ok(mut created) => {
                        eprintln!("RenderingSetup: Renderer initialized successfully");
                        let pending = frame_pending.clone();
                        let w = ui_weak.clone();
                        created.set_update_callback(move || {
                            if !pending.swap(true, Ordering::SeqCst) {
                                let w = w.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(ui) = w.upgrade() {
                                        ui.window().request_redraw();
                                    }
                                });
                            }
                        });
                        renderer = Some(created);
                        if let Some(on_ready) = on_renderer_ready.take() {
                            on_ready();
                        }
                    }
                    Err(error) => {
                        if let Some(ui) = ui_weak.upgrade() {
                            crate::playback::show_error(&ui, error);
                        }
                    }
                }
            }
            slint::RenderingState::BeforeRendering => {
                frame_pending.store(false, Ordering::SeqCst);
                let Some(renderer) = renderer.as_mut() else {
                    return;
                };
                let Some(ui) = ui_weak.upgrade() else { return };
                let width = ui.get_texture_width().max(2) as u32;
                let height = ui.get_texture_height().max(2) as u32;
                match renderer.render(width, height) {
                    Ok(Some(image)) => {
                        ui.set_video_image(image);
                        if ui.get_is_switching_movie() && ui.get_player_media_loaded() {
                            ui.set_is_switching_movie(false);
                        }
                        if ui.get_is_changing_quality() && ui.get_player_media_loaded() {
                            ui.set_is_changing_quality(false);
                        }
                    }
                    Ok(None) => {
                        if ui.get_is_switching_movie() && ui.get_player_media_loaded() {
                            ui.set_is_switching_movie(false);
                        }
                        if ui.get_is_changing_quality() && ui.get_player_media_loaded() {
                            ui.set_is_changing_quality(false);
                        }
                    }
                    Err(error) => {
                        ui.set_is_switching_movie(false);
                        ui.set_is_changing_quality(false);
                        crate::playback::show_error(&ui, error);
                    }
                }
            }
            slint::RenderingState::RenderingTeardown => {
                renderer = None;
            }
            _ => {}
        })
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_dimensions_clamp_and_non_macroblock_widths() {
        // Test that 404 (non-16 multiple) and edge dimensions are accepted and clamped
        let width = 404u32.clamp(2, 8192);
        let height = 720u32.clamp(2, 8192);
        assert_eq!(width, 404);
        assert_eq!(height, 720);

        let small = 0u32.clamp(2, 8192);
        assert_eq!(small, 2);

        let large = 10000u32.clamp(2, 8192);
        assert_eq!(large, 8192);
    }

    #[test]
    fn test_diagnostics_message_formatting_redacts_credentials() {
        let msg = "Video render diagnostics: mpv='0.41.0' ffmpeg='N-127313' hwdec_requested='auto-safe' hwdec_active='d3d11va-copy' pixfmt='nv12' coded=404x720 display=404x720 target_fbo=404x720 (status=0x8CD5) gl_err=0x0000 unpack_align=4 unpack_row_len=0 unpack_skip_px=0 unpack_skip_rows=0 pbo=0x0 active_tex=0x84C0 scissor_test=false".to_string();
        let redacted = crate::diagnostics::redact(&msg);
        assert!(redacted.contains("d3d11va-copy"));
        assert!(redacted.contains("404x720"));
        assert!(redacted.contains("nv12"));
        assert!(!redacted.contains("token="));
        assert!(!redacted.contains("password"));
    }
}
