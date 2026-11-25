//! Minimal libmpv binding: the player core and its OpenGL rendering inside the
//! application window (no separate mpv window).

use std::ffi::{c_char, c_double, c_int, c_void, CStr, CString};
use std::path::Path;
use std::ptr;

use eframe::egui;
use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};

#[repr(C)]
struct MpvHandle {
    _private: [u8; 0],
}

#[repr(C)]
struct MpvRenderContext {
    _private: [u8; 0],
}

#[repr(C)]
struct MpvEvent {
    event_id: c_int,
    error: c_int,
    reply_userdata: u64,
    data: *mut c_void,
}

#[repr(C)]
struct MpvEventStartFile {
    playlist_entry_id: i64,
}

#[repr(C)]
struct MpvEventEndFile {
    reason: c_int,
    error: c_int,
    playlist_entry_id: i64,
}

#[repr(C)]
struct MpvEventProperty {
    name: *const c_char,
    format: c_int,
    data: *mut c_void,
}

#[repr(C)]
struct MpvRenderParam {
    kind: c_int,
    data: *mut c_void,
}

#[repr(C)]
struct MpvOpenGlInitParams {
    get_proc_address: extern "C" fn(ctx: *mut c_void, name: *const c_char) -> *mut c_void,
    get_proc_address_ctx: *mut c_void,
}

#[repr(C)]
struct MpvOpenGlFbo {
    fbo: c_int,
    w: c_int,
    h: c_int,
    internal_format: c_int,
}

const FORMAT_FLAG: c_int = 3;
const FORMAT_INT64: c_int = 4;
const FORMAT_DOUBLE: c_int = 5;

const EVENT_NONE: c_int = 0;
const EVENT_SHUTDOWN: c_int = 1;
const EVENT_START_FILE: c_int = 6;
const EVENT_END_FILE: c_int = 7;
const EVENT_FILE_LOADED: c_int = 8;
const EVENT_PROPERTY_CHANGE: c_int = 22;

const END_FILE_EOF: c_int = 0;
const END_FILE_ERROR: c_int = 4;

const RENDER_PARAM_API_TYPE: c_int = 1;
const RENDER_PARAM_OPENGL_INIT_PARAMS: c_int = 2;
const RENDER_PARAM_OPENGL_FBO: c_int = 3;
const RENDER_PARAM_FLIP_Y: c_int = 4;
const RENDER_PARAM_X11_DISPLAY: c_int = 8;
const RENDER_PARAM_WL_DISPLAY: c_int = 9;

#[link(name = "mpv")]
extern "C" {
    fn mpv_create() -> *mut MpvHandle;
    fn mpv_initialize(ctx: *mut MpvHandle) -> c_int;
    fn mpv_terminate_destroy(ctx: *mut MpvHandle);
    fn mpv_error_string(error: c_int) -> *const c_char;
    fn mpv_free(data: *mut c_void);
    fn mpv_set_option_string(ctx: *mut MpvHandle, name: *const c_char, data: *const c_char) -> c_int;
    fn mpv_set_property_string(ctx: *mut MpvHandle, name: *const c_char, data: *const c_char) -> c_int;
    fn mpv_get_property_string(ctx: *mut MpvHandle, name: *const c_char) -> *mut c_char;
    fn mpv_get_property(ctx: *mut MpvHandle, name: *const c_char, format: c_int, data: *mut c_void) -> c_int;
    fn mpv_observe_property(ctx: *mut MpvHandle, reply: u64, name: *const c_char, format: c_int) -> c_int;
    fn mpv_command(ctx: *mut MpvHandle, args: *mut *const c_char) -> c_int;
    fn mpv_wait_event(ctx: *mut MpvHandle, timeout: c_double) -> *mut MpvEvent;
    fn mpv_set_wakeup_callback(ctx: *mut MpvHandle, cb: extern "C" fn(*mut c_void), data: *mut c_void);

    fn mpv_render_context_create(
        res: *mut *mut MpvRenderContext,
        mpv: *mut MpvHandle,
        params: *mut MpvRenderParam,
    ) -> c_int;
    fn mpv_render_context_set_update_callback(
        ctx: *mut MpvRenderContext,
        cb: extern "C" fn(*mut c_void),
        data: *mut c_void,
    );
    fn mpv_render_context_render(ctx: *mut MpvRenderContext, params: *mut MpvRenderParam) -> c_int;
    fn mpv_render_context_free(ctx: *mut MpvRenderContext);
}

pub enum Event {
    StartFile {
        entry: i64,
    },
    FileLoaded,
    /// Natural end of the file.
    Finished {
        entry: i64,
    },
    Failed {
        entry: i64,
        message: String,
    },
    Fullscreen(bool),
    Shutdown,
}

