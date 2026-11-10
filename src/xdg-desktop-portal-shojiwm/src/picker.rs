//! iced-based source picker.
//!
//! `iced::daemon` lets us run an app with no initial window. We open a window
//! when a `PickRequest` arrives over the cross-thread channel and close it
//! once the user confirms a choice (or cancels). cosmic-text under the hood
//! handles font fallback, so CJK strings render without any font setup.
//!
//! Threading model:
//! - main thread runs iced (winit event loop)
//! - the D-Bus / tokio worker thread sends `PickRequest`s via an mpsc channel
//! - the receiver is parked in a `OnceLock` and pulled out by the iced
//!   subscription on first poll

use std::sync::{Mutex, OnceLock};

use iced::widget::{
    button, column, container, image, responsive, row, scrollable, space, text, text_input,
};
use iced::window;
use iced::{Element, Length, Subscription, Task};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::{ReceiverStream, UnboundedReceiverStream};

use crate::i18n;
use crate::sources::{SelectionRequestId as RequestId, SourceInfo, SourceKind, SourceUpdate};

#[derive(Debug, Clone)]
pub enum PickResult {
    Source(SourceInfo),
    Cancelled,
}

#[derive(Debug)]
pub struct PickRequest {
    request_id: RequestId,
    pub session_key: String,
    pub sources: Vec<SourceInfo>,
    pub responder: oneshot::Sender<PickResult>,
    pub cancelled: watch::Receiver<bool>,
    pub prefer_windows: bool,
}

enum PickerCommand {
    Pick(PickRequest),
    CancelRequest(RequestId),
    CancelSession(String),
}

/// Handle the D-Bus side uses to request a pick. Cheap to clone.
#[derive(Clone)]
pub struct PickerHandle {
    tx: mpsc::UnboundedSender<PickerCommand>,
}

/// Dropping an unfinished pick future must dismiss only its own UI request.
struct CancelOnDrop {
    tx: mpsc::UnboundedSender<PickerCommand>,
    request_id: Option<RequestId>,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(request_id) = self.request_id {
            let _ = self.tx.send(PickerCommand::CancelRequest(request_id));
        }
    }
}

impl PickerHandle {
    pub async fn pick(
        &self,
        session_key: String,
        request_id: RequestId,
        sources: Vec<SourceInfo>,
        cancelled: watch::Receiver<bool>,
        prefer_windows: bool,
    ) -> PickResult {
        let mut guard = CancelOnDrop {
            tx: self.tx.clone(),
            request_id: Some(request_id),
        };
        let (responder, rx) = oneshot::channel();
        let request = PickRequest {
            request_id,
            session_key,
            sources,
            responder,
            cancelled,
            prefer_windows,
        };
        if self.tx.send(PickerCommand::Pick(request)).is_err() {
            tracing::error!("picker thread is gone");
            return PickResult::Cancelled;
        }
        let result = rx.await.unwrap_or(PickResult::Cancelled);
        guard.request_id = None;
        result
    }

    pub fn cancel_request(&self, request_id: RequestId) {
        let _ = self.tx.send(PickerCommand::CancelRequest(request_id));
    }

    pub fn cancel(&self, session_key: &str) {
        let _ = self
            .tx
            .send(PickerCommand::CancelSession(session_key.to_owned()));
    }
}

type ReceiverSlot = Mutex<Option<mpsc::UnboundedReceiver<PickerCommand>>>;
static PICKER_RX: OnceLock<ReceiverSlot> = OnceLock::new();

type ThumbnailRxSlot = Mutex<Option<mpsc::Receiver<SourceUpdate>>>;
static THUMBNAIL_RX: OnceLock<ThumbnailRxSlot> = OnceLock::new();

pub fn setup() -> (PickerHandle, mpsc::Sender<SourceUpdate>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let _ = PICKER_RX.set(Mutex::new(Some(rx)));
    let (thumb_tx, thumb_rx) = mpsc::channel(2);
    let _ = THUMBNAIL_RX.set(Mutex::new(Some(thumb_rx)));
    (PickerHandle { tx }, thumb_tx)
}

pub fn run_on_main_thread() -> iced::Result {
    iced::daemon(|| (State::default(), Task::none()), update, view)
        .title(|_state: &State, _id: window::Id| i18n::t("picker.window_title"))
        .subscription(subscription)
        .run()
}

#[derive(Default)]
struct State {
    active: Option<Active>,
    window_id: Option<window::Id>,
}

struct Active {
    request_id: RequestId,
    session_key: String,
    sources: Vec<SourceInfo>,
    responder: Option<oneshot::Sender<PickResult>>,
    tab: Tab,
    prefer_windows: bool,
    tab_chosen: bool,
    query: String,
    selected_id: Option<String>,
}

impl Active {
    fn visible(&self, source: &SourceInfo) -> bool {
        self.tab.contains(source)
            && source
                .label()
                .to_lowercase()
                .contains(&self.query.trim().to_lowercase())
    }

    fn selection(&self) -> Option<&SourceInfo> {
        self.selected_id.as_ref().and_then(|id| {
            self.sources
                .iter()
                .find(|source| source.id() == id && self.visible(source))
        })
    }

