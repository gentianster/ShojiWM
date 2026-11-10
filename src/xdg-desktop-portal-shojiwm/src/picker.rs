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
use tokio::sync::{mpsc, oneshot};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::i18n;
use crate::sources::{SourceInfo, SourceKind, ThumbnailUpdate};

#[derive(Debug, Clone)]
pub enum PickResult {
    Source(SourceInfo),
    Cancelled,
}

pub struct PickRequest {
    pub session_key: String,
    pub sources: Vec<SourceInfo>,
    pub responder: oneshot::Sender<PickResult>,
}

/// Handle the D-Bus side uses to request a pick. Cheap to clone.
enum PickerCommand {
    Pick(PickRequest),
    Cancel(String),
}

#[derive(Clone)]
pub struct PickerHandle {
    tx: mpsc::UnboundedSender<PickerCommand>,
}

impl PickerHandle {
    pub async fn pick(&self, session_key: String, sources: Vec<SourceInfo>) -> PickResult {
        struct CancelOnDrop(PickerHandle, String);
        impl Drop for CancelOnDrop {
            fn drop(&mut self) {
                let _ = self.0.tx.send(PickerCommand::Cancel(self.1.clone()));
            }
        }
        let _guard = CancelOnDrop(self.clone(), session_key.clone());
        let (responder, rx) = oneshot::channel();
        if self
            .tx
            .send(PickerCommand::Pick(PickRequest {
                session_key,
                sources,
                responder,
            }))
            .is_err()
        {
            tracing::error!("picker thread is gone");
            return PickResult::Cancelled;
        }
        rx.await.unwrap_or(PickResult::Cancelled)
    }
}

type ReceiverSlot = Mutex<Option<mpsc::UnboundedReceiver<PickerCommand>>>;
static PICKER_RX: OnceLock<ReceiverSlot> = OnceLock::new();

type ThumbnailRxSlot = Mutex<Option<mpsc::UnboundedReceiver<ThumbnailUpdate>>>;
static THUMBNAIL_RX: OnceLock<ThumbnailRxSlot> = OnceLock::new();

pub fn setup() -> (PickerHandle, mpsc::UnboundedSender<ThumbnailUpdate>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let _ = PICKER_RX.set(Mutex::new(Some(rx)));
    let (thumb_tx, thumb_rx) = mpsc::unbounded_channel();
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
    session_key: String,
    sources: Vec<SourceInfo>,
    responder: Option<oneshot::Sender<PickResult>>,
    tab: Tab,
    query: String,
    selected: Option<usize>,
}

impl Active {
    fn visible(&self, index: usize) -> bool {
        self.sources.get(index).is_some_and(|source| {
            self.tab.contains(source)
                && source
                    .label()
                    .to_lowercase()
                    .contains(&self.query.trim().to_lowercase())
        })
    }

    fn selection(&self) -> Option<usize> {
        self.selected.filter(|&index| self.visible(index))
    }
}

#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
enum Tab {
    #[default]
    FullScreen,
    Window,
}

