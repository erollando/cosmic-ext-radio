use crate::controller::{start_controller, PlaybackPhase, UiCommand};
use crate::models::{Station, StationRef};
use cosmic::app::{Core, Task};
use cosmic::iced::{Length, Rectangle};
use cosmic::iced_runtime::core::window;
use cosmic::surface::action::{app_popup, destroy_popup};
use cosmic::widget;

const APP_ID: &str = "io.github.xinia.RadioWidget";

pub struct RadioWidget {
    core: Core,
    controller: crate::controller::ControllerHandle,
    state: crate::controller::ControllerState,
    popup: Option<cosmic::iced::window::Id>,
}

#[derive(Clone, Debug)]
pub enum Message {
    PopupClosed(cosmic::iced::window::Id),
    Surface(cosmic::surface::Action),
    ControllerState(crate::controller::ControllerState),
    SearchInput(String),
    SearchSubmit,
    PlayStation(StationRef),
    TogglePause,
    Stop,
    ToggleFavorite(StationRef),
    Noop,
}

impl cosmic::Application for RadioWidget {
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = APP_ID;

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, _flags: Self::Flags) -> (Self, Task<Message>) {
        let controller = start_controller();
        let state = controller.state_rx.borrow().clone();
        (
            Self {
                core,
                controller,
                state,
                popup: None,
            },
            Task::none(),
        )
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Message> {
        Some(Message::PopupClosed(id))
    }

    fn subscription(&self) -> cosmic::iced::Subscription<Message> {
        use cosmic::iced_futures::futures::SinkExt;

        let mut rx = self.controller.state_rx.clone();
        cosmic::iced::Subscription::run_with_id(
            "controller_state",
            cosmic::iced_futures::stream::channel(16, move |mut output| async move {
                loop {
                    if rx.changed().await.is_err() {
                        break;
                    }
                    let snapshot = rx.borrow().clone();
                    let _ = output.send(Message::ControllerState(snapshot)).await;
                }
            }),
        )
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::PopupClosed(id) => {
                if self.popup.as_ref() == Some(&id) {
                    self.popup = None;
                }
                Task::none()
            }
            Message::Surface(a) => cosmic::task::message(cosmic::Action::Cosmic(
                cosmic::app::Action::Surface(a),
            )),
            Message::ControllerState(s) => {
                self.state = s;
                Task::none()
            }
            Message::SearchInput(s) => {
                self.state.search_query = s;
                Task::none()
            }
            Message::SearchSubmit => {
                let _ = self
                    .controller
                    .cmd_tx
                    .send(UiCommand::Search(self.state.search_query.clone()));
                Task::none()
            }
            Message::PlayStation(s) => {
                let _ = self.controller.cmd_tx.send(UiCommand::Play(s));
                Task::none()
            }
            Message::TogglePause => {
                let _ = self.controller.cmd_tx.send(UiCommand::TogglePause);
                Task::none()
            }
            Message::Stop => {
                let _ = self.controller.cmd_tx.send(UiCommand::Stop);
                Task::none()
            }
            Message::ToggleFavorite(s) => {
                let _ = self.controller.cmd_tx.send(UiCommand::ToggleFavorite(s));
                Task::none()
            }
            Message::Noop => Task::none(),
        }
    }

    fn view(&self) -> cosmic::Element<'_, Message> {
        let have_popup = self.popup;

        let full_label = self.state.label_text();
        let label = ellipsize_chars(&full_label, 30);

        let btn = self
            .core
            .applet
            .text_button(
                widget::text::body(label).width(Length::Fixed(240.0)),
                Message::Noop,
            )
            .width(Length::Fixed(240.0))
            .on_press_with_rectangle(move |offset, bounds| {
                if let Some(id) = have_popup {
                    Message::Surface(destroy_popup(id))
                } else {
                    Message::Surface(app_popup::<RadioWidget>(
                        move |state: &mut RadioWidget| {
                            let new_id = cosmic::iced::window::Id::unique();
                            state.popup = Some(new_id);
                            let mut popup_settings = state.core.applet.get_popup_settings(
                                state.core.main_window_id().unwrap(),
                                new_id,
                                None,
                                None,
                                None,
                            );

                            popup_settings.positioner.anchor_rect = Rectangle {
                                x: (bounds.x - offset.x) as i32,
                                y: (bounds.y - offset.y) as i32,
                                width: bounds.width as i32,
                                height: bounds.height as i32,
                            };

                            popup_settings
                        },
                        Some(Box::new(|state: &RadioWidget| {
                            state.popup_content().map(cosmic::Action::App)
                        })),
                    ))
                }
            });

        // IMPORTANT: move an owned String into the tooltip (no &full_label)
        let with_tooltip = self.core.applet.applet_tooltip::<Message>(
            btn,
            full_label, // <-- owned String, not a reference
            self.popup.is_some(),
            Message::Surface,
            None,
        );

        self.core.applet.autosize_window(with_tooltip).into()
    }

    fn view_window(&self, _id: cosmic::iced::window::Id) -> cosmic::Element<'_, Message> {
        "RadioWidget".into()
    }

    fn style(&self) -> Option<cosmic::iced_runtime::Appearance> {
        Some(cosmic::applet::style())
    }
}