pub struct Mpv {
    handle: *mut MpvHandle,
    render: *mut MpvRenderContext,
}

/// Render handle to pass to egui's paint callback.
#[derive(Clone, Copy)]
pub struct Renderer(*mut MpvRenderContext);

// The render context is only used from the thread that owns the GL context.
unsafe impl Send for Renderer {}
unsafe impl Sync for Renderer {}

type ProcAddress<'a> = &'a dyn Fn(&CStr) -> *const c_void;

extern "C" fn get_proc_address(ctx: *mut c_void, name: *const c_char) -> *mut c_void {
    let lookup = unsafe { &*(ctx as *const ProcAddress) };
    lookup(unsafe { CStr::from_ptr(name) }) as *mut c_void
}

extern "C" fn wake(data: *mut c_void) {
    let ctx = unsafe { &*(data as *const egui::Context) };
    ctx.request_repaint();
}

fn cstring(text: &str) -> CString {
    CString::new(text.replace('\0', "")).unwrap_or_default()
}

fn error_text(code: c_int) -> String {
    unsafe { CStr::from_ptr(mpv_error_string(code)) }.to_string_lossy().into_owned()
}

impl Mpv {
    /// Call with the window's OpenGL context current.
    /// `config_dir` holds the user's `mpv.conf`, `input.conf` and `scripts/`.
    pub fn new(cc: &eframe::CreationContext<'_>, config_dir: &Path) -> Result<Self, String> {
        let loader = cc.get_proc_address.clone().ok_or("OpenGL rendering is not available")?;
        let lookup: ProcAddress = &*loader;
        let handle = unsafe { mpv_create() };
        if handle.is_null() {
            return Err("could not create the mpv player".into());
        }
        let mut mpv = Self { handle, render: ptr::null_mut() };

        // Defaults; the user's mpv.conf, read at initialization, overrides them.
        let _ = std::fs::create_dir_all(config_dir);
        let options = [
            ("config-dir", config_dir.to_string_lossy().into_owned()),
            ("config", "yes".into()),
            ("vo", "libmpv".into()),
            ("hwdec", "auto-safe".into()),
            // jf draws its own playback interface.
            ("osc", "no".into()),
            ("osd-bar", "no".into()),
            ("input-default-bindings", "yes".into()),
            ("idle", "yes".into()),
            ("ytdl", "no".into()),
            ("audio-client-name", "jf".into()),
            ("user-agent", crate::USER_AGENT.into()),
        ];
        for (name, value) in options {
            unsafe { mpv_set_option_string(handle, cstring(name).as_ptr(), cstring(&value).as_ptr()) };
        }
        let code = unsafe { mpv_initialize(handle) };
        if code < 0 {
            return Err(format!("mpv initialization: {}", error_text(code)));
        }

        // mpv signals an event or a frame to show: wake the interface.
        let waker = Box::into_raw(Box::new(cc.egui_ctx.clone())) as *mut c_void;
        unsafe {
            mpv_set_wakeup_callback(handle, wake, waker);
            mpv_observe_property(handle, 0, cstring("fullscreen").as_ptr(), FORMAT_FLAG);
        }

        let api = cstring("opengl");
        let mut init = MpvOpenGlInitParams {
            get_proc_address,
            get_proc_address_ctx: &lookup as *const ProcAddress as *mut c_void,
        };
        let mut params = vec![
            MpvRenderParam { kind: RENDER_PARAM_API_TYPE, data: api.as_ptr() as *mut c_void },
            MpvRenderParam { kind: RENDER_PARAM_OPENGL_INIT_PARAMS, data: &mut init as *mut _ as *mut c_void },
        ];
        // Hardware decoding (VA-API) needs the native display.
        match cc.display_handle().map(|handle| handle.as_raw()) {
            Ok(RawDisplayHandle::Wayland(display)) => {
                params.push(MpvRenderParam { kind: RENDER_PARAM_WL_DISPLAY, data: display.display.as_ptr() })
            }
            Ok(RawDisplayHandle::Xlib(display)) => {
                if let Some(pointer) = display.display {
                    params.push(MpvRenderParam { kind: RENDER_PARAM_X11_DISPLAY, data: pointer.as_ptr() });
                }
            }
            _ => {}
        }
        params.push(MpvRenderParam { kind: 0, data: ptr::null_mut() });

        let mut render = ptr::null_mut();
        let code = unsafe { mpv_render_context_create(&mut render, handle, params.as_mut_ptr()) };
        if code < 0 {
            return Err(format!("mpv renderer: {}", error_text(code)));
        }
        unsafe { mpv_render_context_set_update_callback(render, wake, waker) };
        mpv.render = render;
        Ok(mpv)
    }

