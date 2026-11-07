use std::io;
use std::sync::{Arc, Mutex};

use cosmic::app::{Core, Task};
use cosmic::applet::{menu_button, padded_control};
use cosmic::iced::platform_specific::shell::wayland::commands::popup::destroy_popup;
use cosmic::iced::widget::container as iced_container;
use cosmic::iced::{window, Alignment, Background, Border, Color, Length};
use cosmic::widget::{self, button, container, divider, slider, text, toggler};
use cosmic::Element;

use crate::razer::{
    KbMode, Keyboard, Razer, State, FAN_RPM_MAX, FAN_RPM_MIN, POWER_BALANCED, POWER_CUSTOM,
    ZONE_CPU, ZONE_GPU,
};

const APP_ID: &str = "dev.razerctl.CosmicAppletRazer";
const PANEL_ICON: &str = "input-gaming-symbolic";

const LEVELS: [&str; 4] = ["Low", "Medium", "High", "Boost"];

const SWATCHES: [[u8; 3]; 9] = [
    [0x44, 0xd6, 0x2c], // Razer green
    [0xff, 0xff, 0xff],
    [0xff, 0x20, 0x20],
    [0xff, 0x7a, 0x00],
    [0xff, 0xd4, 0x00],
    [0x00, 0xe5, 0xff],
    [0x20, 0x60, 0xff],
    [0xa0, 0x40, 0xff],
    [0xff, 0x40, 0xc0],
];

type Shared = Arc<Mutex<Option<Razer>>>;
type Outcome = (Result<State, String>, Option<String>);

pub struct RazerApplet {
    core: Core,
    popup: Option<window::Id>,
    dev: Shared,
    state: Option<State>,
    error: Option<String>,
    warning: Option<String>,
    /// Requests are numbered so a slow, older reply can't overwrite a newer one.
    seq: u64,
    applied: u64,
    fan_drag: Option<u32>,
    speed_drag: Option<u8>,
    battery_drag: Option<u8>,
}

#[derive(Debug, Clone)]
pub enum Message {
    TogglePopup,
    PopupClosed(window::Id),
    Refresh,
    Loaded(u64, Outcome),
    PowerMode(u8),
    Level(u8, u8),
    FanManual(bool),
    FanDrag(u32),
    FanRelease,
    KbMode(KbMode),
    KbColor([u8; 3]),
    KbSpeedDrag(u8),
    KbSpeedRelease,
    KbWaveRight(bool),
    LogoOn(bool),
    LogoBreathing(bool),
    BatteryOn(bool),
    BatteryDrag(u8),
    BatteryRelease,
}

/// Runs on a blocking thread: (re)open the laptop if needed, apply the change,
/// then read back the real state so the UI always reflects the hardware.
fn with_device(dev: Shared, f: impl FnOnce(&mut Razer) -> io::Result<()>) -> Outcome {
    let mut guard = match dev.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if guard.is_none() {
        match Razer::open() {
            Ok(d) => {
                tracing::info!("using {} (pid {:04x})", d.path.display(), d.pid);
                *guard = Some(d);
            }
            Err(e) => return (Err(e.to_string()), None),
        }
    }
    let d = guard.as_mut().unwrap();
    let warning = f(d).err().map(|e| format!("That setting didn't apply: {e}"));
    match d.read_state() {
        Ok(s) => (Ok(s), warning),
        Err(e) => {
            *guard = None; // e.g. after suspend; reopen next time
            (Err(format!("Lost connection to the laptop ({e}). Try again.")), None)
        }
    }
}

