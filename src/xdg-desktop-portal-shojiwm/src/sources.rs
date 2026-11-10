//! Source enumeration + one-shot thumbnail capture.
//!
//! A single Wayland session does double duty:
//!
//!   1. Bind `wl_output` and `ext_foreign_toplevel_list_v1` to discover what
//!      can be captured.
//!   2. For each discovered source, run a one-shot `ext-image-copy-capture-v1`
//!      cycle (output_image_capture_source or foreign_toplevel_image_capture_source
//!      → session → buffer constraints → frame → ready) and convert the
//!      captured SHM buffer into an `iced` image handle for the picker UI.
//!
//! Everything is synchronous — `enumerate` and thumbnail capture share one
//! Connection and one EventQueue, all driven by `roundtrip`. Cheap enough
//! for the picker (~100ms for a handful of windows) and dramatically simpler
//! than running a parallel async pipeline. Each `SelectSources` call gets a
//! fresh Wayland connection that's torn down when the function returns.

use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use iced::widget::image;
use rustix::mm::{MapFlags, ProtFlags};
use wayland_client::Proxy;
use wayland_client::protocol::{wl_buffer, wl_output, wl_registry, wl_shm, wl_shm_pool};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
    ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
};
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1,
    ext_image_capture_source_v1::ExtImageCaptureSourceV1,
    ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
    ext_image_copy_capture_manager_v1::{self, ExtImageCopyCaptureManagerV1},
    ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
};

const MAX_THUMBNAIL_DIM: u32 = 320;

pub mod source_types {
    pub const MONITOR: u32 = 1;
    pub const WINDOW: u32 = 2;
}

#[derive(Debug, Clone)]
pub struct OutputInfo {
    pub name: String,
    pub description: String,
    pub position: (i32, i32),
    /// Current mode dimensions in hardware (untransformed) orientation, as
    /// sent by `wl_output.mode`.
    pub width: i32,
    pub height: i32,
    pub refresh_mhz: i32,
    /// Output rotation/flip from `wl_output.geometry`. Determines the stream
    /// route: wlr-screencopy delivers rotated (scanout-orientation) frames,
    /// so transformed outputs stream through ext-image-copy-capture instead,
    /// which this compositor renders upright.
    pub transform: wl_output::Transform,
}