    pub fn renderer(&self) -> Renderer {
        Renderer(self.render)
    }

    /// Call while the OpenGL context still exists.
    pub fn free_renderer(&mut self) {
        if !self.render.is_null() {
            unsafe { mpv_render_context_free(self.render) };
            self.render = ptr::null_mut();
        }
    }

    pub fn command(&self, args: &[&str]) -> Result<(), String> {
        let owned: Vec<CString> = args.iter().map(|arg| cstring(arg)).collect();
        let mut pointers: Vec<*const c_char> = owned.iter().map(|arg| arg.as_ptr()).collect();
        pointers.push(ptr::null());
        let code = unsafe { mpv_command(self.handle, pointers.as_mut_ptr()) };
        if code < 0 {
            return Err(error_text(code));
        }
        Ok(())
    }

    pub fn set(&self, name: &str, value: &str) {
        unsafe { mpv_set_property_string(self.handle, cstring(name).as_ptr(), cstring(value).as_ptr()) };
    }

    pub fn get_string(&self, name: &str) -> Option<String> {
        let pointer = unsafe { mpv_get_property_string(self.handle, cstring(name).as_ptr()) };
        if pointer.is_null() {
            return None;
        }
        let text = unsafe { CStr::from_ptr(pointer) }.to_string_lossy().into_owned();
        unsafe { mpv_free(pointer as *mut c_void) };
        Some(text)
    }

    fn get<T: Default>(&self, name: &str, format: c_int) -> Option<T> {
        let mut value = T::default();
        let code = unsafe {
            mpv_get_property(self.handle, cstring(name).as_ptr(), format, &mut value as *mut T as *mut c_void)
        };
        (code >= 0).then_some(value)
    }

    pub fn get_f64(&self, name: &str) -> Option<f64> {
        self.get::<f64>(name, FORMAT_DOUBLE)
    }

    pub fn get_i64(&self, name: &str) -> Option<i64> {
        self.get::<i64>(name, FORMAT_INT64)
    }

    pub fn get_flag(&self, name: &str) -> Option<bool> {
        self.get::<c_int>(name, FORMAT_FLAG).map(|value| value != 0)
    }

    /// Next pending event, without blocking.
    pub fn poll_event(&self) -> Option<Event> {
        loop {
            let event = unsafe { &*mpv_wait_event(self.handle, 0.0) };
            return Some(match event.event_id {
                EVENT_NONE => return None,
                EVENT_SHUTDOWN => Event::Shutdown,
                EVENT_FILE_LOADED => Event::FileLoaded,
                EVENT_START_FILE => {
                    let data = unsafe { &*(event.data as *const MpvEventStartFile) };
                    Event::StartFile { entry: data.playlist_entry_id }
                }
                EVENT_END_FILE => {
                    let data = unsafe { &*(event.data as *const MpvEventEndFile) };
                    let entry = data.playlist_entry_id;
                    match data.reason {
                        END_FILE_EOF => Event::Finished { entry },
                        END_FILE_ERROR => Event::Failed { entry, message: error_text(data.error) },
                        _ => continue,
                    }
                }
                EVENT_PROPERTY_CHANGE => {
                    let data = unsafe { &*(event.data as *const MpvEventProperty) };
                    let name = unsafe { CStr::from_ptr(data.name) }.to_bytes();
                    if name == b"fullscreen" && data.format == FORMAT_FLAG && !data.data.is_null() {
                        Event::Fullscreen(unsafe { *(data.data as *const c_int) } != 0)
                    } else {
                        continue;
                    }
                }
                _ => continue,
            });
        }
    }
}

impl Renderer {
    /// Draws the current frame into the given framebuffer (size in pixels).
    pub fn render(&self, fbo: i32, width: i32, height: i32) {
        let mut target = MpvOpenGlFbo { fbo, w: width, h: height, internal_format: 0 };
        let mut flip: c_int = 1;
        let mut params = [
            MpvRenderParam { kind: RENDER_PARAM_OPENGL_FBO, data: &mut target as *mut _ as *mut c_void },
            MpvRenderParam { kind: RENDER_PARAM_FLIP_Y, data: &mut flip as *mut _ as *mut c_void },
            MpvRenderParam { kind: 0, data: ptr::null_mut() },
        ];
        unsafe { mpv_render_context_render(self.0, params.as_mut_ptr()) };
    }
}

impl Drop for Mpv {
    fn drop(&mut self) {
        self.free_renderer();
        unsafe { mpv_terminate_destroy(self.handle) };
    }
}