impl RazerApplet {
    fn run(
        &mut self,
        f: impl FnOnce(&mut Razer) -> io::Result<()> + Send + 'static,
    ) -> Task<Message> {
        self.seq += 1;
        let seq = self.seq;
        let dev = self.dev.clone();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || with_device(dev, f))
                    .await
                    .unwrap_or_else(|e| (Err(e.to_string()), None))
            },
            move |out| cosmic::Action::App(Message::Loaded(seq, out)),
        )
    }

    fn refresh(&mut self) -> Task<Message> {
        self.run(|_| Ok(()))
    }

    fn keyboard(&self) -> Keyboard {
        self.state.as_ref().and_then(|s| s.keyboard.clone()).unwrap_or_default()
    }

    fn set_keyboard(&mut self, kb: Keyboard) -> Task<Message> {
        if let Some(s) = self.state.as_mut() {
            s.keyboard = Some(kb.clone());
        }
        self.run(move |d| d.set_keyboard(&kb))
    }

    fn popup_view(&self) -> Element<'_, Message> {
        let mut content: Vec<Element<'_, Message>> = Vec::new();

        let header = widget::row::with_children(vec![
            widget::icon::from_name(PANEL_ICON).size(20).symbolic(true).into(),
            text::title4("Razer").width(Length::Fill).into(),
            button::icon(widget::icon::from_name("view-refresh-symbolic"))
                .on_press(Message::Refresh)
                .into(),
        ])
        .spacing(8)
        .align_y(Alignment::Center);
        content.push(padded_control(header).into());

        let state = match (&self.state, &self.error) {
            (_, Some(err)) => {
                content.push(padded_control(text::body(err.clone()).width(Length::Fill)).into());
                content.push(
                    menu_button(text::body("Try again").width(Length::Fill))
                        .on_press(Message::Refresh)
                        .into(),
                );
                return self.core.applet.popup_container(column(content)).into();
            }
            (None, None) => {
                content.push(padded_control(text::body("Connecting…")).into());
                return self.core.applet.popup_container(column(content)).into();
            }
            (Some(s), None) => s,
        };

        if let Some(w) = &self.warning {
            content.push(padded_control(text::caption(w.clone())).into());
        }

        // ---- Performance ----
        if let Some(mode) = state.power_mode {
            content.push(sep());
            content.push(heading("Performance"));
            let custom = mode == POWER_CUSTOM;
            content.push(
                padded_control(choices(vec![
                    choice("Balanced", mode == POWER_BALANCED, Message::PowerMode(POWER_BALANCED)),
                    choice("Custom", custom, Message::PowerMode(POWER_CUSTOM)),
                ]))
                .into(),
            );
            if custom {
                for (zone, name, count, current) in [
                    (ZONE_CPU, "CPU", 4, state.cpu_level),
                    (ZONE_GPU, "GPU", 3, state.gpu_level),
                ] {
                    let mut row: Vec<Element<'_, Message>> =
                        vec![text::body(name).width(Length::Fixed(40.0)).into()];
                    for (i, label) in LEVELS.iter().take(count).enumerate() {
                        let lvl = i as u8;
                        row.push(choice(label, current == Some(lvl), Message::Level(zone, lvl)));
                    }
                    content.push(padded_control(labeled_choices(row)).into());
                }
            }
        }

        // ---- Fans ----
        content.push(sep());
        content.push(toggle_row("Manual fan speed", state.fan_manual, Message::FanManual));
        if state.fan_manual {
            let rpm = self
                .fan_drag
                .or(state.fan_rpm)
                .unwrap_or(3500)
                .clamp(FAN_RPM_MIN, FAN_RPM_MAX);
            content.push(slider_row(
                slider(FAN_RPM_MIN..=FAN_RPM_MAX, rpm, Message::FanDrag)
                    .step(100u32)
                    .on_release(Message::FanRelease)
                    .into(),
                format!("{rpm} rpm"),
            ));
        }

        // ---- Keyboard ----
        if let Some(kb) = &state.keyboard {
            content.push(sep());
            content.push(heading("Keyboard"));
            let modes: Vec<Element<'_, Message>> = KbMode::ALL
                .iter()
                .map(|&m| choice(m.label(), kb.mode == m, Message::KbMode(m)))
                .collect();
            let mut modes = modes.into_iter();
            let first: Vec<_> = modes.by_ref().take(3).collect();
            let second: Vec<_> = modes.collect();
            content.push(
                padded_control(column(vec![choices(first).into(), choices(second).into()]).spacing(6))
                    .into(),
            );

            if kb.mode.uses_color() {
                let swatches: Vec<Element<'_, Message>> =
                    SWATCHES.iter().map(|&c| swatch(c, kb.rgb == c)).collect();
                content.push(
                    padded_control(widget::row::with_children(swatches).spacing(2)).into(),
                );
            }
            if kb.mode.uses_speed() {
                let speed = self.speed_drag.unwrap_or(kb.speed);
                content.push(slider_row(
                    slider(1u8..=15, speed, Message::KbSpeedDrag)
                        .on_release(Message::KbSpeedRelease)
                        .into(),
                    format!("speed {speed}"),
                ));
            }
            if kb.mode.uses_direction() {
                content.push(
                    padded_control(choices(vec![
                        choice("← Left", !kb.wave_right, Message::KbWaveRight(false)),
                        choice("Right →", kb.wave_right, Message::KbWaveRight(true)),
                    ]))
                    .into(),
                );
            }
        }

        // ---- Logo ----
        if let Some(on) = state.logo_on {
            content.push(sep());
            content.push(toggle_row("Logo light", on, Message::LogoOn));
            if on {
                content.push(
                    padded_control(choices(vec![
                        choice("Static", !state.logo_breathing, Message::LogoBreathing(false)),
                        choice("Breathing", state.logo_breathing, Message::LogoBreathing(true)),
                    ]))
                    .into(),
                );
            }
        }

        // ---- Battery health ----
        if let Some(on) = state.battery_limit_on {
            content.push(sep());
            content.push(toggle_row("Limit battery charge", on, Message::BatteryOn));
            if on {
                let lvl = self.battery_drag.unwrap_or(state.battery_limit);
                content.push(slider_row(
                    slider(50u8..=80, lvl, Message::BatteryDrag)
                        .step(5u8)
                        .on_release(Message::BatteryRelease)
                        .into(),
                    format!("{lvl}%"),
                ));
            }
        }

        self.core
            .applet
            .popup_container(column(content).padding([8, 0]))
            .into()
    }
}