    fn revalidate_selection(&mut self) {
        if self.selection().is_none() {
            self.selected_id = None;
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
enum Tab {
    #[default]
    FullScreen,
    Window,
}

impl Tab {
    fn initial(sources: &[SourceInfo], prefer_windows: bool) -> Self {
        let has_outputs = sources
            .iter()
            .any(|source| Self::FullScreen.contains(source));
        let has_windows = sources.iter().any(|source| Self::Window.contains(source));
        if prefer_windows || (!has_outputs && has_windows) {
            Self::Window
        } else {
            Self::FullScreen
        }
    }

    fn contains(self, source: &SourceInfo) -> bool {
        matches!(
            (self, &source.kind),
            (Self::FullScreen, SourceKind::Output(_)) | (Self::Window, SourceKind::Toplevel(_))
        )
    }
}

#[derive(Debug, Clone)]
enum Message {
    RequestArrived(RequestArrived),
    WindowOpened(window::Id),
    WindowClosed(window::Id),
    SourceClicked {
        request_id: RequestId,
        source_id: String,
    },
    Confirm(RequestId),
    SearchSubmitted(RequestId),
    SearchChanged {
        request_id: RequestId,
        query: String,
    },
    KeyPressed {
        request_id: RequestId,
        window_id: window::Id,
        key: iced::keyboard::key::Named,
    },
    Cancelled(RequestId),
    SessionCancelled(String),
    TabSelected {
        request_id: RequestId,
        tab: Tab,
    },
    SourceUpdate(SourceUpdate),
}

/// Request handed off to the picker's event loop.
type PickHandoff = PickRequest;

/// Boxed handoff for the responder so Message can stay `Clone`.
#[derive(Debug, Clone)]
struct RequestArrived(std::sync::Arc<Mutex<Option<PickHandoff>>>);

fn update(state: &mut State, message: Message) -> Task<Message> {
    match message {
        Message::RequestArrived(arrived) => {
            let Some(request) = arrived.0.lock().unwrap().take() else {
                return Task::none();
            };
            if *request.cancelled.borrow() || request.responder.is_closed() {
                return Task::none();
            }
            if let Some(mut prev) = state.active.take()
                && let Some(r) = prev.responder.take()
            {
                let _ = r.send(PickResult::Cancelled);
            }
            state.active = Some(Active {
                request_id: request.request_id,
                session_key: request.session_key,
                tab: Tab::initial(&request.sources, request.prefer_windows),
                prefer_windows: request.prefer_windows,
                tab_chosen: false,
                sources: request.sources,
                responder: Some(request.responder),
                query: String::new(),
                selected_id: None,
            });
            if state.window_id.is_some() {
                Task::none()
            } else {
                let (id, open_task) = window::open(window::Settings {
                    size: iced::Size::new(720.0, 640.0),
                    min_size: Some(iced::Size::new(360.0, 480.0)),
                    ..Default::default()
                });
                state.window_id = Some(id);
                open_task.map(Message::WindowOpened)
            }
        }
        Message::WindowOpened(_id) => Task::none(),
        Message::WindowClosed(id) => {
            if state.window_id != Some(id) {
                return Task::none();
            }
            state.window_id = None;
            if let Some(mut active) = state.active.take()
                && let Some(r) = active.responder.take()
            {
                let _ = r.send(PickResult::Cancelled);
            }
            Task::none()
        }
        Message::SourceClicked {
            request_id,
            source_id,
        } => {
            if let Some(active) = state.active.as_mut()
                && active.request_id == request_id
                && active
                    .sources
                    .iter()
                    .any(|source| source.id() == source_id && active.visible(source))
            {
                active.selected_id = Some(source_id);
                active.tab_chosen = true;
            }
            Task::none()
        }
        Message::Confirm(request_id) => {
            let source = state
                .active
                .as_ref()
                .filter(|active| active.request_id == request_id)
                .and_then(Active::selection)
                .cloned();
            let Some(source) = source else {
                return Task::none();
            };
            finish(state, |_| Some(PickResult::Source(source)))
        }
        Message::SearchSubmitted(request_id) => {
            // Consume Enter in the search field, including IME/modified submit.
            // Sharing requires the Share button or unhandled Enter outside it.
            if let Some(active) = state.active.as_mut()
                && active.request_id == request_id
            {
                active.revalidate_selection();
            }
            Task::none()
        }
        Message::SearchChanged { request_id, query } => {
            if let Some(active) = state.active.as_mut()
                && active.request_id == request_id
            {
                active.query = query;
                active.revalidate_selection();
            }
            Task::none()
        }
        Message::KeyPressed {
            request_id,
            window_id,
            key,
        } => {
            if state.window_id != Some(window_id)
                || !state
                    .active
                    .as_ref()
                    .is_some_and(|active| active.request_id == request_id)
            {
                return Task::none();
            }
            match key {
                iced::keyboard::key::Named::Escape => update(state, Message::Cancelled(request_id)),
                iced::keyboard::key::Named::Enter => update(state, Message::Confirm(request_id)),
                _ => Task::none(),
            }
        }
        Message::Cancelled(request_id) => {
            if state
                .active
                .as_ref()
                .is_some_and(|active| active.request_id == request_id)
            {
                finish(state, |_| Some(PickResult::Cancelled))
            } else {
                Task::none()
            }
        }
        Message::SessionCancelled(session_key) => {
            if state
                .active
                .as_ref()
                .is_some_and(|active| active.session_key == session_key)
            {
                finish(state, |_| Some(PickResult::Cancelled))
            } else {
                Task::none()
            }
        }
        Message::TabSelected { request_id, tab } => {
            if let Some(active) = state.active.as_mut()
                && active.request_id == request_id
            {
                active.tab = tab;
                active.tab_chosen = true;
                active.revalidate_selection();
            }
            Task::none()
        }
        Message::SourceUpdate(update) => {
            if let Some(active) = state.active.as_mut()
                && active.session_key == update.session_key
                && active.request_id == update.request_id
            {
                active.sources = update.sources;
                // An empty initial discovery must not strand a windows-only request
                // on the monitor tab. Once the user chooses, respect that choice.
                if !active.tab_chosen {
                    active.tab = Tab::initial(&active.sources, active.prefer_windows);
                }
                active.revalidate_selection();
            }
            Task::none()
        }
    }
}

fn finish<F>(state: &mut State, choose: F) -> Task<Message>
where
    F: FnOnce(&[SourceInfo]) -> Option<PickResult>,
{
    let Some(mut active) = state.active.take() else {
        return Task::none();
    };
    let result = choose(&active.sources).unwrap_or(PickResult::Cancelled);
    if let Some(r) = active.responder.take() {
        let _ = r.send(result);
    }
    if let Some(id) = state.window_id.take() {
        window::close(id)
    } else {
        Task::none()
    }
}

fn view(state: &State, id: window::Id) -> Element<'_, Message> {
    // An old window may render once more while its asynchronous close is pending.
    // Never put the replacement request's actionable controls in that window.
    if state.window_id != Some(id) {
        return container(text(i18n::t("picker.idle"))).padding(16).into();
    }
    responsive(move |size| view_content(state, size.width >= 560.0)).into()
}

fn view_content(state: &State, wide: bool) -> Element<'_, Message> {
    let Some(active) = state.active.as_ref() else {
        return container(text(i18n::t("picker.idle"))).padding(16).into();
    };

    // Counts per tab, used to label and empty-state.
    let (n_outputs, n_windows) =
        active
            .sources
            .iter()
            .fold((0usize, 0usize), |acc, s| match s.kind {
                SourceKind::Output(_) => (acc.0 + 1, acc.1),
                SourceKind::Toplevel(_) => (acc.0, acc.1 + 1),
            });

    let request_id = active.request_id;
    let mut tabs = row![].spacing(8);
    for (tab, count, key) in [
        (Tab::FullScreen, n_outputs, "picker.tab_fullscreen"),
        (Tab::Window, n_windows, "picker.tab_window"),
    ] {
        tabs = tabs.push(tab_button(
            i18n::t_args(key, &[("count", &count.to_string())]),
            active.tab == tab,
            tab,
            request_id,
        ));
    }

    let search_hint = i18n::t("picker.search");
    let search = text_input(&search_hint, &active.query)
        .on_input(move |query| Message::SearchChanged { request_id, query })
        .on_submit(Message::SearchSubmitted(request_id))
        .padding(10);
    let mut list = column![].spacing(12).width(Length::Fill);
    let mut shown_any = false;
    for source in &active.sources {
        if !active.visible(source) {
            continue;
        }
        shown_any = true;
        let selected = active.selected_id.as_deref() == Some(source.id());
        // Compact rows at normal width; stack only when titles would be squeezed.
        let preview_width = if wide {
            Length::Fixed(180.0)
        } else {
            Length::Fill
        };
        let preview_height = if wide { 112 } else { 160 };
        let preview: Element<'_, Message> = if let Some(handle) = &source.thumbnail {
            image(handle.clone())
                .width(preview_width)
                .height(preview_height)
                .content_fit(iced::ContentFit::Contain)
                .into()
        } else {
            container(text(i18n::t("picker.preview_unavailable")).size(14))
                .center_x(preview_width)
                .center_y(preview_height)
                .into()
        };
        let (title, detail) = match &source.kind {
            SourceKind::Toplevel(window) => (
                if window.title.is_empty() {
                    source.label()
                } else {
                    window.title.clone()
                },
                if window.app_id.is_empty() {
                    i18n::t("source.window")
                } else {
                    window.app_id.clone()
                },
            ),
            SourceKind::Output(_) => (source.label(), source.detail()),
        };
        let title = if title.is_empty() {
            i18n::t("source.window")
        } else {
            title
        };
        let marker = if selected {
            "picker.selected"
        } else {
            "picker.select"
        };
        let labels = column![
            text(title)
                .size(18)
                .width(Length::Fill)
                .wrapping(text::Wrapping::WordOrGlyph),
            text(detail)
                .size(14)
                .width(Length::Fill)
                .wrapping(text::Wrapping::WordOrGlyph),
            text(i18n::t(marker)).size(14)
        ]
        .spacing(6)
        .width(Length::Fill);
        let card: Element<'_, Message> = if wide {
            row![preview, labels]
                .spacing(14)
                .align_y(iced::Center)
                .into()
        } else {
            column![preview, labels].spacing(8).into()
        };
        list = list.push(
            button(card)
                .style(if selected {
                    button::primary
                } else {
                    button::secondary
                })
                .width(Length::Fill)
                .padding(12)
                .on_press(Message::SourceClicked {
                    request_id,
                    source_id: source.id().to_owned(),
                }),
        );
    }
    if !shown_any {
        let key = if active.sources.is_empty() {
            "picker.no_sources"
        } else if !active.query.trim().is_empty() {
            "picker.no_matches"
        } else {
            match active.tab {
                Tab::FullScreen => "picker.no_outputs",
                Tab::Window => "picker.no_windows",
            }
        };
        list = list.push(container(text(i18n::t(key)).width(Length::Fill)).padding(12));
    }
    let footer = row![
        button(text(i18n::t("picker.cancel")))
            .style(button::secondary)
            .padding([10, 16])
            .on_press(Message::Cancelled(request_id)),
        space::horizontal(),
        button(text(i18n::t("picker.share")))
            .padding([10, 16])
            .on_press_maybe(active.selection().map(|_| Message::Confirm(request_id))),
    ]
    .spacing(8);
    container(
        column![
            text(i18n::t("picker.heading")).size(20),
            text(i18n::t("picker.instructions")).size(14),
            tabs,
            search,
            scrollable(list).height(Length::Fill),
            footer
        ]
        .spacing(12),
    )
    .padding(16)
    .height(Length::Fill)
    .into()
}

