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

#[derive(Debug, Clone)]
pub struct OutputInfo {
    pub name: String,
    pub description: String,
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
                "{}×{} @ {:.2}Hz",
                o.width,
                o.height,
                o.refresh_mhz as f64 / 1000.0
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
    proxy: wl_output::WlOutput,
    info: OutputInfo,
}

struct DiscoveredToplevel {
    proxy: ExtForeignToplevelHandleV1,
    info: ToplevelInfo,
}

#[derive(Default)]
struct DiscoveryState {
    outputs: Vec<DiscoveredOutput>,
    output_pending: Vec<OutputInfo>,
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
                registry.bind::<wl_output::WlOutput, _, _>(name, version.min(4), qh, ());
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
        if state.output_pending.is_empty() {
            state.output_pending.push(OutputInfo {
                name: String::new(),
                description: String::new(),
                width: 0,
                height: 0,
                refresh_mhz: 0,
                transform: wl_output::Transform::Normal,
            });
        }
        let cur = state.output_pending.last_mut().unwrap();
        match event {
            wl_output::Event::Name { name } => cur.name = name,
            wl_output::Event::Description { description } => cur.description = description,
            wl_output::Event::Geometry {
                transform: WEnum::Value(transform),
                ..
            } => cur.transform = transform,
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
                let info = state.output_pending.pop().unwrap();
                state.outputs.push(DiscoveredOutput {
                    proxy: output.clone(),
                    info,
                });
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
    if width == 0 || height == 0 {
        return None;
    }
    let format = format.unwrap_or(wl_shm::Format::Xrgb8888);
    let (stride, _) = shm_layout(width, height)?;
    let buffer = allocate_shm_buffer(app, qh, width, height, stride, format)?;

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
                let _ = rustix::mm::munmap(raw, size);
            }
            return None;
        };
        Some(Self { ptr, size, fd })
    }
}

impl Drop for ShmMapping {
    fn drop(&mut self) {
        unsafe {
            let _ = rustix::mm::munmap(self.ptr.as_ptr().cast(), self.size);
        }
    }
}

struct ShmBuffer {
    buffer: ProtocolGuard<wl_buffer::WlBuffer>,
    mapping: ShmMapping,
}

fn shm_layout(width: u32, height: u32) -> Option<(i32, usize)> {
    if width == 0 || height == 0 || height > i32::MAX as u32 {
        return None;
    }
    let stride = i32::try_from(width.checked_mul(4)?).ok()?;
    let size = usize::try_from(stride).ok()?.checked_mul(height as usize)?;
    if size > i32::MAX as usize {
        return None;
    }
    Some((stride, size))
}

