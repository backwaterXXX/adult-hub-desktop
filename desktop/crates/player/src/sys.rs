//! Minimal libmpv C API declarations.
//!
//! Only the subset used by the desktop player is declared. Values are copied
//! from mpv 0.40 (`client.h`, `render.h`, `render_gl.h`); the client API
//! version is checked at runtime after loading the library.
#![allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code
)]

use std::ffi::{c_char, c_int, c_void};

pub enum mpv_handle {}
pub enum mpv_render_context {}

#[repr(C)]
pub struct mpv_event {
    pub event_id: c_int,
    pub error: c_int,
    pub reply_userdata: u64,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct mpv_event_end_file {
    pub reason: c_int,
    pub error: c_int,
    pub playlist_entry_id: i64,
    pub playlist_insert_id: i64,
    pub playlist_insert_num_entries: c_int,
}

#[repr(C)]
pub struct mpv_opengl_init_params {
    pub get_proc_address: Option<unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void>,
    pub get_proc_address_ctx: *mut c_void,
}

#[repr(C)]
pub struct mpv_opengl_fbo {
    pub fbo: c_int,
    pub w: c_int,
    pub h: c_int,
    pub internal_format: c_int,
}

#[repr(C)]
pub struct mpv_render_param {
    pub type_: c_int,
    pub data: *mut c_void,
}

pub const MPV_EVENT_NONE: c_int = 0;
pub const MPV_EVENT_SHUTDOWN: c_int = 1;
pub const MPV_EVENT_END_FILE: c_int = 7;
pub const MPV_EVENT_FILE_LOADED: c_int = 8;
pub const MPV_EVENT_VIDEO_RECONFIG: c_int = 17;
pub const MPV_EVENT_PLAYBACK_RESTART: c_int = 21;

pub const MPV_END_FILE_REASON_EOF: c_int = 0;
pub const MPV_END_FILE_REASON_STOP: c_int = 2;
pub const MPV_END_FILE_REASON_QUIT: c_int = 3;
pub const MPV_END_FILE_REASON_ERROR: c_int = 4;

pub const MPV_RENDER_PARAM_INVALID: c_int = 0;
pub const MPV_RENDER_PARAM_API_TYPE: c_int = 1;
pub const MPV_RENDER_PARAM_OPENGL_INIT_PARAMS: c_int = 2;
pub const MPV_RENDER_PARAM_OPENGL_FBO: c_int = 3;
pub const MPV_RENDER_PARAM_FLIP_Y: c_int = 4;

pub const MPV_RENDER_API_TYPE_OPENGL: &[u8] = b"opengl\0";

pub struct MpvApi {
    pub(crate) _library: libloading::Library,
    pub mpv_create: unsafe extern "C" fn() -> *mut mpv_handle,
    pub mpv_initialize: unsafe extern "C" fn(*mut mpv_handle) -> c_int,
    pub mpv_terminate_destroy: unsafe extern "C" fn(*mut mpv_handle),
    pub mpv_set_option_string:
        unsafe extern "C" fn(*mut mpv_handle, *const c_char, *const c_char) -> c_int,
    pub mpv_set_property_string:
        unsafe extern "C" fn(*mut mpv_handle, *const c_char, *const c_char) -> c_int,
    pub mpv_get_property_string:
        unsafe extern "C" fn(*mut mpv_handle, *const c_char) -> *mut c_char,
    pub mpv_free: unsafe extern "C" fn(*mut c_void),
    pub mpv_command: unsafe extern "C" fn(*mut mpv_handle, *const *const c_char) -> c_int,
    pub mpv_wait_event: unsafe extern "C" fn(*mut mpv_handle, f64) -> *mut mpv_event,
    pub mpv_error_string: unsafe extern "C" fn(c_int) -> *const c_char,
    pub mpv_client_api_version: unsafe extern "C" fn() -> u64,
    pub mpv_render_context_create: unsafe extern "C" fn(
        *mut *mut mpv_render_context,
        *mut mpv_handle,
        *mut mpv_render_param,
    ) -> c_int,
    pub mpv_render_context_set_update_callback: unsafe extern "C" fn(
        *mut mpv_render_context,
        Option<unsafe extern "C" fn(*mut c_void)>,
        *mut c_void,
    ),
    pub mpv_render_context_render:
        unsafe extern "C" fn(*mut mpv_render_context, *mut mpv_render_param) -> c_int,
    pub mpv_render_context_free: unsafe extern "C" fn(*mut mpv_render_context),
}