fn tab_button(
    label: String,
    selected: bool,
    kind: Tab,
    request_id: RequestId,
) -> Element<'static, Message> {
    let btn = button(text(label));
    let btn = if selected {
        btn.style(button::primary)
    } else {
        btn.style(button::secondary)
    };
    btn.on_press(Message::TabSelected {
        request_id,
        tab: kind,
    })
    .into()
}

fn subscription(state: &State) -> Subscription<Message> {
    let request_stream = Subscription::run(|| {
        let rx = PICKER_RX
            .get()
            .expect("picker::setup() not called before run_on_main_thread")
            .lock()
            .unwrap()
            .take()
            .expect("picker subscription started twice");
        UnboundedReceiverStream::new(rx).map(|command| match command {
            PickerCommand::Pick(request) => Message::RequestArrived(RequestArrived(
                std::sync::Arc::new(Mutex::new(Some(request))),
            )),
            PickerCommand::CancelRequest(request_id) => Message::Cancelled(request_id),
            PickerCommand::CancelSession(session_key) => Message::SessionCancelled(session_key),
        })
    });

    let thumbnail_stream = Subscription::run(|| {
        let rx = THUMBNAIL_RX
            .get()
            .expect("picker::setup() not called before run_on_main_thread")
            .lock()
            .unwrap()
            .take()
            .expect("thumbnail subscription started twice");
        ReceiverStream::new(rx).map(Message::SourceUpdate)
    });

    let close_events = window::close_events().map(Message::WindowClosed);

    Subscription::batch([
        request_stream,
        thumbnail_stream,
        close_events,
        keyboard_events(state.active.as_ref().map(|active| active.request_id)),
    ])
}