// Simple char-based ellipsis
fn ellipsize_chars(s: &str, max_chars: usize) -> String {
    let mut it = s.chars();
    let taken: String = it.by_ref().take(max_chars).collect();
    if it.next().is_some() {
        format!("{taken}…")
    } else {
        taken
    }
}

impl RadioWidget {
    fn popup_content(&self) -> cosmic::Element<'_, Message> {
        let cosmic::cosmic_theme::Spacing {
            space_xxs,
            space_s,
            ..
        } = cosmic::theme::spacing();

        let search = widget::search_input("Search stations…", &self.state.search_query)
            .on_input(Message::SearchInput)
            .on_submit(|_| Message::SearchSubmit);

        let mut content = widget::column()
            .spacing(space_s)
            .padding(space_s)
            .push(search);

        if matches!(self.state.phase, PlaybackPhase::Playing | PlaybackPhase::Paused) {
            let pause_label = if self.state.phase == PlaybackPhase::Paused {
                "Resume"
            } else {
                "Pause"
            };
            let controls = widget::row()
                .spacing(space_xxs)
                .push(widget::button::text(pause_label).on_press(Message::TogglePause))
                .push(widget::button::text("Stop").on_press(Message::Stop));
            content = content.push(controls);
        }

        if let Some(err) = &self.state.error {
            content = content.push(widget::text::body(err));
        } else if self.state.search_loading {
            content = content.push(widget::text::body("Loading…"));
        } else if self.state.search_results.is_empty() {
            content = content.push(widget::text::body("Search to choose a station."));
        } else {
            content = content.push(self.results_list(&self.state.search_results));
        }

        cosmic::Element::from(self.core.applet.popup_container(content))
    }

    fn results_list<'a>(&'a self, stations: &'a [Station]) -> cosmic::Element<'a, Message> {
        let mut list = widget::list_column().padding(0).spacing(0);

        for s in stations {
            let subtitle = station_subtitle(s);
            let station_ref = StationRef {
                stationuuid: s.stationuuid.clone(),
                name: s.name.clone(),
            };
            let is_fav = self
                .state
                .favorites
                .iter()
                .any(|f| f.stationuuid == s.stationuuid);
            let fav_text = if is_fav { "★" } else { "☆" };

            let item = widget::row()
                .spacing(8)
                .push(
                    widget::button::custom(
                        widget::column()
                            .spacing(2)
                            .push(widget::text::body(&s.name))
                            .push(widget::text::caption(subtitle)),
                    )
                    .on_press(Message::PlayStation(station_ref.clone()))
                    .width(Length::Fill),
                )
                .push(widget::button::text(fav_text).on_press(Message::ToggleFavorite(station_ref)));

            list = list.add(item);
        }

        let scroll =
            cosmic::iced_widget::scrollable(list.into_element()).height(Length::Fixed(300.0));
        scroll.into()
    }
}

fn station_subtitle(s: &Station) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(c) = s.country.as_ref().map(|x| x.trim()).filter(|x| !x.is_empty()) {
        parts.push(c.to_string());
    }
    if let Some(codec) = s.codec.as_ref().map(|x| x.trim()).filter(|x| !x.is_empty()) {
        parts.push(codec.to_string());
    }
    if let Some(br) = s.bitrate {
        parts.push(format!("{br} kbps"));
    }
    if parts.is_empty() {
        String::new()
    } else {
        parts.join(" · ")
    }
}