fn allocate_shm_buffer(
    app: &mut AppData,
    qh: &QueueHandle<AppData>,
    width: u32,
    height: u32,
    stride: i32,
    format: wl_shm::Format,
) -> Option<ShmBuffer> {
    let shm = app.state.lock().unwrap().shm.clone()?;
    let (expected_stride, size) = shm_layout(width, height)?;
    if stride != expected_stride {
        return None;
    }
    let mapping = ShmMapping::new(size)?;
    let pool = ProtocolGuard::new(
        shm.create_pool(mapping.fd.as_fd(), size as i32, qh, ()),
        wl_shm_pool::WlShmPool::destroy,
    );
    let buffer = ProtocolGuard::new(
        pool.create_buffer(0, width as i32, height as i32, stride, format, qh, ()),
        wl_buffer::WlBuffer::destroy,
    );
    // wl_buffer retains the server-side storage; the pool object is no longer needed.
    drop(pool);
    Some(ShmBuffer { buffer, mapping })
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

/// A single thumbnail update pushed by the background thread.
#[derive(Debug, Clone)]
pub struct ThumbnailUpdate {
    pub source_id: String,
    pub handle: image::Handle,
}

/// Handle to the background thumbnail-refresh thread. Drop it to stop.
pub struct ThumbnailStream {
    stop: Arc<std::sync::atomic::AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
    socket: Arc<Mutex<Option<UnixStream>>>,
}

impl Drop for ThumbnailStream {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        // Interrupt a roundtrip even when the compositor stops responding.
        if let Some(socket) = self.socket.lock().unwrap().as_ref() {
            let _ = socket.shutdown(std::net::Shutdown::Both);
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Spawn a background thread that:
///   1. enumerates all sources + captures their first thumbnail,
///   2. sends the initial `Vec<SourceInfo>` through `init_tx`,
///   3. then round-robins through the sources, pushing fresh thumbnails on
///      `updates_tx` until the returned `ThumbnailStream` is dropped.
///
/// One wayland Connection lives on the thread for its whole lifetime —
/// per-source sessions are created and destroyed per refresh because
/// `ext-image-copy-capture-v1` sessions are themselves not long-lived
/// streaming primitives; they're billed per frame.
pub fn start_thumbnail_stream(
    init_tx: tokio::sync::oneshot::Sender<Vec<SourceInfo>>,
    updates_tx: tokio::sync::mpsc::UnboundedSender<ThumbnailUpdate>,
) -> ThumbnailStream {
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_for_thread = stop.clone();
    let socket = Arc::new(Mutex::new(None));
    let socket_for_thread = socket.clone();
    let join = std::thread::Builder::new()
        .name("portal-thumbnails".into())
        .spawn(move || {
            let result = run_stream(
                init_tx,
                updates_tx,
                stop_for_thread.clone(),
                socket_for_thread.clone(),
            );
            // Do not keep a failed worker's connection alive via the cancellation handle.
            socket_for_thread.lock().unwrap().take();
            if let Err(e) = result
                && !stop_for_thread.load(std::sync::atomic::Ordering::SeqCst)
            {
                tracing::warn!("thumbnail stream exited: {e}");
            }
        })
        .expect("spawn thumbnail thread");
    ThumbnailStream {
        stop,
        join: Some(join),
        socket,
    }
}

fn run_stream(
    init_tx: tokio::sync::oneshot::Sender<Vec<SourceInfo>>,
    updates_tx: tokio::sync::mpsc::UnboundedSender<ThumbnailUpdate>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    socket: Arc<Mutex<Option<UnixStream>>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::connect_to_env()?;
    {
        let mut socket = socket.lock().unwrap();
        *socket = Some(UnixStream::from(conn.as_fd().try_clone_to_owned()?));
        if stop.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(());
        }
    }
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

    // Snapshot the discovered proxies + identity strings up front. The
    // refresh loop keys off the identity string ("source_id") so the picker
    // can match incoming updates against its source list.
    enum LiveSource {
        Output {
            id: String,
            proxy: wl_output::WlOutput,
            info: OutputInfo,
        },
        Toplevel {
            id: String,
            proxy: ExtForeignToplevelHandleV1,
            info: ToplevelInfo,
        },
    }
    let live_sources: Vec<LiveSource> = {
        let s = state.lock().unwrap();
        let mut v: Vec<LiveSource> = Vec::new();
        for o in &s.outputs {
            v.push(LiveSource::Output {
                id: o.info.name.clone(),
                proxy: o.proxy.clone(),
                info: o.info.clone(),
            });
        }
        for t in &s.toplevels {
            v.push(LiveSource::Toplevel {
                id: t.info.identifier.clone(),
                proxy: t.proxy.clone(),
                info: t.info.clone(),
            });
        }
        v
    };

    // Send the source list immediately with thumbnails=None so the picker
    // window opens without delay. Thumbnails flow in through `updates_tx`
    // below, one source at a time — the picker fills them in as they arrive.
    let initial: Vec<SourceInfo> = live_sources
        .iter()
        .map(|src| match src {
            LiveSource::Output { info, .. } => SourceInfo {
                kind: SourceKind::Output(info.clone()),
                thumbnail: None,
            },
            LiveSource::Toplevel { info, .. } => SourceInfo {
                kind: SourceKind::Toplevel(info.clone()),
                thumbnail: None,
            },
        })
        .collect();
    let _ = init_tx.send(initial);

    // Round-robin refresh. Short sleep between captures keeps CPU bounded
    // and avoids starving the compositor / cursor when many windows are
    // present. Per-source rate ≈ 1 / (N_sources × (per_capture + tick))
    // — typical numbers: 1 source ~10fps, 5 sources ~2-3fps. Adjust if the
    // picker feels sluggish vs. too hot.
    let tick = std::time::Duration::from_millis(50);
    let empty_idle = std::time::Duration::from_millis(200);
    let mut live_sources = live_sources;
    let mut cursor = 0usize;
    while !stop.load(std::sync::atomic::Ordering::SeqCst) {
        if live_sources.is_empty() {
            std::thread::sleep(empty_idle);
            continue;
        }
        let idx = cursor % live_sources.len();
        cursor = cursor.wrapping_add(1);

        // Skip dead proxies (window closed, output unplugged) — and prune
        // them so we don't trip on them again. Touching a dead resource
        // would cause the compositor to send us a protocol error, killing
        // the whole stream.
        let alive = match &live_sources[idx] {
            LiveSource::Output { proxy, .. } => proxy.is_alive(),
            LiveSource::Toplevel { proxy, .. } => proxy.is_alive(),
        };
        if !alive {
            live_sources.remove(idx);
            continue;
        }

        let (id, handle) = match &live_sources[idx] {
            LiveSource::Output { id, proxy, .. } => (
                id.clone(),
                capture_output_thumbnail(&mut event_queue, &mut app, &qh, proxy),
            ),
            LiveSource::Toplevel { id, proxy, .. } => (
                id.clone(),
                capture_toplevel_thumbnail(&mut event_queue, &mut app, &qh, proxy),
            ),
        };
        if let Some(handle) = handle
            && updates_tx
                .send(ThumbnailUpdate {
                    source_id: id,
                    handle,
                })
                .is_err()
        {
            break;
        }
        std::thread::sleep(tick);
    }
    Ok(())
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn protocol_guard_destroys_once_on_early_return() {
        fn cleanup(counter: &Arc<AtomicUsize>) {
            counter.fetch_add(1, Ordering::SeqCst);
        }
        fn early_return(counter: Arc<AtomicUsize>) -> Option<()> {
            let _guard = ProtocolGuard::new(counter, cleanup);
            None?;
            Some(())
        }
        let counter = Arc::new(AtomicUsize::new(0));
        assert_eq!(early_return(counter.clone()), None);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn shm_layout_rejects_zero_and_overflow() {
        assert_eq!(shm_layout(320, 180), Some((1280, 230400)));
        for (w, h) in [
            (0, 100),
            (100, 0),
            (u32::MAX, 1),
            (1, u32::MAX),
            (1_000_000, 1_000_000),
        ] {
            assert!(shm_layout(w, h).is_none());
        }
        assert!(ShmMapping::new(0).is_none());
        assert!(ShmMapping::new(i32::MAX as usize + 1).is_none());
    }

    #[test]
    fn repeated_mapping_drop_releases_fd_and_mmap() {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::MetadataExt;
        for _ in 0..32 {
            let mapping = ShmMapping::new(4096).unwrap();
            let fd_path = format!("/proc/self/fd/{}", mapping.fd.as_raw_fd());
            let inode = std::fs::metadata(&fd_path).unwrap().ino();
            let has_mapping = || {
                std::fs::read_to_string("/proc/self/maps")
                    .unwrap()
                    .lines()
                    .any(|line| {
                        line.split_whitespace()
                            .nth(4)
                            .and_then(|s| s.parse::<u64>().ok())
                            == Some(inode)
                            && line.contains("memfd:shojiwm-portal-thumbnail")
                    })
            };
            assert!(has_mapping());
            drop(mapping);
            assert!(!has_mapping(), "thumbnail mapping survived its owner");
            assert!(
                std::fs::metadata(&fd_path)
                    .map(|m| m.ino() != inode)
                    .unwrap_or(true)
            );
        }
    }

    #[test]
    fn stream_drop_interrupts_blocked_reader_before_joining() {
        use std::io::Read;
        let (mut reader, _server) = UnixStream::pair().unwrap();
        let socket = reader.try_clone().unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let join = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let mut byte = [0];
            assert_eq!(reader.read(&mut byte).unwrap(), 0);
        });
        let stream = ThumbnailStream {
            stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            join: Some(join),
            socket: Arc::new(Mutex::new(Some(socket))),
        };
        let stop = stream.stop.clone();
        started_rx.recv().unwrap();
        drop(stream);
        assert!(stop.load(Ordering::SeqCst));
    }
}