fn keyboard_events(request_id: Option<RequestId>) -> Subscription<Message> {
    let Some(request_id) = request_id else {
        return Subscription::none();
    };
    iced::event::listen_with(|event, status, window_id| {
        if let iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
            key: iced::keyboard::Key::Named(key),
            modifiers,
            ..
        }) = event
            && keyboard_action(key, status, modifiers)
        {
            return Some((window_id, key));
        }
        None
    })
    .with(request_id)
    .map(|(request_id, (window_id, key))| Message::KeyPressed {
        request_id,
        window_id,
        key,
    })
}

fn keyboard_action(
    key: iced::keyboard::key::Named,
    status: iced::event::Status,
    modifiers: iced::keyboard::Modifiers,
) -> bool {
    // Escape works even in the search field. Captured/modified Enter (including
    // text entry and IME) must never implicitly confirm a selection.
    key == iced::keyboard::key::Named::Escape
        || (key == iced::keyboard::key::Named::Enter
            && status == iced::event::Status::Ignored
            && modifiers.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window_source() -> SourceInfo {
        SourceInfo {
            kind: SourceKind::Toplevel(crate::sources::ToplevelInfo {
                identifier: "window".into(),
                app_id: "app".into(),
                title: "Window".into(),
            }),
            thumbnail: None,
        }
    }

    #[test]
    fn window_request_opens_the_window_tab() {
        let (responder, _response) = oneshot::channel();
        let (_cancel, cancelled) = watch::channel(false);
        let mut state = State {
            active: None,
            window_id: Some(window::Id::unique()),
        };
        let _ = update(
            &mut state,
            Message::RequestArrived(RequestArrived(std::sync::Arc::new(Mutex::new(Some(
                PickRequest {
                    request_id: RequestId::unique(),
                    session_key: "session".into(),
                    sources: vec![window_source()],
                    responder,
                    cancelled,
                    prefer_windows: true,
                },
            ))))),
        );
        assert_eq!(state.active.unwrap().tab, Tab::Window);
    }

    #[test]
    fn closing_an_old_window_does_not_cancel_the_current_request() {
        let (responder, mut response) = oneshot::channel();
        let id = window::Id::unique();
        let mut state = State {
            active: Some(Active {
                request_id: RequestId::unique(),
                session_key: "current".into(),
                sources: vec![window_source()],
                responder: Some(responder),
                tab: Tab::Window,
                prefer_windows: true,
                tab_chosen: false,
                query: String::new(),
                selected_id: None,
            }),
            window_id: Some(id),
        };
        let _ = update(&mut state, Message::WindowClosed(window::Id::unique()));
        assert_eq!(state.window_id, Some(id));
        assert!(state.active.is_some());
        assert!(matches!(
            response.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
    }
}

#[cfg(test)]
mod safety_and_usability_tests {
    use super::*;
    use crate::sources::{OutputInfo, ToplevelInfo};
    use iced::keyboard::key::Named;

    fn window(id: &str, title: &str, app_id: &str) -> SourceInfo {
        SourceInfo {
            kind: SourceKind::Toplevel(ToplevelInfo {
                identifier: id.into(),
                title: title.into(),
                app_id: app_id.into(),
            }),
            thumbnail: None,
        }
    }

    fn output() -> SourceInfo {
        SourceInfo {
            kind: SourceKind::Output(OutputInfo {
                name: "DP-1".into(),
                description: "Desk display".into(),
                position: (0, 0),
                width: 1920,
                height: 1080,
                refresh_mhz: 60000,
                transform: wayland_client::protocol::wl_output::Transform::Normal,
            }),
            thumbnail: None,
        }
    }

    fn arrived(request: PickRequest) -> Message {
        Message::RequestArrived(RequestArrived(std::sync::Arc::new(Mutex::new(Some(
            request,
        )))))
    }

    fn request(
        state: &mut State,
        sources: Vec<SourceInfo>,
        prefer_windows: bool,
    ) -> (RequestId, oneshot::Receiver<PickResult>) {
        let (responder, response) = oneshot::channel();
        let (_, cancelled) = watch::channel(false);
        let request_id = RequestId::unique();
        state.window_id.get_or_insert_with(window::Id::unique);
        let _ = update(
            state,
            arrived(PickRequest {
                request_id,
                session_key: "session".into(),
                sources,
                responder,
                cancelled,
                prefer_windows,
            }),
        );
        (request_id, response)
    }

    fn click(state: &mut State, request_id: RequestId, source_id: &str) {
        let _ = update(
            state,
            Message::SourceClicked {
                request_id,
                source_id: source_id.into(),
            },
        );
    }

    fn search(state: &mut State, request_id: RequestId, query: &str) {
        let _ = update(
            state,
            Message::SearchChanged {
                request_id,
                query: query.into(),
            },
        );
    }

    fn refresh(state: &mut State, sources: Vec<SourceInfo>) {
        let request_id = state.active.as_ref().unwrap().request_id;
        let _ = update(
            state,
            Message::SourceUpdate(SourceUpdate {
                request_id,
                session_key: "session".into(),
                sources,
            }),
        );
    }

    fn pending(response: &mut oneshot::Receiver<PickResult>) {
        assert!(matches!(
            response.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn abandoning_pick_requests_ui_cleanup() {
        let (tx, mut commands) = mpsc::unbounded_channel();
        let picker = PickerHandle { tx };
        let (_, cancelled) = watch::channel(false);
        let task = tokio::spawn(async move {
            picker
                .pick(
                    "session".into(),
                    RequestId::unique(),
                    vec![],
                    cancelled,
                    false,
                )
                .await
        });
        let Some(PickerCommand::Pick(request)) = commands.recv().await else {
            panic!("missing pick");
        };
        let request_id = request.request_id;
        let mut state = State::default();
        let _ = update(&mut state, arrived(request));
        task.abort();
        let _ = task.await;
        assert!(
            state
                .active
                .as_ref()
                .unwrap()
                .responder
                .as_ref()
                .unwrap()
                .is_closed()
        );
        assert!(
            matches!(commands.recv().await, Some(PickerCommand::CancelRequest(id)) if id == request_id)
        );
        let _ = update(&mut state, Message::Cancelled(request_id));
        assert!(state.active.is_none());
        assert!(state.window_id.is_none());
    }

    #[tokio::test]
    async fn dropping_an_old_future_in_the_same_session_cannot_cancel_a_new_pick() {
        let (tx, mut commands) = mpsc::unbounded_channel();
        let picker = PickerHandle { tx };
        let old_picker = picker.clone();
        let (_, cancelled) = watch::channel(false);
        let task = tokio::spawn(async move {
            old_picker
                .pick(
                    "session".into(),
                    RequestId::unique(),
                    vec![],
                    cancelled,
                    false,
                )
                .await
        });
        let Some(PickerCommand::Pick(old)) = commands.recv().await else {
            panic!("missing old pick");
        };
        let old_id = old.request_id;
        let mut state = State::default();
        let (current_id, mut current_response) =
            request(&mut state, vec![window("a", "Editor", "App")], false);
        assert_ne!(old_id, current_id);
        task.abort();
        let _ = task.await;
        assert!(old.responder.is_closed());
        assert!(
            matches!(commands.recv().await, Some(PickerCommand::CancelRequest(id)) if id == old_id)
        );
        let _ = update(&mut state, Message::Cancelled(old_id));
        // The closed queued request cannot displace the current UI either.
        let _ = update(&mut state, arrived(old));
        assert_eq!(state.active.as_ref().unwrap().request_id, current_id);
        pending(&mut current_response);
    }

    #[tokio::test]
    async fn completing_pick_disarms_cleanup_and_explicit_session_cancel_remains_available() {
        let (tx, mut commands) = mpsc::unbounded_channel();
        let picker = PickerHandle { tx };
        let requester = picker.clone();
        let (_, cancelled) = watch::channel(false);
        let task = tokio::spawn(async move {
            requester
                .pick(
                    "session".into(),
                    RequestId::unique(),
                    vec![],
                    cancelled,
                    false,
                )
                .await
        });
        let Some(PickerCommand::Pick(request)) = commands.recv().await else {
            panic!("missing pick");
        };
        request.responder.send(PickResult::Cancelled).unwrap();
        assert!(matches!(task.await.unwrap(), PickResult::Cancelled));
        assert!(matches!(
            commands.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        picker.cancel("session");
        assert!(
            matches!(commands.recv().await, Some(PickerCommand::CancelSession(key)) if key == "session")
        );
    }

    #[test]
    fn closed_or_already_cancelled_queued_requests_never_open_a_window() {
        for cancel_watch in [false, true] {
            let (responder, response) = oneshot::channel();
            let (_, cancelled) = watch::channel(cancel_watch);
            if !cancel_watch {
                drop(response);
            }
            let mut state = State::default();
            let _ = update(
                &mut state,
                arrived(PickRequest {
                    request_id: RequestId::unique(),
                    session_key: "session".into(),
                    sources: vec![],
                    responder,
                    cancelled,
                    prefer_windows: false,
                }),
            );
            assert!(state.active.is_none());
            assert!(state.window_id.is_none());
        }
    }

    #[test]
    fn selection_requires_a_separate_confirmation_and_repeated_confirm_is_harmless() {
        let mut state = State::default();
        let (id, mut response) = request(&mut state, vec![window("a", "Editor", "App")], false);
        let _ = update(&mut state, Message::Confirm(id));
        click(&mut state, id, "missing");
        assert!(state.active.as_ref().unwrap().selection().is_none());
        click(&mut state, id, "a");
        click(&mut state, id, "a");
        let _ = update(&mut state, Message::SearchSubmitted(id));
        pending(&mut response);
        let _ = update(&mut state, Message::Confirm(id));
        assert!(
            matches!(response.try_recv(), Ok(PickResult::Source(source)) if source.id() == "a")
        );
        let _ = update(&mut state, Message::Confirm(id));
        assert!(state.active.is_none());
    }

    #[test]
    fn reorder_and_rename_confirm_the_selected_id_with_latest_metadata() {
        let mut state = State::default();
        let (id, mut response) = request(
            &mut state,
            vec![window("a", "Editor", "App"), window("b", "Browser", "Web")],
            false,
        );
        click(&mut state, id, "a");
        let mut renamed = window("a", "Renamed document", "NewApp");
        renamed.thumbnail = Some(image::Handle::from_rgba(1, 1, vec![1, 2, 3, 255]));
        refresh(&mut state, vec![window("b", "Browser", "Web"), renamed]);
        assert_eq!(
            state.active.as_ref().unwrap().selection().unwrap().id(),
            "a"
        );
        pending(&mut response);
        let _ = update(&mut state, Message::Confirm(id));
        let Ok(PickResult::Source(source)) = response.try_recv() else {
            panic!("missing selected source");
        };
        assert!(source.thumbnail.is_some());
        assert!(
            matches!(source.kind, SourceKind::Toplevel(window) if window.identifier == "a" && window.title == "Renamed document" && window.app_id == "NewApp")
        );
    }

    #[test]
    fn removed_selection_is_cleared_and_does_not_resurrect_when_source_returns() {
        let mut state = State::default();
        let a = window("a", "Editor", "App");
        let b = window("b", "Browser", "Web");
        let (id, mut response) = request(&mut state, vec![a.clone(), b.clone()], false);
        click(&mut state, id, "a");
        refresh(&mut state, vec![b.clone()]);
        assert!(state.active.as_ref().unwrap().selected_id.is_none());
        let _ = update(&mut state, Message::Confirm(id));
        click(&mut state, id, "a");
        refresh(&mut state, vec![b, a]);
        assert!(state.active.as_ref().unwrap().selected_id.is_none());
        pending(&mut response);
    }

    #[test]
    fn source_rename_that_no_longer_matches_search_clears_selection() {
        let mut state = State::default();
        let (id, mut response) = request(&mut state, vec![window("a", "Editor", "App")], false);
        search(&mut state, id, "  EDITOR  ");
        click(&mut state, id, "a");
        refresh(&mut state, vec![window("a", "Browser", "App")]);
        assert!(state.active.as_ref().unwrap().selected_id.is_none());
        let _ = update(&mut state, Message::Confirm(id));
        pending(&mut response);
    }

    #[test]
    fn search_matches_trimmed_case_insensitive_title_app_and_display_names() {
        let mut state = State::default();
        let (id, _response) = request(
            &mut state,
            vec![window("a", "日本語 Editor", "Org.Example.App"), output()],
            true,
        );
        for query in [" editor ", "ORG.EXAMPLE", " 日本語 ", ""] {
            search(&mut state, id, query);
            assert!(
                state
                    .active
                    .as_ref()
                    .unwrap()
                    .visible(&state.active.as_ref().unwrap().sources[0])
            );
        }
        let _ = update(
            &mut state,
            Message::TabSelected {
                request_id: id,
                tab: Tab::FullScreen,
            },
        );
        for query in [" dp-1 ", "DESK DISPLAY"] {
            search(&mut state, id, query);
            assert!(
                state
                    .active
                    .as_ref()
                    .unwrap()
                    .visible(&state.active.as_ref().unwrap().sources[1])
            );
        }
    }

    #[test]
    fn filtering_and_tab_changes_clear_hidden_selections_without_reselecting() {
        let mut state = State::default();
        let (id, mut response) = request(
            &mut state,
            vec![output(), window("a", "Editor", "App")],
            false,
        );
        click(&mut state, id, "DP-1");
        let _ = update(
            &mut state,
            Message::TabSelected {
                request_id: id,
                tab: Tab::Window,
            },
        );
        assert!(state.active.as_ref().unwrap().selection().is_none());
        click(&mut state, id, "DP-1");
        assert!(state.active.as_ref().unwrap().selection().is_none());
        click(&mut state, id, "a");
        search(&mut state, id, "absent");
        let _ = update(&mut state, Message::Confirm(id));
        search(&mut state, id, "");
        assert!(state.active.as_ref().unwrap().selected_id.is_none());
        pending(&mut response);
    }

    #[test]
    fn stale_ui_messages_cannot_mutate_or_finish_a_same_session_replacement() {
        let mut state = State::default();
        let (old, mut previous) = request(&mut state, vec![window("a", "Editor", "App")], false);
        click(&mut state, old, "a");
        search(&mut state, old, "Editor");
        let window_id = state.window_id.unwrap();
        let (new, mut response) = request(
            &mut state,
            vec![output(), window("a", "Editor", "App")],
            true,
        );
        assert!(matches!(previous.try_recv(), Ok(PickResult::Cancelled)));
        let active = state.active.as_ref().unwrap();
        assert_eq!(active.tab, Tab::Window);
        assert!(active.query.is_empty());
        assert!(active.selected_id.is_none());
        assert_eq!(state.window_id, Some(window_id));
        for message in [
            Message::SourceClicked {
                request_id: old,
                source_id: "a".into(),
            },
            Message::SearchChanged {
                request_id: old,
                query: "stale".into(),
            },
            Message::TabSelected {
                request_id: old,
                tab: Tab::FullScreen,
            },
            Message::Confirm(old),
            Message::SearchSubmitted(old),
            Message::Cancelled(old),
            Message::KeyPressed {
                request_id: old,
                window_id,
                key: Named::Escape,
            },
        ] {
            let _ = update(&mut state, message);
        }
        let active = state.active.as_ref().unwrap();
        assert_eq!(active.request_id, new);
        assert_eq!(active.tab, Tab::Window);
        assert!(active.query.is_empty());
        assert!(active.selected_id.is_none());
        click(&mut state, new, "a");
        let _ = update(&mut state, Message::Confirm(old));
        let _ = update(
            &mut state,
            Message::KeyPressed {
                request_id: old,
                window_id,
                key: Named::Enter,
            },
        );
        pending(&mut response);
    }

    #[test]
    fn stale_session_updates_and_cancels_do_not_affect_current_request() {
        let mut state = State::default();
        let (id, mut response) = request(&mut state, vec![window("a", "Editor", "App")], false);
        click(&mut state, id, "a");
        let _ = update(
            &mut state,
            Message::SourceUpdate(SourceUpdate {
                request_id: id,
                session_key: "old".into(),
                sources: vec![],
            }),
        );
        let _ = update(&mut state, Message::SessionCancelled("old".into()));
        assert_eq!(
            state.active.as_ref().unwrap().selection().unwrap().id(),
            "a"
        );
        pending(&mut response);
        let _ = update(&mut state, Message::SessionCancelled("session".into()));
        assert!(matches!(response.try_recv(), Ok(PickResult::Cancelled)));
    }

    #[test]
    fn old_same_session_snapshots_cannot_replace_current_sources_or_selection() {
        let mut state = State::default();
        let (old_id, _old_response) = request(&mut state, vec![output()], false);
        let (current_id, mut response) =
            request(&mut state, vec![window("a", "Current", "App")], true);
        click(&mut state, current_id, "a");
        for sources in [
            vec![],
            vec![output()],
            vec![window("a", "Stale title", "WrongApp")],
        ] {
            let _ = update(
                &mut state,
                Message::SourceUpdate(SourceUpdate {
                    session_key: "session".into(),
                    request_id: old_id,
                    sources,
                }),
            );
            let active = state.active.as_ref().unwrap();
            assert_eq!(active.sources.len(), 1);
            assert_eq!(active.selection().unwrap().label(), "App — Current");
        }
        refresh(&mut state, vec![window("a", "Latest", "App")]);
        let _ = update(&mut state, Message::Confirm(current_id));
        assert!(
            matches!(response.try_recv(), Ok(PickResult::Source(source)) if source.label() == "App — Latest")
        );
    }

    #[test]
    fn cancelled_older_discovery_cannot_replace_a_live_same_session_request() {
        let mut state = State::default();
        let (request_id, mut response) =
            request(&mut state, vec![window("a", "Current", "App")], true);
        let (responder, _old_response) = oneshot::channel();
        let (cancel, cancelled) = watch::channel(false);
        // The old discovery finishes after a new SelectSources cancels its watch.
        cancel.send_replace(true);
        let _ = update(
            &mut state,
            arrived(PickRequest {
                request_id: RequestId::unique(),
                session_key: "session".into(),
                sources: vec![output()],
                responder,
                cancelled,
                prefer_windows: false,
            }),
        );
        assert_eq!(state.active.as_ref().unwrap().request_id, request_id);
        assert_eq!(state.active.as_ref().unwrap().sources[0].id(), "a");
        pending(&mut response);
    }

    #[test]
    fn old_backend_completion_cancels_only_its_own_request() {
        let (tx, mut commands) = mpsc::unbounded_channel();
        let picker = PickerHandle { tx };
        let mut state = State::default();
        let (old_id, _old_response) = request(&mut state, vec![], false);
        let (current_id, mut response) =
            request(&mut state, vec![window("a", "Current", "App")], true);
        picker.cancel_request(old_id);
        let PickerCommand::CancelRequest(cancelled_id) = commands.try_recv().unwrap() else {
            panic!("normal completion must not cancel the whole session");
        };
        assert_eq!(cancelled_id, old_id);
        let _ = update(&mut state, Message::Cancelled(cancelled_id));
        assert_eq!(state.active.as_ref().unwrap().request_id, current_id);
        pending(&mut response);
        picker.cancel_request(current_id);
        let PickerCommand::CancelRequest(cancelled_id) = commands.try_recv().unwrap() else {
            panic!("missing current request cancellation");
        };
        let _ = update(&mut state, Message::Cancelled(cancelled_id));
        assert!(matches!(response.try_recv(), Ok(PickResult::Cancelled)));
    }

    #[test]
    fn empty_to_populated_updates_choose_a_usable_tab_and_honor_preference() {
        for prefer_windows in [false, true] {
            let mut state = State::default();
            let (id, mut response) = request(&mut state, vec![], prefer_windows);
            refresh(&mut state, vec![window("a", "Editor", "App")]);
            assert_eq!(state.active.as_ref().unwrap().tab, Tab::Window);
            refresh(&mut state, vec![output(), window("a", "Editor", "App")]);
            assert_eq!(
                state.active.as_ref().unwrap().tab,
                if prefer_windows {
                    Tab::Window
                } else {
                    Tab::FullScreen
                }
            );
            let _ = update(
                &mut state,
                Message::TabSelected {
                    request_id: id,
                    tab: Tab::Window,
                },
            );
            click(&mut state, id, "a");
            refresh(&mut state, vec![]);
            assert!(state.active.as_ref().unwrap().selected_id.is_none());
            refresh(&mut state, vec![output(), window("a", "Editor", "App")]);
            assert_eq!(state.active.as_ref().unwrap().tab, Tab::Window);
            assert!(state.active.as_ref().unwrap().selected_id.is_none());
            pending(&mut response);
        }
    }

    #[test]
    fn cancel_and_escape_work_with_empty_sources_and_hidden_selection() {
        for sources in [vec![], vec![window("a", "Editor", "App")]] {
            for escape in [false, true] {
                let mut state = State::default();
                let (id, mut response) = request(&mut state, sources.clone(), false);
                click(&mut state, id, "a");
                search(&mut state, id, "no matches");
                let message = if escape {
                    Message::KeyPressed {
                        request_id: id,
                        window_id: state.window_id.unwrap(),
                        key: Named::Escape,
                    }
                } else {
                    Message::Cancelled(id)
                };
                let _ = update(&mut state, message);
                assert!(matches!(response.try_recv(), Ok(PickResult::Cancelled)));
                assert!(state.active.is_none());
            }
        }
    }

    #[test]
    fn current_window_and_valid_selection_are_required_for_keyboard_confirmation() {
        let mut state = State::default();
        let (id, mut response) = request(&mut state, vec![window("a", "Editor", "App")], false);
        let current = state.window_id.unwrap();
        let stale = window::Id::unique();
        let _ = update(
            &mut state,
            Message::KeyPressed {
                request_id: id,
                window_id: current,
                key: Named::Enter,
            },
        );
        pending(&mut response);
        click(&mut state, id, "a");
        for key in [Named::Escape, Named::Enter] {
            let _ = update(
                &mut state,
                Message::KeyPressed {
                    request_id: id,
                    window_id: stale,
                    key,
                },
            );
        }
        let _ = update(&mut state, Message::WindowClosed(stale));
        pending(&mut response);
        let _ = update(
            &mut state,
            Message::KeyPressed {
                request_id: id,
                window_id: current,
                key: Named::Enter,
            },
        );
        assert!(
            matches!(response.try_recv(), Ok(PickResult::Source(source)) if source.id() == "a")
        );
    }

    #[test]
    fn captured_or_modified_enter_never_confirms_but_escape_can_cancel_search() {
        use iced::{event::Status, keyboard::Modifiers};
        assert!(keyboard_action(
            Named::Enter,
            Status::Ignored,
            Modifiers::empty()
        ));
        assert!(!keyboard_action(
            Named::Enter,
            Status::Captured,
            Modifiers::empty()
        ));
        for modifiers in [
            Modifiers::SHIFT,
            Modifiers::CTRL,
            Modifiers::ALT,
            Modifiers::LOGO,
        ] {
            assert!(!keyboard_action(Named::Enter, Status::Ignored, modifiers));
        }
        assert!(keyboard_action(
            Named::Escape,
            Status::Captured,
            Modifiers::empty()
        ));
        assert!(!keyboard_action(
            Named::Space,
            Status::Ignored,
            Modifiers::empty()
        ));
    }
}

/// Standalone manual QA only: synthetic sources, no D-Bus or capture connection.
#[cfg(debug_assertions)]
#[allow(dead_code)]
pub fn run_demo() -> iced::Result {
    use crate::sources::{OutputInfo, ToplevelInfo};
    let window = |title: &str, app: &str| SourceInfo {
        kind: SourceKind::Toplevel(ToplevelInfo {
            identifier: title.into(),
            title: title.into(),
            app_id: app.into(),
        }),
        thumbnail: None,
    };
    let output = || SourceInfo {
        kind: SourceKind::Output(OutputInfo {
            name: "DP-1".into(),
            description: "Desktop monitor".into(),
            position: (0, 0),
            width: 1920,
            height: 1080,
            refresh_mhz: 60000,
            transform: wayland_client::protocol::wl_output::Transform::Normal,
        }),
        thumbnail: None,
    };
    i18n::init();
    let mut sources = vec![
        window(
            "日本語の長いウィンドウタイトル・画面共有の確認 / A long document title that wraps on a narrow window",
            "org.example.Editor",
        ),
        window("Browser – project notes", "org.example.Browser"),
        output(),
    ];
    // A neutral synthetic image, never pixels captured from the user's screen.
    sources[0].thumbnail = Some(image::Handle::from_rgba(
        320,
        180,
        [70, 100, 130, 255].repeat(320 * 180),
    ));
    // Screenshot mode captures only this renderer's synthetic window, never the desktop.
    // Usage: SHOJI_PICKER_DEMO_SCREENSHOT=/tmp/picker.ppm
    // Optional: SHOJI_PICKER_DEMO_WIDTH=360, SHOJI_PICKER_DEMO_STATE=selected|empty|no-matches
    let screenshot = std::env::var_os("SHOJI_PICKER_DEMO_SCREENSHOT").map(std::path::PathBuf::from);
    let width = std::env::var("SHOJI_PICKER_DEMO_WIDTH")
        .ok()
        .and_then(|value| value.parse::<f32>().ok())
        .unwrap_or(720.0)
        .clamp(360.0, 1440.0);
    let fixture = std::env::var("SHOJI_PICKER_DEMO_STATE").unwrap_or_default();
    if fixture == "empty" {
        sources.clear();
    }

    struct DemoState {
        picker: State,
        screenshot: Option<std::path::PathBuf>,
        capture_started: bool,
    }
    #[derive(Debug, Clone)]
    enum DemoMessage {
        Picker(Message),
        Frame,
        Capture,
        Captured(window::Screenshot),
    }
    fn demo_view(state: &DemoState) -> Element<'_, DemoMessage> {
        view(
            &state.picker,
            state.picker.window_id.unwrap_or_else(window::Id::unique),
        )
        .map(DemoMessage::Picker)
    }
    iced::application(
        move || {
            (
                DemoState {
                    picker: State {
                        active: Some(Active {
                            request_id: RequestId::unique(),
                            session_key: "demo".into(),
                            sources: sources.clone(),
                            responder: None,
                            tab: Tab::Window,
                            prefer_windows: true,
                            tab_chosen: false,
                            query: if fixture == "no-matches" {
                                "no matching source".into()
                            } else {
                                String::new()
                            },
                            selected_id: (fixture == "selected")
                                .then(|| sources[0].id().to_owned()),
                        }),
                        window_id: None,
                    },
                    screenshot: screenshot.clone(),
                    capture_started: false,
                },
                window::latest().map(|id| DemoMessage::Picker(Message::WindowOpened(id.unwrap()))),
            )
        },
        |state: &mut DemoState, message| match message {
            DemoMessage::Picker(Message::WindowOpened(id)) => {
                state.picker.window_id = Some(id);
                Task::none()
            }
            DemoMessage::Picker(message) => {
                update(&mut state.picker, message).map(DemoMessage::Picker)
            }
            DemoMessage::Frame
                if state.screenshot.is_some()
                    && !state.capture_started
                    && state.picker.window_id.is_some() =>
            {
                state.capture_started = true;
                // Allow responsive layout and font rendering to settle after the first frame.
                Task::perform(
                    async { tokio::time::sleep(std::time::Duration::from_millis(300)).await },
                    |_| DemoMessage::Capture,
                )
            }
            DemoMessage::Capture => {
                if let Some(id) = state.picker.window_id {
                    window::screenshot(id).map(DemoMessage::Captured)
                } else {
                    iced::exit()
                }
            }
            DemoMessage::Captured(screenshot) => {
                use std::io::Write;
                let path = state.screenshot.take().expect("screenshot path");
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                    .expect("create new demo screenshot file");
                write!(
                    file,
                    "P6\n{} {}\n255\n",
                    screenshot.size.width, screenshot.size.height
                )
                .expect("write PPM header");
                let rgb: Vec<u8> = screenshot
                    .rgba
                    .chunks_exact(4)
                    .flat_map(|pixel| pixel[..3].iter().copied())
                    .collect();
                file.write_all(&rgb).expect("write PPM pixels");
                eprintln!(
                    "Demo screenshot saved: {} ({}x{})",
                    path.display(),
                    screenshot.size.width,
                    screenshot.size.height
                );
                iced::exit()
            }
            _ => Task::none(),
        },
        demo_view,
    )
    .title("ShojiWM Picker Demo (synthetic sources)")
    .window_size((width, 640.0))
    .subscription(|state: &DemoState| {
        let interactions = Subscription::batch([
            keyboard_events(state.picker.active.as_ref().map(|active| active.request_id)),
            window::close_events().map(Message::WindowClosed),
        ])
        .map(DemoMessage::Picker);
        if state.screenshot.is_some() && !state.capture_started {
            Subscription::batch([interactions, window::frames().map(|_| DemoMessage::Frame)])
        } else {
            interactions
        }
    })
    .run()
}