impl OutputInfo {
    /// Mode dimensions in the transformed (as-displayed) orientation.
    pub fn logical_mode_size(&self) -> (i32, i32) {
        match self.transform {
            wl_output::Transform::_90
            | wl_output::Transform::_270
            | wl_output::Transform::Flipped90
            | wl_output::Transform::Flipped270 => (self.height, self.width),
            _ => (self.width, self.height),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToplevelInfo {
    pub identifier: String,
    pub title: String,
    pub app_id: String,
}

#[derive(Debug, Clone)]
pub enum SourceKind {
    Output(OutputInfo),
    Toplevel(ToplevelInfo),
}

#[derive(Debug, Clone)]
pub struct SourceInfo {
    pub kind: SourceKind,
    pub thumbnail: Option<image::Handle>,
}

impl SourceInfo {
    /// Stable identifier used to route per-source updates from the
    /// background thumbnail refresh thread back to the picker UI.
    pub fn id(&self) -> &str {
        match &self.kind {
            SourceKind::Output(o) => &o.name,
            SourceKind::Toplevel(t) => &t.identifier,
        }
    }

    pub fn label(&self) -> String {
        match &self.kind {
            SourceKind::Output(o) => {
                if o.description.is_empty() {
                    o.name.clone()
                } else {
                    format!("{} — {}", o.name, o.description)
                }
            }
            SourceKind::Toplevel(t) => {
                if t.app_id.is_empty() {
                    t.title.clone()
                } else if t.title.is_empty() {
                    t.app_id.clone()
                } else {
                    format!("{} — {}", t.app_id, t.title)
                }
            }
        }
    }

    pub fn detail(&self) -> String {
        match &self.kind {
            SourceKind::Output(o) => format!(
                "{}×{} @ {:.2}Hz · ({}, {})",
                o.width,
                o.height,
                o.refresh_mhz as f64 / 1000.0,
                o.position.0,
                o.position.1,
            ),
            SourceKind::Toplevel(t) => {
                if t.identifier.is_empty() {
                    crate::i18n::t("source.window")
                } else {
                    let short = &t.identifier[..t.identifier.len().min(12)];
                    crate::i18n::t_args("source.window_with_id", &[("id", short)])
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------

struct DiscoveredOutput {
    global_name: u32,
    proxy: wl_output::WlOutput,
    info: OutputInfo,
    pending: OutputInfo,
}

struct DiscoveredToplevel {
    proxy: ExtForeignToplevelHandleV1,
    info: ToplevelInfo,
}

#[derive(Default)]
struct DiscoveryState {
    outputs: Vec<DiscoveredOutput>,
    toplevels: Vec<DiscoveredToplevel>,
    toplevel_index: std::collections::HashMap<u32, usize>,

    shm: Option<wl_shm::WlShm>,
    capture_manager: Option<ExtImageCopyCaptureManagerV1>,
    output_source_manager: Option<ExtOutputImageCaptureSourceManagerV1>,
    toplevel_source_manager: Option<ExtForeignToplevelImageCaptureSourceManagerV1>,
}

struct AppData {
    state: Arc<Mutex<DiscoveryState>>,
    capture: Arc<Mutex<CaptureState>>,
}

#[derive(Default)]
struct CaptureState {
    /// The size + format the compositor wants us to allocate for the *current*
    /// session, filled in as buffer_size / shm_format events arrive.
    constraint_width: u32,
    constraint_height: u32,
    constraint_format: Option<wl_shm::Format>,
    constraints_done: bool,

    /// Set when the in-flight frame resolves.
    frame_ready: bool,
    frame_failed: bool,
}

impl Dispatch<wl_registry::WlRegistry, ()> for AppData {
    fn event(
        _app: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::GlobalRemove { name } = event {
            _app.state.lock().unwrap().outputs.retain(|output| {
                if output.global_name != name {
                    return true;
                }
                if output.proxy.version() >= 3 {
                    output.proxy.release();
                }
                false
            });
            return;
        }
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        match interface.as_str() {
            "wl_output" => {
                let proxy =
                    registry.bind::<wl_output::WlOutput, _, _>(name, version.min(4), qh, ());
                let info = OutputInfo {
                    name: String::new(),
                    description: String::new(),
                    position: (0, 0),
                    width: 0,
                    height: 0,
                    refresh_mhz: 0,
                    transform: wl_output::Transform::Normal,
                };
                _app.state.lock().unwrap().outputs.push(DiscoveredOutput {
                    global_name: name,
                    proxy,
                    pending: info.clone(),
                    info,
                });
            }
            "wl_shm" => {
                let shm = registry.bind::<wl_shm::WlShm, _, _>(name, version.min(1), qh, ());
                _app.state.lock().unwrap().shm = Some(shm);
            }
            "ext_foreign_toplevel_list_v1" => {
                registry.bind::<ExtForeignToplevelListV1, _, _>(name, version.min(1), qh, ());
            }
            "ext_image_copy_capture_manager_v1" => {
                let m = registry.bind::<ExtImageCopyCaptureManagerV1, _, _>(
                    name,
                    version.min(1),
                    qh,
                    (),
                );
                _app.state.lock().unwrap().capture_manager = Some(m);
            }
            "ext_output_image_capture_source_manager_v1" => {
                let m = registry.bind::<ExtOutputImageCaptureSourceManagerV1, _, _>(
                    name,
                    version.min(1),
                    qh,
                    (),
                );
                _app.state.lock().unwrap().output_source_manager = Some(m);
            }
            "ext_foreign_toplevel_image_capture_source_manager_v1" => {
                let m = registry.bind::<ExtForeignToplevelImageCaptureSourceManagerV1, _, _>(
                    name,
                    version.min(1),
                    qh,
                    (),
                );
                _app.state.lock().unwrap().toplevel_source_manager = Some(m);
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for AppData {
    fn event(
        app: &mut Self,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let mut state = app.state.lock().unwrap();
        let Some(entry) = state
            .outputs
            .iter_mut()
            .find(|entry| &entry.proxy == output)
        else {
            return;
        };
        let cur = &mut entry.pending;
        match event {
            wl_output::Event::Name { name } => cur.name = name,
            wl_output::Event::Description { description } => cur.description = description,
            wl_output::Event::Geometry {
                x, y, transform, ..
            } => {
                cur.position = (x, y);
                if let WEnum::Value(transform) = transform {
                    cur.transform = transform;
                }
            }
            wl_output::Event::Mode {
                flags,
                width,
                height,
                refresh,
            } if flags
                .into_result()
                .map(|f| f.contains(wl_output::Mode::Current))
                .unwrap_or(false) =>
            {
                cur.width = width;
                cur.height = height;
                cur.refresh_mhz = refresh;
            }
            wl_output::Event::Done => {
                entry.info = entry.pending.clone();
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtForeignToplevelListV1, ()> for AppData {
    fn event(
        _app: &mut Self,
        _: &ExtForeignToplevelListV1,
        _event: ext_foreign_toplevel_list_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }

    wayland_client::event_created_child!(AppData, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for AppData {
    fn event(
        app: &mut Self,
        proxy: &ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let id = proxy.id().protocol_id();
        let mut state = app.state.lock().unwrap();
        let idx = if let Some(&existing) = state.toplevel_index.get(&id) {
            existing
        } else {
            state.toplevels.push(DiscoveredToplevel {
                proxy: proxy.clone(),
                info: ToplevelInfo {
                    identifier: String::new(),
                    title: String::new(),
                    app_id: String::new(),
                },
            });
            let new_idx = state.toplevels.len() - 1;
            state.toplevel_index.insert(id, new_idx);
            new_idx
        };
        match event {
            ext_foreign_toplevel_handle_v1::Event::Title { title } => {
                state.toplevels[idx].info.title = title;
            }
            ext_foreign_toplevel_handle_v1::Event::AppId { app_id } => {
                state.toplevels[idx].info.app_id = app_id;
            }
            ext_foreign_toplevel_handle_v1::Event::Identifier { identifier } => {
                state.toplevels[idx].info.identifier = identifier;
            }
            ext_foreign_toplevel_handle_v1::Event::Closed => {
                if let Some(remove_idx) = state.toplevel_index.remove(&id) {
                    state.toplevels.remove(remove_idx);
                    for entry in state.toplevel_index.values_mut() {
                        if *entry > remove_idx {
                            *entry -= 1;
                        }
                    }
                }
                proxy.destroy();
            }
            _ => {}
        }
    }
}

// Empty/no-op dispatch for everything else we touch.
macro_rules! empty_dispatch {
    ($t:ty) => {
        impl Dispatch<$t, ()> for AppData {
            fn event(
                _: &mut Self,
                _: &$t,
                _: <$t as Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    };
}
empty_dispatch!(wl_shm::WlShm);
empty_dispatch!(wl_shm_pool::WlShmPool);
empty_dispatch!(wl_buffer::WlBuffer);
empty_dispatch!(ExtImageCopyCaptureManagerV1);
empty_dispatch!(ExtOutputImageCaptureSourceManagerV1);
empty_dispatch!(ExtForeignToplevelImageCaptureSourceManagerV1);
empty_dispatch!(ExtImageCaptureSourceV1);

impl Dispatch<ExtImageCopyCaptureSessionV1, ()> for AppData {
    fn event(
        app: &mut Self,
        _: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let mut c = app.capture.lock().unwrap();
        match event {
            ext_image_copy_capture_session_v1::Event::BufferSize { width, height } => {
                c.constraint_width = width;
                c.constraint_height = height;
            }
            ext_image_copy_capture_session_v1::Event::ShmFormat {
                format: WEnum::Value(f),
            } => {
                // Prefer Xrgb8888 / Argb8888 over more exotic ones.
                let is_preferred = matches!(f, wl_shm::Format::Xrgb8888 | wl_shm::Format::Argb8888);
                if c.constraint_format.is_none() || is_preferred {
                    c.constraint_format = Some(f);
                }
            }
            ext_image_copy_capture_session_v1::Event::Done => {
                c.constraints_done = true;
            }
            ext_image_copy_capture_session_v1::Event::Stopped => {
                c.frame_failed = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, ()> for AppData {
    fn event(
        app: &mut Self,
        _: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let mut c = app.capture.lock().unwrap();
        match event {
            ext_image_copy_capture_frame_v1::Event::Ready => c.frame_ready = true,
            ext_image_copy_capture_frame_v1::Event::Failed { .. } => c.frame_failed = true,
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------

fn capture_output_thumbnail(
    event_queue: &mut EventQueue<AppData>,
    app: &mut AppData,
    qh: &QueueHandle<AppData>,
    output: &wl_output::WlOutput,
) -> Option<image::Handle> {
    let (manager, output_mgr) = {
        let s = app.state.lock().unwrap();
        (s.capture_manager.clone()?, s.output_source_manager.clone()?)
    };
    let source = ProtocolGuard::new(
        output_mgr.create_source(output, qh, ()),
        ExtImageCaptureSourceV1::destroy,
    );
    capture_via_source(event_queue, app, qh, &manager, &source)
}

fn capture_toplevel_thumbnail(
    event_queue: &mut EventQueue<AppData>,
    app: &mut AppData,
    qh: &QueueHandle<AppData>,
    toplevel: &ExtForeignToplevelHandleV1,
) -> Option<image::Handle> {
    let (manager, toplevel_mgr) = {
        let s = app.state.lock().unwrap();
        (
            s.capture_manager.clone()?,
            s.toplevel_source_manager.clone()?,
        )
    };
    let source = ProtocolGuard::new(
        toplevel_mgr.create_source(toplevel, qh, ()),
        ExtImageCaptureSourceV1::destroy,
    );
    capture_via_source(event_queue, app, qh, &manager, &source)
}

/// Run one capture cycle against a freshly-created `ExtImageCaptureSourceV1`.
/// Returns the decoded thumbnail on success, `None` on any failure path.
fn capture_via_source(
    event_queue: &mut EventQueue<AppData>,
    app: &mut AppData,
    qh: &QueueHandle<AppData>,
    manager: &ExtImageCopyCaptureManagerV1,
    source: &ExtImageCaptureSourceV1,
) -> Option<image::Handle> {
    // Reset per-capture state.
    {
        let mut c = app.capture.lock().unwrap();
        *c = CaptureState::default();
    }

    let session = ProtocolGuard::new(
        manager.create_session(
            source,
            ext_image_copy_capture_manager_v1::Options::empty(),
            qh,
            (),
        ),
        ExtImageCopyCaptureSessionV1::destroy,
    );

    // Drain buffer_size / shm_format / done.
    for _ in 0..8 {
        if event_queue.roundtrip(app).is_err() {
            return None;
        }
        if app.capture.lock().unwrap().constraints_done {
            break;
        }
    }
    let (width, height, format) = {
        let c = app.capture.lock().unwrap();
        if !c.constraints_done {
            tracing::debug!("thumbnail: constraints never finalized");
            return None;
        }
        (c.constraint_width, c.constraint_height, c.constraint_format)
    };
    let format = format.unwrap_or(wl_shm::Format::Xrgb8888);
    let layout = shm_layout(width, height)?;
    let buffer = allocate_shm_buffer(app, qh, layout, format)?;

    let frame = ProtocolGuard::new(
        session.create_frame(qh, ()),
        ExtImageCopyCaptureFrameV1::destroy,
    );
    frame.attach_buffer(&buffer.buffer);
    frame.capture();

    // Drain until ready/failed (or we give up).
    let mut tries = 0;
    let outcome = loop {
        if event_queue.roundtrip(app).is_err() {
            break false;
        }
        let c = app.capture.lock().unwrap();
        if c.frame_ready {
            break true;
        }
        if c.frame_failed {
            break false;
        }
        tries += 1;
        if tries > 12 {
            break false;
        }
    };

    // Guards release the frame, buffer, mapping, session and source on every path.
    if outcome {
        unsafe {
            decode_buffer(
                buffer.mapping.ptr,
                buffer.mapping.size,
                width,
                height,
                format,
            )
        }
    } else {
        None
    }
}

/// Wayland proxies do not send their destructor when their Rust handle drops.
struct ProtocolGuard<T> {
    proxy: T,
    destroy: fn(&T),
}

impl<T> ProtocolGuard<T> {
    fn new(proxy: T, destroy: fn(&T)) -> Self {
        Self { proxy, destroy }
    }
}

impl<T> std::ops::Deref for ProtocolGuard<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.proxy
    }
}

impl<T> Drop for ProtocolGuard<T> {
    fn drop(&mut self) {
        (self.destroy)(&self.proxy);
    }
}

/// Own the mapping independently of the fd, including allocation-error paths.
struct ShmMapping {
    ptr: NonNull<u8>,
    size: usize,
    fd: OwnedFd,
}

impl ShmMapping {
    fn new(size: usize) -> Option<Self> {
        if size == 0 || size > i32::MAX as usize {
            return None;
        }
        let fd =
            rustix::fs::memfd_create("shojiwm-portal-thumbnail", rustix::fs::MemfdFlags::CLOEXEC)
                .ok()?;
        rustix::fs::ftruncate(&fd, size as u64).ok()?;
        let raw = unsafe {
            rustix::mm::mmap(
                std::ptr::null_mut(),
                size,
                ProtFlags::READ,
                MapFlags::SHARED,
                fd.as_fd(),
                0,
            )
        }
        .ok()?;
        let Some(ptr) = NonNull::new(raw.cast::<u8>()) else {
            unsafe {
                if let Err(error) = rustix::mm::munmap(raw, size) {
                    tracing::warn!(%error, "failed to unmap thumbnail buffer");
                }
            }
            return None;
        };
        Some(Self { ptr, size, fd })
    }
}

impl Drop for ShmMapping {
    fn drop(&mut self) {
        if let Err(error) = unsafe { rustix::mm::munmap(self.ptr.as_ptr().cast(), self.size) } {
            tracing::warn!(%error, "failed to unmap thumbnail buffer");
        }
    }
}

// Fields drop in declaration order: release the protocol objects before the mapping.
struct ShmBuffer {
    buffer: ProtocolGuard<wl_buffer::WlBuffer>,
    _pool: ProtocolGuard<wl_shm_pool::WlShmPool>,
    mapping: ShmMapping,
}

#[derive(Debug, PartialEq, Eq)]
struct ShmLayout {
    width: i32,
    height: i32,
    stride: i32,
    size: usize,
}

/// Validate every size once, before allocating or passing signed dimensions to Wayland.
fn shm_layout(width: u32, height: u32) -> Option<ShmLayout> {
    if width == 0 || height == 0 {
        return None;
    }
    let stride = i32::try_from(width.checked_mul(4)?).ok()?;
    let width = i32::try_from(width).ok()?;
    let height = i32::try_from(height).ok()?;
    let size = usize::try_from(stride)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?;
    i32::try_from(size).ok()?;
    Some(ShmLayout {
        width,
        height,
        stride,
        size,
    })
}

fn allocate_shm_buffer(
    app: &mut AppData,
    qh: &QueueHandle<AppData>,
    layout: ShmLayout,
    format: wl_shm::Format,
) -> Option<ShmBuffer> {
    let shm = app.state.lock().unwrap().shm.clone()?;
    let mapping = ShmMapping::new(layout.size)?;
    let pool = ProtocolGuard::new(
        shm.create_pool(mapping.fd.as_fd(), layout.size as i32, qh, ()),
        wl_shm_pool::WlShmPool::destroy,
    );
    let buffer = ProtocolGuard::new(
        pool.create_buffer(
            0,
            layout.width,
            layout.height,
            layout.stride,
            format,
            qh,
            (),
        ),
        wl_buffer::WlBuffer::destroy,
    );
    Some(ShmBuffer {
        buffer,
        _pool: pool,
        mapping,
    })
}

/// Convert the BGRx / ARGB SHM buffer into an iced RGBA image, downscaled to
/// at most `MAX_THUMBNAIL_DIM` on the longest edge to keep widget memory
/// bounded.
unsafe fn decode_buffer(
    ptr: NonNull<u8>,
    size: usize,
    width: u32,
    height: u32,
    format: wl_shm::Format,
) -> Option<image::Handle> {
    let bytes: &[u8] = unsafe { std::slice::from_raw_parts(ptr.as_ptr(), size) };

    // Compute downscale factor (integer nearest-neighbour).
    let longest = width.max(height);
    let factor = longest.div_ceil(MAX_THUMBNAIL_DIM).max(1);
    let dst_w = (width / factor).max(1);
    let dst_h = (height / factor).max(1);

    let mut rgba = vec![0u8; (dst_w * dst_h * 4) as usize];
    let stride = (width * 4) as usize;
    for y in 0..dst_h {
        let src_y = (y * factor) as usize;
        for x in 0..dst_w {
            let src_x = (x * factor) as usize;
            let i = src_y * stride + src_x * 4;
            if i + 3 >= bytes.len() {
                continue;
            }
            // SHM byte order for Xrgb8888 / Argb8888 is B, G, R, X|A.
            let (r, g, b) = match format {
                wl_shm::Format::Xrgb8888 | wl_shm::Format::Argb8888 => {
                    (bytes[i + 2], bytes[i + 1], bytes[i])
                }
                wl_shm::Format::Xbgr8888 | wl_shm::Format::Abgr8888 => {
                    (bytes[i], bytes[i + 1], bytes[i + 2])
                }
                _ => (bytes[i + 2], bytes[i + 1], bytes[i]),
            };
            let o = ((y * dst_w + x) * 4) as usize;
            rgba[o] = r;
            rgba[o + 1] = g;
            rgba[o + 2] = b;
            rgba[o + 3] = 0xff;
        }
    }
    Some(image::Handle::from_rgba(dst_w, dst_h, rgba))
}

// ---------------------------------------------------------------------------
// Live refresh ("Lv.2")
// ---------------------------------------------------------------------------

/// One SelectSources attempt, distinct from its reusable portal session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SelectionRequestId(u64);

impl SelectionRequestId {
    pub fn unique() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// A current source list, including the latest available thumbnails.
#[derive(Debug, Clone)]
pub struct SourceUpdate {
    pub session_key: String,
    pub request_id: SelectionRequestId,
    pub sources: Vec<SourceInfo>,
}

/// Shared cancellation state. Registering the socket and observing cancellation
/// use the same lock so cancellation cannot miss a newly registered connection.
#[derive(Default)]
struct ThumbnailCancellation {
    stop: AtomicBool,
    socket: Mutex<Option<UnixStream>>,
}

impl ThumbnailCancellation {
    fn is_stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    fn cancel(&self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(socket) = self.socket.lock().unwrap().as_ref() {
            // Closing just the duplicate would not interrupt the worker's fd.
            let _ = socket.shutdown(std::net::Shutdown::Both);
        }
    }

    fn register(&self, conn: &Connection) -> std::io::Result<Option<SocketRegistration<'_>>> {
        let duplicate = UnixStream::from(conn.as_fd().try_clone_to_owned()?);
        let mut socket = self.socket.lock().unwrap();
        if self.is_stopped() {
            // Cancellation may have run before registration acquired the lock.
            // Do not start a roundtrip on a connection that it could not interrupt.
            return Ok(None);
        }
        *socket = Some(duplicate);
        Ok(Some(SocketRegistration { cancellation: self }))
    }
}

/// Clear the duplicate on every worker exit, including connection errors.
struct SocketRegistration<'a> {
    cancellation: &'a ThumbnailCancellation,
}

impl Drop for SocketRegistration<'_> {
    fn drop(&mut self) {
        self.cancellation.socket.lock().unwrap().take();
    }
}

/// Handle to the background thumbnail-refresh thread. Drop it to stop.
pub struct ThumbnailStream {
    cancellation: Arc<ThumbnailCancellation>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Drop for ThumbnailStream {
    fn drop(&mut self) {
        // Interrupt a blocked Wayland roundtrip before waiting for the worker.
        self.cancellation.cancel();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Spawn a background thread that:
///   1. enumerates all sources + captures their first thumbnail,
///   2. sends the initial `Vec<SourceInfo>` through `init_tx`,
///   3. then round-robins through the sources, pushing current source lists on
///      `updates_tx` until the returned `ThumbnailStream` is dropped.
///
/// One wayland Connection lives on the thread for its whole lifetime —
/// per-source sessions are created and destroyed per refresh because
/// `ext-image-copy-capture-v1` sessions are themselves not long-lived
/// streaming primitives; they're billed per frame.
pub fn start_thumbnail_stream(
    session_key: String,
    request_id: SelectionRequestId,
    requested: u32,
    init_tx: tokio::sync::oneshot::Sender<Vec<SourceInfo>>,
    updates_tx: tokio::sync::mpsc::Sender<SourceUpdate>,
) -> ThumbnailStream {
    let cancellation = Arc::new(ThumbnailCancellation::default());
    let cancellation_for_thread = cancellation.clone();
    let join = std::thread::Builder::new()
        .name("portal-thumbnails".into())
        .spawn(move || {
            if let Err(e) = run_stream(
                session_key,
                request_id,
                requested,
                init_tx,
                updates_tx,
                &cancellation_for_thread,
            ) && !cancellation_for_thread.is_stopped()
            {
                tracing::warn!("thumbnail stream exited: {e}");
            }
        })
        .expect("spawn thumbnail thread");
    ThumbnailStream {
        cancellation,
        join: Some(join),
    }
}

fn run_stream(
    session_key: String,
    request_id: SelectionRequestId,
    requested: u32,
    init_tx: tokio::sync::oneshot::Sender<Vec<SourceInfo>>,
    updates_tx: tokio::sync::mpsc::Sender<SourceUpdate>,
    cancellation: &ThumbnailCancellation,
) -> Result<(), Box<dyn std::error::Error>> {
    if cancellation.is_stopped() {
        return Ok(());
    }
    // connect_to_env itself is not interruptible until its socket is registered.
    let conn = Connection::connect_to_env()?;
    let Some(_registration) = cancellation.register(&conn)? else {
        return Ok(());
    };
    let display = conn.display();
    let mut event_queue: EventQueue<AppData> = conn.new_event_queue();
    let qh = event_queue.handle();
    let _registry = display.get_registry(&qh, ());

    let state = Arc::new(Mutex::new(DiscoveryState::default()));
    let capture = Arc::new(Mutex::new(CaptureState::default()));
    let mut app = AppData {
        state: state.clone(),
        capture: capture.clone(),
    };
    for _ in 0..3 {
        event_queue.roundtrip(&mut app)?;
    }

    // Rebuild this list after dispatching events so open/close, title changes
    // and output hotplug reach the picker while it is open.
    enum LiveSource {
        Output {
            proxy: wl_output::WlOutput,
            info: SourceInfo,
        },
        Toplevel {
            proxy: ExtForeignToplevelHandleV1,
            info: SourceInfo,
        },
    }
    impl LiveSource {
        fn info(&self) -> &SourceInfo {
            match self {
                Self::Output { info, .. } | Self::Toplevel { info, .. } => info,
            }
        }
    }
    let discover = |thumbnails: &std::collections::HashMap<String, image::Handle>| {
        let s = state.lock().unwrap();
        let mut v: Vec<LiveSource> = Vec::new();
        if requested & source_types::MONITOR != 0 {
            let mut outputs: Vec<_> = s
                .outputs
                .iter()
                .filter(|o| !o.info.name.is_empty() && o.info.width > 0 && o.info.height > 0)
                .collect();
            outputs.sort_by(|a, b| {
                (a.info.position, &a.info.name).cmp(&(b.info.position, &b.info.name))
            });
            for o in outputs {
                v.push(LiveSource::Output {
                    proxy: o.proxy.clone(),
                    info: SourceInfo {
                        kind: SourceKind::Output(o.info.clone()),
                        thumbnail: thumbnails.get(&o.info.name).cloned(),
                    },
                });
            }
        }
        if requested & source_types::WINDOW != 0 {
            for t in &s.toplevels {
                if t.info.identifier.is_empty() {
                    continue;
                }
                v.push(LiveSource::Toplevel {
                    proxy: t.proxy.clone(),
                    info: SourceInfo {
                        kind: SourceKind::Toplevel(t.info.clone()),
                        thumbnail: thumbnails.get(&t.info.identifier).cloned(),
                    },
                });
            }
        }
        v
    };

    let mut thumbnails = std::collections::HashMap::new();
    let initial = discover(&thumbnails)
        .iter()
        .map(|src| src.info().clone())
        .collect();
    if init_tx.send(initial).is_err() {
        return Ok(());
    }

    // Round-robin refresh. Short sleep between captures keeps CPU bounded
    // and avoids starving the compositor / cursor when many windows are
    // present. Per-source rate ≈ 1 / (N_sources × (per_capture + tick))
    // — typical numbers: 1 source ~10fps, 5 sources ~2-3fps. Adjust if the
    // picker feels sluggish vs. too hot.
    let tick = std::time::Duration::from_millis(50);
    let mut cursor = 0usize;
    while !cancellation.is_stopped() {
        event_queue.roundtrip(&mut app)?;
        let live_sources = discover(&thumbnails);
        if !live_sources.is_empty() {
            let src = &live_sources[cursor % live_sources.len()];
            cursor = cursor.wrapping_add(1);
            let handle = match src {
                LiveSource::Output { proxy, .. } => {
                    capture_output_thumbnail(&mut event_queue, &mut app, &qh, proxy)
                }
                LiveSource::Toplevel { proxy, .. } => {
                    capture_toplevel_thumbnail(&mut event_queue, &mut app, &qh, proxy)
                }
            };
            if let Some(handle) = handle {
                thumbnails.insert(src.info().id().to_owned(), handle);
            }
        }
        let sources: Vec<_> = discover(&thumbnails)
            .iter()
            .map(|src| src.info().clone())
            .collect();
        thumbnails.retain(|id, _| sources.iter().any(|src| src.id() == id));
        match updates_tx.try_send(SourceUpdate {
            session_key: session_key.clone(),
            request_id,
            sources,
        }) {
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => break,
            // Keep only a bounded number of snapshots if the UI is busy.
            Ok(()) | Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {}
        }
        std::thread::sleep(tick);
    }
    Ok(())
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    const WATCHDOG: Duration = Duration::from_secs(2);

    #[derive(Clone, Copy)]
    struct FileIdentity {
        device: u64,
        inode: u64,
    }

    fn identity(fd: &impl AsFd) -> FileIdentity {
        use std::os::fd::AsRawFd;
        let metadata =
            std::fs::metadata(format!("/proc/self/fd/{}", fd.as_fd().as_raw_fd())).unwrap();
        FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }

    // Check the underlying file, not just a raw fd number that another test may reuse.
    fn open_fds(file: FileIdentity) -> usize {
        std::fs::read_dir("/proc/self/fd")
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|entry| std::fs::metadata(entry.path()).ok())
            .filter(|metadata| metadata.dev() == file.device && metadata.ino() == file.inode)
            .count()
    }

    fn has_mapping(file: FileIdentity) -> bool {
        std::fs::read_to_string("/proc/self/maps")
            .unwrap()
            .lines()
            .any(|line| {
                line.split_whitespace()
                    .nth(4)
                    .and_then(|value| value.parse::<u64>().ok())
                    == Some(file.inode)
                    && line.contains("memfd:shojiwm-portal-thumbnail")
            })
    }

    fn wait_until(mut condition: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + WATCHDOG;
        loop {
            if condition() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    // If cancellation regresses, release the fake peer so this test fails instead
    // of leaving the test runner stuck forever in ThumbnailStream::drop's join.
    fn finish_drop(
        dropper: std::thread::JoinHandle<()>,
        done: mpsc::Receiver<()>,
        peer: &UnixStream,
    ) -> bool {
        let timely = done.recv_timeout(WATCHDOG).is_ok();
        let finished = timely || {
            let _ = peer.shutdown(std::net::Shutdown::Both);
            done.recv_timeout(WATCHDOG).is_ok()
        };
        if finished {
            dropper.join().unwrap();
        }
        timely
    }

    #[test]
    fn protocol_guards_destroy_once_on_success_and_each_early_error() {
        type Resource = (&'static str, Arc<Mutex<Vec<&'static str>>>);
        fn destroy(resource: &Resource) {
            resource.1.lock().unwrap().push(resource.0);
        }
        fn capture_like(fail_after: usize, log: Arc<Mutex<Vec<&'static str>>>) -> Result<(), ()> {
            let check = |step| if fail_after == step { Err(()) } else { Ok(()) };
            let _source = ProtocolGuard::new(("source", log.clone()), destroy);
            check(1)?;
            let _session = ProtocolGuard::new(("session", log.clone()), destroy);
            check(2)?;
            let _pool = ProtocolGuard::new(("pool", log.clone()), destroy);
            check(3)?;
            let _buffer = ProtocolGuard::new(("buffer", log.clone()), destroy);
            check(4)?;
            let _frame = ProtocolGuard::new(("frame", log), destroy);
            check(5)?;
            Ok(())
        }

        let resources = ["source", "session", "pool", "buffer", "frame"];
        for fail_after in 0..=resources.len() {
            let log = Arc::new(Mutex::new(Vec::new()));
            assert_eq!(
                capture_like(fail_after, log.clone()).is_ok(),
                fail_after == 0
            );
            let count = if fail_after == 0 {
                resources.len()
            } else {
                fail_after
            };
            let expected: Vec<_> = resources[..count].iter().copied().rev().collect();
            assert_eq!(*log.lock().unwrap(), expected);
        }
    }

    #[test]
    fn shm_layout_checks_all_protocol_dimensions_and_pool_size() {
        assert_eq!(
            shm_layout(320, 180),
            Some(ShmLayout {
                width: 320,
                height: 180,
                stride: 1280,
                size: 230400,
            })
        );
        // Largest valid stride and largest valid height for a one-pixel row.
        let limit = i32::MAX as u32 / 4;
        assert_eq!(shm_layout(limit, 1).unwrap().size, limit as usize * 4);
        assert_eq!(shm_layout(1, limit).unwrap().size, limit as usize * 4);
        for (width, height) in [
            (0, 100),
            (100, 0),
            (u32::MAX, 1),
            (1, u32::MAX),
            (limit + 1, 1),
            (1, limit + 1),
            (1, i32::MAX as u32 + 1),
            (1_000_000, 1_000_000),
        ] {
            assert!(
                shm_layout(width, height).is_none(),
                "accepted {width}x{height}"
            );
        }
        assert!(ShmMapping::new(0).is_none());
        assert!(ShmMapping::new(i32::MAX as usize + 1).is_none());
    }

    #[test]
    fn repeated_mapping_release_on_normal_and_early_error_paths() {
        fn allocate_then_return(early_error: bool, file: &mut Option<FileIdentity>) -> Option<()> {
            let mapping = ShmMapping::new(4096)?;
            let identity = identity(&mapping.fd);
            *file = Some(identity);
            assert_eq!(open_fds(identity), 1);
            assert!(has_mapping(identity));
            if early_error {
                None?;
            }
            Some(())
        }

        for _ in 0..32 {
            for early_error in [false, true] {
                let mut file = None;
                assert_eq!(
                    allocate_then_return(early_error, &mut file).is_none(),
                    early_error
                );
                let file = file.unwrap();
                assert!(!has_mapping(file), "thumbnail mapping survived its owner");
                assert_eq!(open_fds(file), 0, "thumbnail memfd survived its owner");
            }
        }
    }

    #[test]
    fn socket_registration_clears_duplicate_on_early_error() {
        fn fail_after_registration(
            cancellation: &ThumbnailCancellation,
            connection: &Connection,
        ) -> Result<(), ()> {
            let _registration = cancellation.register(connection).unwrap().unwrap();
            assert!(cancellation.socket.lock().unwrap().is_some());
            assert_eq!(open_fds(identity(connection)), 2);
            Err(())?;
            Ok(())
        }

        let (client, _peer) = UnixStream::pair().unwrap();
        let connection = Connection::from_socket(client).unwrap();
        let file = identity(&connection);
        let cancellation = ThumbnailCancellation::default();
        assert!(fail_after_registration(&cancellation, &connection).is_err());
        assert!(cancellation.socket.lock().unwrap().is_none());
        assert_eq!(open_fds(file), 1);
        drop(connection);
        assert_eq!(open_fds(file), 0);
    }

    #[test]
    fn cancellation_before_socket_registration_does_not_keep_duplicate() {
        let (client, _peer) = UnixStream::pair().unwrap();
        let connection = Connection::from_socket(client).unwrap();
        let file = identity(&connection);
        let cancellation = Arc::new(ThumbnailCancellation::default());
        drop(ThumbnailStream {
            cancellation: cancellation.clone(),
            join: None,
        });
        assert!(cancellation.register(&connection).unwrap().is_none());
        assert!(cancellation.socket.lock().unwrap().is_none());
        assert_eq!(open_fds(file), 1);
        drop(connection);
        assert_eq!(open_fds(file), 0);
    }

    #[test]
    fn cancellation_during_socket_registration_does_not_miss_worker() {
        let (client, peer) = UnixStream::pair().unwrap();
        let connection = Connection::from_socket(client).unwrap();
        let file = identity(&connection);
        let cancellation = Arc::new(ThumbnailCancellation::default());
        // Hold registration at its lock until the duplicate exists and Drop has
        // set the stop flag. Either lock acquisition order must then be safe.
        let registration_lock = cancellation.socket.lock().unwrap();
        let worker_cancellation = cancellation.clone();
        let (registered_tx, registered_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let registration = worker_cancellation.register(&connection).unwrap();
            let registered = registration.is_some();
            drop(registration);
            registered_tx.send(registered).unwrap();
        });
        let duplicate_created = wait_until(|| open_fds(file) == 2);
        let stream = ThumbnailStream {
            cancellation: cancellation.clone(),
            join: Some(worker),
        };
        let (done_tx, done_rx) = mpsc::channel();
        let dropper = std::thread::spawn(move || {
            drop(stream);
            done_tx.send(()).unwrap();
        });
        let cancel_started = wait_until(|| cancellation.is_stopped());
        drop(registration_lock);
        let timely = finish_drop(dropper, done_rx, &peer);
        assert!(
            duplicate_created,
            "worker never reached socket registration"
        );
        assert!(cancel_started, "stream drop did not request cancellation");
        assert!(timely, "stream drop did not finish within watchdog");
        assert!(!registered_rx.recv_timeout(WATCHDOG).unwrap());
        assert!(cancellation.socket.lock().unwrap().is_none());
        assert_eq!(open_fds(file), 0);
    }

    #[test]
    fn stream_drop_interrupts_real_wayland_roundtrip_before_joining() {
        // A private socket pair acts as an unresponsive compositor. No process
        // environment changes or connection to the user's compositor are needed.
        let (client, mut peer) = UnixStream::pair().unwrap();
        peer.set_read_timeout(Some(WATCHDOG)).unwrap();
        let connection = Connection::from_socket(client).unwrap();
        let file = identity(&connection);
        let cancellation = Arc::new(ThumbnailCancellation::default());
        let worker_cancellation = cancellation.clone();
        let (result_tx, result_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let failed = {
                let _registration = worker_cancellation.register(&connection).unwrap().unwrap();
                let mut queue: EventQueue<AppData> = connection.new_event_queue();
                let mut app = AppData {
                    state: Arc::new(Mutex::new(DiscoveryState::default())),
                    capture: Arc::new(Mutex::new(CaptureState::default())),
                };
                queue.roundtrip(&mut app).is_err()
            };
            result_tx.send(failed).unwrap();
        });
        let stream = ThumbnailStream {
            cancellation: cancellation.clone(),
            join: Some(worker),
        };
        // Read the actual wl_display.sync request and deliberately never reply.
        let mut request = [0u8; 12];
        let read_result = peer.read_exact(&mut request);
        let pending = matches!(
            result_rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
        let (done_tx, done_rx) = mpsc::channel();
        let dropper = std::thread::spawn(move || {
            drop(stream);
            done_tx.send(()).unwrap();
        });
        let timely = finish_drop(dropper, done_rx, &peer);
        read_result.unwrap();
        assert_eq!(u32::from_ne_bytes(request[..4].try_into().unwrap()), 1);
        assert_eq!(
            u32::from_ne_bytes(request[4..8].try_into().unwrap()),
            12 << 16
        );
        assert!(
            pending,
            "roundtrip returned without a sync reply or cancellation"
        );
        assert!(
            timely,
            "stream drop did not interrupt roundtrip within watchdog"
        );
        assert!(
            result_rx.recv_timeout(WATCHDOG).unwrap(),
            "shutdown should fail the pending roundtrip"
        );
        assert!(cancellation.is_stopped());
        assert!(cancellation.socket.lock().unwrap().is_none());
        assert_eq!(
            open_fds(file),
            0,
            "worker connection or duplicate survived shutdown"
        );
    }
}