impl cosmic::Application for RazerApplet {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = APP_ID;

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, _flags: ()) -> (Self, Task<Message>) {
        let mut app = Self {
            core,
            popup: None,
            dev: Arc::new(Mutex::new(None)),
            state: None,
            error: None,
            warning: None,
            seq: 0,
            applied: 0,
            fan_drag: None,
            speed_drag: None,
            battery_drag: None,
        };
        let task = app.refresh();
        (app, task)
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Message> {
        Some(Message::PopupClosed(id))
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::TogglePopup => {
                if let Some(p) = self.popup.take() {
                    return destroy_popup(p);
                }
                self.warning = None;
                let open = cosmic::surface::surface_task(cosmic::surface::action::app_popup(
                    |_| Default::default(),
                    |app: &mut Self| {
                        let id = window::Id::unique();
                        app.popup = Some(id);
                        app.core.applet.get_popup_settings(
                            app.core.main_window_id().unwrap(),
                            id,
                            Some((1, 1)),
                            None,
                            None,
                        )
                    },
                    None,
                ));
                let refresh = self.refresh();
                return Task::batch([open, refresh]);
            }
            Message::PopupClosed(id) => {
                if self.popup == Some(id) {
                    self.popup = None;
                }
            }
            Message::Refresh => {
                self.error = None;
                self.warning = None;
                return self.refresh();
            }
            Message::Loaded(seq, (result, warning)) => {
                if seq < self.applied {
                    return Task::none();
                }
                self.applied = seq;
                match result {
                    Ok(s) => {
                        self.state = Some(s);
                        self.error = None;
                    }
                    Err(e) => {
                        self.state = None;
                        self.error = Some(e);
                    }
                }
                if warning.is_some() {
                    self.warning = warning;
                }
            }

            Message::PowerMode(mode) => {
                let manual = self.state.as_ref().is_some_and(|s| s.fan_manual);
                if let Some(s) = self.state.as_mut() {
                    s.power_mode = Some(mode);
                }
                return self.run(move |d| d.set_power(mode, manual));
            }
            Message::Level(zone, level) => {
                let (needs_custom, manual) = match &self.state {
                    Some(s) => (s.power_mode != Some(POWER_CUSTOM), s.fan_manual),
                    None => (true, false),
                };
                if let Some(s) = self.state.as_mut() {
                    s.power_mode = Some(POWER_CUSTOM);
                    if zone == ZONE_CPU {
                        s.cpu_level = Some(level);
                    } else {
                        s.gpu_level = Some(level);
                    }
                }
                return self.run(move |d| {
                    if needs_custom {
                        // Keep the fan setting as-is (the web app reset it to auto here).
                        d.set_power(POWER_CUSTOM, manual)?;
                    }
                    d.set_level(zone, level)
                });
            }

            Message::FanManual(on) => {
                let (mode, rpm) = match &self.state {
                    Some(s) => (s.power_mode.unwrap_or(POWER_BALANCED), s.fan_rpm),
                    None => (POWER_BALANCED, None),
                };
                if let Some(s) = self.state.as_mut() {
                    s.fan_manual = on;
                }
                return self.run(move |d| {
                    d.set_power(mode, on)?;
                    if on {
                        d.set_fan_rpm(rpm.unwrap_or(3500))?;
                    }
                    Ok(())
                });
            }
            Message::FanDrag(rpm) => self.fan_drag = Some(rpm),
            Message::FanRelease => {
                if let Some(rpm) = self.fan_drag.take() {
                    if let Some(s) = self.state.as_mut() {
                        s.fan_rpm = Some(rpm);
                    }
                    return self.run(move |d| d.set_fan_rpm(rpm));
                }
            }

            Message::KbMode(mode) => {
                let mut kb = self.keyboard();
                kb.mode = mode;
                return self.set_keyboard(kb);
            }
            Message::KbColor(rgb) => {
                let mut kb = self.keyboard();
                kb.rgb = rgb;
                return self.set_keyboard(kb);
            }
            Message::KbSpeedDrag(v) => self.speed_drag = Some(v),
            Message::KbSpeedRelease => {
                if let Some(v) = self.speed_drag.take() {
                    let mut kb = self.keyboard();
                    kb.speed = v;
                    return self.set_keyboard(kb);
                }
            }
            Message::KbWaveRight(right) => {
                let mut kb = self.keyboard();
                kb.wave_right = right;
                return self.set_keyboard(kb);
            }

            Message::LogoOn(on) => {
                if let Some(s) = self.state.as_mut() {
                    s.logo_on = Some(on);
                }
                return self.run(move |d| d.set_logo_on(on));
            }
            Message::LogoBreathing(b) => {
                if let Some(s) = self.state.as_mut() {
                    s.logo_breathing = b;
                }
                return self.run(move |d| d.set_logo_breathing(b));
            }

            Message::BatteryOn(on) => {
                let lvl = self.state.as_ref().map_or(80, |s| s.battery_limit);
                if let Some(s) = self.state.as_mut() {
                    s.battery_limit_on = Some(on);
                }
                return self.run(move |d| d.set_battery_limit(on, lvl));
            }
            Message::BatteryDrag(v) => self.battery_drag = Some(v),
            Message::BatteryRelease => {
                if let Some(lvl) = self.battery_drag.take() {
                    if let Some(s) = self.state.as_mut() {
                        s.battery_limit = lvl;
                    }
                    return self.run(move |d| d.set_battery_limit(true, lvl));
                }
            }
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let btn = self
            .core
            .applet
            .icon_button(PANEL_ICON)
            .on_press_down(Message::TogglePopup);
        self.core.applet.autosize_window(btn).into()
    }

    fn view_window(&self, id: window::Id) -> Element<'_, Message> {
        if self.popup == Some(id) {
            self.popup_view()
        } else {
            text::body("").into()
        }
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }
}