impl Tab {
    fn initial(sources: &[SourceInfo]) -> Self {
        if sources
            .iter()
            .any(|source| matches!(source.kind, SourceKind::Output(_)))
        {
            Self::FullScreen
        } else {
            Self::Window
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
    SourceClicked(usize),
    Confirm,
    SearchChanged(String),
    KeyPressed(window::Id, iced::keyboard::key::Named),
    Cancelled,
    SessionCancelled(String),
    TabSelected(Tab),
    ThumbnailUpdate(ThumbnailUpdate),
}

/// `(picked sources, responder)` handed off to the picker's request handler.
type PickHandoff = (String, Vec<SourceInfo>, oneshot::Sender<PickResult>);

/// Boxed handoff for the responder so Message can stay `Clone`.
#[derive(Debug, Clone)]
struct RequestArrived(std::sync::Arc<Mutex<Option<PickHandoff>>>);

fn update(state: &mut State, message: Message) -> Task<Message> {
    match message {
        Message::RequestArrived(arrived) => {
            let Some((session_key, sources, responder)) = arrived.0.lock().unwrap().take() else {
                return Task::none();
            };
            if responder.is_closed() {
                return Task::none();
            }
            if let Some(mut prev) = state.active.take()
                && let Some(r) = prev.responder.take()
            {
                let _ = r.send(PickResult::Cancelled);
            }
            state.active = Some(Active {
                session_key,
                tab: Tab::initial(&sources),
                sources,
                responder: Some(responder),
                query: String::new(),
                selected: None,
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
        Message::SourceClicked(index) => {
            if let Some(active) = state.active.as_mut()
                && active.visible(index)
            {
                active.selected = Some(index);
            }
            Task::none()
        }
        Message::Confirm => {
            let Some(index) = state.active.as_ref().and_then(Active::selection) else {
                return Task::none();
            };
            finish(state, |sources| {
                sources.get(index).cloned().map(PickResult::Source)
            })
        }
        Message::SearchChanged(query) => {
            if let Some(active) = state.active.as_mut() {
                active.query = query;
                active.selected = active.selection();
            }
            Task::none()
        }
        Message::KeyPressed(id, key) => {
            if state.window_id != Some(id) {
                return Task::none();
            }
            match key {
                iced::keyboard::key::Named::Escape => update(state, Message::Cancelled),
                iced::keyboard::key::Named::Enter => update(state, Message::Confirm),
                _ => Task::none(),
            }
        }
        Message::SessionCancelled(key) => {
            if state
                .active
                .as_ref()
                .is_some_and(|active| active.session_key == key)
            {
                finish(state, |_| Some(PickResult::Cancelled))
            } else {
                Task::none()
            }
        }
        Message::Cancelled => finish(state, |_| Some(PickResult::Cancelled)),
        Message::TabSelected(tab) => {
            if let Some(active) = state.active.as_mut()
                && active.sources.iter().any(|source| tab.contains(source))
            {
                active.tab = tab;
                active.selected = active.selection();
            }
            Task::none()
        }
        Message::ThumbnailUpdate(update) => {
            if let Some(active) = state.active.as_mut() {
                for src in &mut active.sources {
                    if src.id() == update.source_id {
                        src.thumbnail = Some(update.handle.clone());
                        break;
                    }
                }
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

fn view(state: &State, _id: window::Id) -> Element<'_, Message> {
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

    let mut tabs = row![].spacing(8);
    for (tab, count, key) in [
        (Tab::FullScreen, n_outputs, "picker.tab_fullscreen"),
        (Tab::Window, n_windows, "picker.tab_window"),
    ] {
        if count > 0 {
            tabs = tabs.push(tab_button(
                i18n::t_args(key, &[("count", &count.to_string())]),
                active.tab == tab,
                tab,
            ));
        }
    }

    let search_hint = i18n::t("picker.search");
    let search = text_input(&search_hint, &active.query)
        .on_input(Message::SearchChanged)
        .on_submit(Message::Confirm)
        .padding(10);
    let mut list = column![].spacing(12).width(Length::Fill);
    let mut shown_any = false;
    for (index, source) in active.sources.iter().enumerate() {
        if !active.visible(index) {
            continue;
        }
        shown_any = true;
        let selected = active.selection() == Some(index);
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
            text(title).size(18).width(Length::Fill),
            text(detail).size(14).width(Length::Fill),
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
                .on_press(Message::SourceClicked(index)),
        );
    }
    if !shown_any {
        let key = if active.sources.is_empty() {
            "picker.no_sources"
        } else {
            "picker.no_matches"
        };
        list = list.push(container(text(i18n::t(key)).width(Length::Fill)).padding(12));
    }
    let footer = row![
        button(text(i18n::t("picker.cancel")))
            .style(button::secondary)
            .padding([10, 16])
            .on_press(Message::Cancelled),
        space::horizontal(),
        button(text(i18n::t("picker.share")))
            .padding([10, 16])
            .on_press_maybe(active.selection().map(|_| Message::Confirm)),
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

fn tab_button(label: String, selected: bool, kind: Tab) -> Element<'static, Message> {
    let btn = button(text(label));
    let btn = if selected {
        btn.style(button::primary)
    } else {
        btn.style(button::secondary)
    };
    btn.on_press(Message::TabSelected(kind)).into()
}

fn subscription(_state: &State) -> Subscription<Message> {
    let request_stream =
        Subscription::run(|| {
            let rx = PICKER_RX
                .get()
                .expect("picker::setup() not called before run_on_main_thread")
                .lock()
                .unwrap()
                .take()
                .expect("picker subscription started twice");
            UnboundedReceiverStream::new(rx).map(|command| match command {
                PickerCommand::Pick(req) => {
                    Message::RequestArrived(RequestArrived(std::sync::Arc::new(Mutex::new(Some(
                        (req.session_key, req.sources, req.responder),
                    )))))
                }
                PickerCommand::Cancel(key) => Message::SessionCancelled(key),
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
        UnboundedReceiverStream::new(rx).map(Message::ThumbnailUpdate)
    });

    let close_events = window::close_events().map(Message::WindowClosed);

    Subscription::batch([
        request_stream,
        thumbnail_stream,
        close_events,
        keyboard_events(),
    ])
}

fn keyboard_events() -> Subscription<Message> {
    iced::event::listen_with(|event, status, id| {
        if let iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
            key: iced::keyboard::Key::Named(key),
            modifiers,
            ..
        }) = event
        {
            // Escape also works while the search field owns keyboard focus.
            if key == iced::keyboard::key::Named::Escape
                || (key == iced::keyboard::key::Named::Enter
                    && status == iced::event::Status::Ignored
                    && modifiers.is_empty())
            {
                return Some(Message::KeyPressed(id, key));
            }
        }
        None
    })
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    #[tokio::test]
    async fn abandoning_pick_requests_ui_cleanup() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let picker = PickerHandle { tx };
        let task = tokio::spawn(async move { picker.pick("request-a".into(), vec![]).await });
        let Some(PickerCommand::Pick(request)) = rx.recv().await else {
            panic!("missing pick")
        };
        task.abort();
        let _ = task.await;
        assert!(request.responder.is_closed());
        assert!(matches!(rx.recv().await, Some(PickerCommand::Cancel(key)) if key == "request-a"));
    }

    #[test]
    fn stale_cancellation_does_not_close_new_picker() {
        let mut state = State::default();
        let (responder, _rx) = oneshot::channel();
        state.active = Some(Active {
            session_key: "new-request".into(),
            sources: vec![],
            responder: Some(responder),
            tab: Tab::default(),
            query: String::new(),
            selected: None,
        });
        let _ = update(&mut state, Message::SessionCancelled("old-request".into()));
        assert_eq!(state.active.as_ref().unwrap().session_key, "new-request");
        let _ = update(&mut state, Message::SessionCancelled("new-request".into()));
        assert!(state.active.is_none());
    }
}

#[cfg(test)]
mod usability_tests {
    use super::*;
    use crate::sources::{OutputInfo, ToplevelInfo};

    fn window(title: &str, app: &str) -> SourceInfo {
        SourceInfo {
            kind: SourceKind::Toplevel(ToplevelInfo {
                identifier: title.into(),
                title: title.into(),
                app_id: app.into(),
            }),
            thumbnail: None,
        }
    }

    fn output() -> SourceInfo {
        SourceInfo {
            kind: SourceKind::Output(OutputInfo {
                name: "DP-1".into(),
                description: "Desktop monitor".into(),
                width: 1920,
                height: 1080,
                refresh_mhz: 60000,
                transform: wayland_client::protocol::wl_output::Transform::Normal,
            }),
            thumbnail: None,
        }
    }

    fn request(state: &mut State, sources: Vec<SourceInfo>) -> oneshot::Receiver<PickResult> {
        let (tx, rx) = oneshot::channel();
        let _ = update(
            state,
            Message::RequestArrived(RequestArrived(std::sync::Arc::new(Mutex::new(Some((
                "request".into(),
                sources,
                tx,
            )))))),
        );
        rx
    }

    #[test]
    fn initial_tab_uses_available_sources() {
        assert_eq!(Tab::initial(&[window("Editor", "app")]), Tab::Window);
        assert_eq!(Tab::initial(&[output()]), Tab::FullScreen);
        assert_eq!(
            Tab::initial(&[window("Editor", "app"), output()]),
            Tab::FullScreen
        );
    }

    #[test]
    fn search_matches_title_and_application_case_insensitively() {
        let mut state = State::default();
        let _rx = request(
            &mut state,
            vec![
                window("日本語 document", "Org.Editor"),
                window("Browser", "web"),
            ],
        );
        for query in [" 日本語 ", "DOCUMENT", "org.EDITOR"] {
            let _ = update(&mut state, Message::SearchChanged(query.into()));
            let active = state.active.as_ref().unwrap();
            assert!(active.visible(0));
            assert!(!active.visible(1));
        }
        let _ = update(&mut state, Message::SearchChanged("not found".into()));
        assert!(!state.active.as_ref().unwrap().visible(0));
        let _ = update(&mut state, Message::SearchChanged("".into()));
        assert!(state.active.as_ref().unwrap().visible(1));
    }

    #[test]
    fn selecting_never_shares_until_confirmation() {
        let mut state = State::default();
        let mut rx = request(&mut state, vec![window("Editor", "app")]);
        let _ = update(&mut state, Message::Confirm);
        assert!(rx.try_recv().is_err());
        let _ = update(&mut state, Message::SourceClicked(99));
        assert_eq!(state.active.as_ref().unwrap().selection(), None);
        let _ = update(&mut state, Message::SourceClicked(0));
        assert!(rx.try_recv().is_err());
        assert_eq!(state.active.as_ref().unwrap().selection(), Some(0));
        let _ = update(&mut state, Message::Confirm);
        assert!(matches!(rx.try_recv(), Ok(PickResult::Source(source)) if source.id() == "Editor"));
        assert!(state.active.is_none());
    }

    #[test]
    fn hidden_selection_cannot_be_confirmed() {
        let mut state = State::default();
        let mut rx = request(&mut state, vec![output(), window("Editor", "app")]);
        let _ = update(&mut state, Message::SourceClicked(0));
        let _ = update(&mut state, Message::TabSelected(Tab::Window));
        assert_eq!(state.active.as_ref().unwrap().selection(), None);
        let _ = update(&mut state, Message::SourceClicked(0));
        assert_eq!(state.active.as_ref().unwrap().selection(), None);
        let _ = update(&mut state, Message::SourceClicked(1));
        let _ = update(&mut state, Message::SearchChanged("not found".into()));
        let _ = update(&mut state, Message::Confirm);
        assert_eq!(state.active.as_ref().unwrap().selection(), None);
        assert!(rx.try_recv().is_err());
        let _ = update(&mut state, Message::SearchChanged("".into()));
        assert_eq!(state.active.as_ref().unwrap().selection(), None);
    }

    #[test]
    fn replacement_request_resets_search_selection_and_tab() {
        let mut state = State::default();
        let mut old_rx = request(&mut state, vec![window("Editor", "app")]);
        let _ = update(&mut state, Message::SearchChanged("Editor".into()));
        let _ = update(&mut state, Message::SourceClicked(0));
        let id = state.window_id;
        let mut new_rx = request(&mut state, vec![output()]);
        assert!(matches!(old_rx.try_recv(), Ok(PickResult::Cancelled)));
        let active = state.active.as_ref().unwrap();
        assert!(active.query.is_empty());
        assert_eq!(active.selection(), None);
        assert_eq!(active.tab, Tab::FullScreen);
        assert_eq!(state.window_id, id);
        let _ = update(&mut state, Message::TabSelected(Tab::Window));
        assert_eq!(state.active.as_ref().unwrap().tab, Tab::FullScreen);
        let _ = update(&mut state, Message::Confirm);
        assert!(new_rx.try_recv().is_err());
        let _ = update(&mut state, Message::Cancelled);
        assert!(matches!(new_rx.try_recv(), Ok(PickResult::Cancelled)));
    }

    #[test]
    fn cancel_works_with_no_sources_and_escape_with_selection() {
        for sources in [vec![], vec![window("Editor", "app")]] {
            let mut state = State::default();
            let mut rx = request(&mut state, sources);
            let _ = update(&mut state, Message::SourceClicked(0));
            let id = state.window_id.unwrap();
            let _ = update(
                &mut state,
                Message::KeyPressed(id, iced::keyboard::key::Named::Escape),
            );
            assert!(matches!(rx.try_recv(), Ok(PickResult::Cancelled)));
            assert!(state.active.is_none());
        }
    }

    #[test]
    fn stale_close_and_keyboard_events_do_not_cancel_new_window() {
        let mut state = State::default();
        let mut rx = request(&mut state, vec![window("Editor", "app")]);
        let stale = window::Id::unique();
        let current = state.window_id.unwrap();
        let _ = update(&mut state, Message::WindowClosed(stale));
        let _ = update(
            &mut state,
            Message::KeyPressed(stale, iced::keyboard::key::Named::Escape),
        );
        assert_eq!(state.window_id, Some(current));
        assert!(rx.try_recv().is_err());
        let _ = update(
            &mut state,
            Message::KeyPressed(current, iced::keyboard::key::Named::Enter),
        );
        assert!(rx.try_recv().is_err());
        let _ = update(&mut state, Message::SourceClicked(0));
        let _ = update(
            &mut state,
            Message::KeyPressed(stale, iced::keyboard::key::Named::Enter),
        );
        assert!(rx.try_recv().is_err());
        let _ = update(
            &mut state,
            Message::KeyPressed(current, iced::keyboard::key::Named::Enter),
        );
        assert!(matches!(rx.try_recv(), Ok(PickResult::Source(_))));
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
                            session_key: "demo".into(),
                            sources: sources.clone(),
                            responder: None,
                            tab: Tab::Window,
                            query: if fixture == "no-matches" {
                                "no matching source".into()
                            } else {
                                String::new()
                            },
                            selected: (fixture == "selected").then_some(0),
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
            keyboard_events(),
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
