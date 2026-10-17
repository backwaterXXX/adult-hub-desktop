//! OpenGL presentation for the libmpv render API.
//!
//! mpv renders into an offscreen RGBA texture owned by this module. Slint then
//! samples that texture directly (`BorrowedOpenGLTextureBuilder`), so no CPU
//! frame readback is involved.

use adult_hub_player::{Player, RenderContext};
use glow::HasContext;
use std::ffi::CStr;
use std::num::NonZeroU32;
use std::rc::Rc;

pub struct Renderer {
    gl: glow::Context,
    mpv: RenderContext,
    surface: Option<Surface>,
}

struct Surface {
    texture: glow::Texture,
    framebuffer: glow::Framebuffer,
    width: u32,
    height: u32,
}

impl Renderer {
    /// Must be called while Slint's OpenGL context is current.
    pub fn new(
        player: Rc<Player>,
        get_proc_address: &dyn Fn(&CStr) -> *const std::ffi::c_void,
        flip_y: bool,
    ) -> Result<Self, String> {
        let gl = unsafe { glow::Context::from_loader_function_cstr(|name| get_proc_address(name)) };
        let mpv = RenderContext::new(&player, get_proc_address, flip_y)
            .map_err(|error| format!("cannot create libmpv render context: {error}"))?;
        Ok(Self {
            gl,
            mpv,
            surface: None,
        })
    }

    pub fn set_update_callback(&mut self, callback: impl Fn() + Send + 'static) {
        self.mpv.set_update_callback(callback);
    }

    /// Renders the current mpv frame and, when the surface was (re)created,
    /// returns a new borrowed texture image for the UI.
    pub fn render(&mut self, width: u32, height: u32) -> Result<Option<slint::Image>, String> {
        let width = width.clamp(2, 8192);
        let height = height.clamp(2, 8192);

        let recreated = self
            .surface
            .as_ref()
            .is_none_or(|surface| surface.width != width || surface.height != height);

        if recreated {
            if let Some(old) = self.surface.take() {
                unsafe { old.destroy(&self.gl) };
            }
            self.surface = Some(unsafe { Surface::create(&self.gl, width, height)? });
        }

        let surface = self.surface.as_ref().expect("surface created above");

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

        unsafe { self.gl.bind_framebuffer(glow::FRAMEBUFFER, None) };

        result.map_err(|error| format!("libmpv render failed: {error}"))?;

        if recreated {
            let texture = NonZeroU32::new(surface.texture.0.get())
                .ok_or_else(|| "OpenGL returned texture id 0".to_owned())?;
            let image = unsafe {
                slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(
                    texture,
                    (surface.width, surface.height).into(),
                )
                .origin(slint::BorrowedOpenGLTextureOrigin::TopLeft)
                .build()
            };
            Ok(Some(image))
        } else {
            Ok(None)
        }
    }
}

impl Surface {
    unsafe fn create(gl: &glow::Context, width: u32, height: u32) -> Result<Self, String> {
        let texture = unsafe { gl.create_texture() }
            .map_err(|error| format!("cannot create video texture: {error}"))?;
        unsafe {
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
        unsafe { gl.bind_framebuffer(glow::FRAMEBUFFER, None) };
        if status != glow::FRAMEBUFFER_COMPLETE {
            unsafe {
                gl.delete_framebuffer(framebuffer);
                gl.delete_texture(texture);
            }
            return Err(format!(
                "video framebuffer is incomplete (OpenGL status 0x{status:04x})"
            ));
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
        if let Some(surface) = self.surface.take() {
            unsafe { surface.destroy(&self.gl) };
        }
    }
}