// ---- small view helpers ---------------------------------------------------

type Col<'a> = widget::Column<'a, Message, cosmic::Theme, cosmic::Renderer>;
type RowW<'a> = widget::Row<'a, Message, cosmic::Theme, cosmic::Renderer>;

fn column(children: Vec<Element<'_, Message>>) -> Col<'_> {
    widget::column::with_children(children)
}

fn sep<'a>() -> Element<'a, Message> {
    padded_control(divider::horizontal::default())
        .padding([4, 12])
        .into()
}

fn heading(label: &str) -> Element<'_, Message> {
    padded_control(text::heading(label)).into()
}

fn choice<'a>(label: &'a str, selected: bool, msg: Message) -> Element<'a, Message> {
    let b = if selected {
        button::suggested(label)
    } else {
        button::standard(label)
    };
    b.on_press(msg).width(Length::Fill).into()
}

fn choices(items: Vec<Element<'_, Message>>) -> RowW<'_> {
    widget::row::with_children(items).spacing(6)
}

fn labeled_choices(items: Vec<Element<'_, Message>>) -> RowW<'_> {
    widget::row::with_children(items)
        .spacing(6)
        .align_y(Alignment::Center)
}

fn toggle_row<'a>(label: &'a str, on: bool, msg: fn(bool) -> Message) -> Element<'a, Message> {
    padded_control(
        widget::row::with_children(vec![
            text::heading(label).width(Length::Fill).into(),
            toggler(on).on_toggle(msg).into(),
        ])
        .align_y(Alignment::Center),
    )
    .into()
}

fn slider_row(slider: Element<'_, Message>, value: String) -> Element<'_, Message> {
    padded_control(
        widget::row::with_children(vec![
            slider,
            container(text::body(value))
                .width(Length::Fixed(72.0))
                .align_x(Alignment::End)
                .into(),
        ])
        .spacing(12)
        .align_y(Alignment::Center),
    )
    .into()
}

fn swatch<'a>(rgb: [u8; 3], selected: bool) -> Element<'a, Message> {
    let color = Color::from_rgb8(rgb[0], rgb[1], rgb[2]);
    let dot = container(text::body(""))
        .width(Length::Fixed(22.0))
        .height(Length::Fixed(22.0))
        .class(cosmic::style::Container::Custom(Box::new(move |theme| {
            iced_container::Style {
                background: Some(Background::Color(color)),
                border: Border {
                    radius: 11.0.into(),
                    width: if selected { 3.0 } else { 1.0 },
                    color: if selected {
                        theme.cosmic().accent_color().into()
                    } else {
                        Color::from_rgba(0.5, 0.5, 0.5, 0.5)
                    },
                },
                ..Default::default()
            }
        })));
    button::custom(dot)
        .padding(3)
        .class(cosmic::theme::Button::Text)
        .on_press(Message::KbColor(rgb))
        .into()
}
