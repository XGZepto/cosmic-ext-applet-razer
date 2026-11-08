use std::io;
use std::sync::{Arc, Mutex};

use cosmic::app::{Core, Task};
use cosmic::applet::{menu_button, padded_control};
use cosmic::iced::platform_specific::shell::wayland::commands::popup::destroy_popup;
use cosmic::iced::widget::container as iced_container;
use cosmic::iced::{window, Alignment, Background, Border, Color, Length};
use cosmic::widget::segmented_button::{Entity, Selectable, SingleSelectModel};
use cosmic::widget::{
    self, button, container, divider, icon, segmented_control, slider, text, toggler,
};
use cosmic::Element;

use crate::razer::{
    DeviceInfo, KbMode, Keyboard, Razer, State, FAN_RPM_MAX, FAN_RPM_MIN, POWER_BALANCED,
    POWER_CUSTOM, ZONE_CPU, ZONE_GPU,
};

const APP_ID: &str = "dev.razerctl.CosmicAppletRazer";
const APP_ICON: &[u8] = include_bytes!("../data/icons/dev.razerctl.CosmicAppletRazer-symbolic.svg");
// Not in every icon theme, so it ships with the applet.
const PERFORMANCE_ICON: &[u8] = include_bytes!("../data/icons/performance-symbolic.svg");

// "Mid" rather than "Medium" so four segments fit the popup's fixed width.
const LEVELS: [&str; 4] = ["Low", "Mid", "High", "Boost"];

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

/// Collapsible groups in the popup; only one is open at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Performance,
    Lighting,
    Battery,
    Device,
}

/// Identifies which segmented control an activation came from.
#[derive(Debug, Clone, Copy)]
pub enum Seg {
    Power,
    Cpu,
    Gpu,
    KbTop,
    KbBottom,
    Wave,
    Logo,
}

/// Models backing the segmented controls. Each item carries the value it
/// stands for, and `sync` keeps the highlighted item in line with the laptop.
struct Segments {
    power: SingleSelectModel,
    cpu: SingleSelectModel,
    gpu: SingleSelectModel,
    // Six keyboard effects don't fit one row, so they're split over two.
    kb_top: SingleSelectModel,
    kb_bottom: SingleSelectModel,
    wave: SingleSelectModel,
    logo: SingleSelectModel,
}

impl Segments {
    fn new() -> Self {
        let kb = |modes: &[KbMode]| model(modes.iter().map(|&m| (m.label(), m)));
        Self {
            power: model([("Balanced", POWER_BALANCED), ("Custom", POWER_CUSTOM)]),
            cpu: model(LEVELS.into_iter().zip(0u8..)),
            gpu: model(LEVELS.into_iter().take(3).zip(0u8..)),
            kb_top: kb(&KbMode::ALL[..3]),
            kb_bottom: kb(&KbMode::ALL[3..]),
            wave: model([("← Left", false), ("Right →", true)]),
            logo: model([("Static", false), ("Breathing", true)]),
        }
    }

    fn get(&self, seg: Seg) -> &SingleSelectModel {
        match seg {
            Seg::Power => &self.power,
            Seg::Cpu => &self.cpu,
            Seg::Gpu => &self.gpu,
            Seg::KbTop => &self.kb_top,
            Seg::KbBottom => &self.kb_bottom,
            Seg::Wave => &self.wave,
            Seg::Logo => &self.logo,
        }
    }

    fn sync(&mut self, s: &State) {
        let kb = s.keyboard.as_ref();
        select(&mut self.power, s.power_mode);
        select(&mut self.cpu, s.cpu_level);
        select(&mut self.gpu, s.gpu_level);
        select(&mut self.kb_top, kb.map(|k| k.mode));
        select(&mut self.kb_bottom, kb.map(|k| k.mode));
        select(&mut self.wave, kb.map(|k| k.wave_right));
        select(&mut self.logo, Some(s.logo_breathing));
    }
}

fn model<T: Copy + 'static>(items: impl IntoIterator<Item = (&'static str, T)>) -> SingleSelectModel {
    items
        .into_iter()
        .fold(SingleSelectModel::builder(), |b, (label, v)| {
            b.insert(move |e| e.text(label).data(v))
        })
        .build()
}

/// Highlight the item holding `value`, or nothing if no item does.
fn select<T: PartialEq + 'static>(m: &mut SingleSelectModel, value: Option<T>) {
    let hit = value.and_then(|v| m.iter().find(|&e| m.data::<T>(e) == Some(&v)));
    match hit {
        Some(e) => m.activate(e),
        None => {
            // Model::deactivate() only clears a selection that's already empty,
            // so deactivate the active item explicitly.
            let active = m.active();
            Selectable::deactivate(m, active);
        }
    }
}

fn embedded_icon(svg: &'static [u8]) -> icon::Handle {
    let mut h = icon::from_svg_bytes(svg);
    h.symbolic = true;
    h
}

fn app_icon() -> icon::Handle {
    embedded_icon(APP_ICON)
}

fn themed_icon<'a>(name: &'static str) -> Element<'a, Message> {
    icon::from_name(name).size(20).symbolic(true).into()
}

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
    expanded: Option<Section>,
    seg: Segments,
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
    Expand(Section),
    Seg(Seg, Entity),
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
                tracing::info!("using {} (pid {:04x})", d.info.path.display(), d.info.pid);
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

    fn segmented(&self, seg: Seg) -> Element<'_, Message> {
        segmented_control::horizontal(self.seg.get(seg))
            .on_activate(move |e| Message::Seg(seg, e))
            .into()
    }

    fn handle(&mut self, message: Message) -> Task<Message> {
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
            Message::Expand(section) => {
                self.expanded = (self.expanded != Some(section)).then_some(section);
            }
            Message::Seg(seg, e) => {
                let m = self.seg.get(seg);
                let msg = match seg {
                    Seg::Power => m.data::<u8>(e).map(|&v| Message::PowerMode(v)),
                    Seg::Cpu => m.data::<u8>(e).map(|&v| Message::Level(ZONE_CPU, v)),
                    Seg::Gpu => m.data::<u8>(e).map(|&v| Message::Level(ZONE_GPU, v)),
                    Seg::KbTop | Seg::KbBottom => m.data::<KbMode>(e).map(|&v| Message::KbMode(v)),
                    Seg::Wave => m.data::<bool>(e).map(|&v| Message::KbWaveRight(v)),
                    Seg::Logo => m.data::<bool>(e).map(|&v| Message::LogoBreathing(v)),
                };
                if let Some(msg) = msg {
                    return self.handle(msg);
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

    /// A section is always one [header, body] column, with an empty body when
    /// closed, so every widget keeps the same place in the widget tree. Otherwise
    /// opening a section hands it the state of whatever used to sit at that
    /// position (e.g. a toggler briefly animating from another toggler's value).
    fn section<'a>(
        &self,
        id: Section,
        icon: Element<'a, Message>,
        title: &'static str,
        summary: String,
        body: impl FnOnce() -> Vec<Element<'a, Message>>,
    ) -> Element<'a, Message> {
        let open = self.expanded == Some(id);
        let body = if open { body() } else { Vec::new() };
        column(vec![section_header(id, open, icon, title, summary), column(body).into()]).into()
    }

    fn performance_body<'a>(&'a self, state: &'a State) -> Vec<Element<'a, Message>> {
        let mut body = Vec::new();
        if state.power_mode.is_some() {
            body.push(padded_control(self.segmented(Seg::Power)).into());
            let levels = if state.power_mode == Some(POWER_CUSTOM) {
                vec![
                    labeled("CPU", self.segmented(Seg::Cpu)),
                    labeled("GPU", self.segmented(Seg::Gpu)),
                ]
            } else {
                Vec::new()
            };
            body.push(column(levels).into());
        }
        body.push(toggle_row("Manual fan speed", state.fan_manual, Message::FanManual));
        let mut fan = Vec::new();
        if state.fan_manual {
            let rpm = self
                .fan_drag
                .or(state.fan_rpm)
                .unwrap_or(3500)
                .clamp(FAN_RPM_MIN, FAN_RPM_MAX);
            fan.push(slider_row(
                slider(FAN_RPM_MIN..=FAN_RPM_MAX, rpm, Message::FanDrag)
                    .step(100u32)
                    .on_release(Message::FanRelease)
                    .into(),
                format!("{rpm} rpm"),
            ));
        }
        body.push(column(fan).into());
        body
    }

    fn lighting_body<'a>(&'a self, state: &'a State) -> Vec<Element<'a, Message>> {
        let mut body = Vec::new();
        if let Some(kb) = &state.keyboard {
            body.push(
                padded_control(
                    column(vec![
                        text::caption("Keyboard").into(),
                        self.segmented(Seg::KbTop),
                        self.segmented(Seg::KbBottom),
                    ])
                    .spacing(6),
                )
                .into(),
            );
            let mut extras = Vec::new();
            if kb.mode.uses_color() {
                let swatches: Vec<Element<'_, Message>> =
                    SWATCHES.iter().map(|&c| swatch(c, kb.rgb == c)).collect();
                extras.push(padded_control(widget::row::with_children(swatches).spacing(2)).into());
            }
            if kb.mode.uses_speed() {
                let speed = self.speed_drag.unwrap_or(kb.speed);
                extras.push(slider_row(
                    slider(1u8..=15, speed, Message::KbSpeedDrag)
                        .on_release(Message::KbSpeedRelease)
                        .into(),
                    format!("speed {speed}"),
                ));
            }
            if kb.mode.uses_direction() {
                extras.push(padded_control(self.segmented(Seg::Wave)).into());
            }
            body.push(column(extras).into());
        }
        if let Some(on) = state.logo_on {
            body.push(toggle_row("Logo", on, Message::LogoOn));
            let mode = if on {
                vec![padded_control(self.segmented(Seg::Logo)).into()]
            } else {
                Vec::new()
            };
            body.push(column(mode).into());
        }
        body
    }

    fn battery_body<'a>(&'a self, state: &'a State, on: bool) -> Vec<Element<'a, Message>> {
        let mut slider_col = Vec::new();
        if on {
            let lvl = self.battery_drag.unwrap_or(state.battery_limit);
            slider_col.push(slider_row(
                slider(50u8..=80, lvl, Message::BatteryDrag)
                    .step(5u8)
                    .on_release(Message::BatteryRelease)
                    .into(),
                format!("{lvl}%"),
            ));
        }
        vec![
            toggle_row("Limit charge", on, Message::BatteryOn),
            column(slider_col).into(),
        ]
    }

    fn popup_view(&self) -> Element<'_, Message> {
        let mut content: Vec<Element<'_, Message>> = Vec::new();

        let title = self
            .state
            .as_ref()
            .and_then(|s| s.info.model.clone())
            .unwrap_or_else(|| "Razer".into());
        let header = widget::row::with_children(vec![
            widget::icon(app_icon()).size(24).into(),
            text::title4(title).width(Length::Fill).into(),
            button::icon(icon::from_name("view-refresh-symbolic"))
                .on_press(Message::Refresh)
                .into(),
        ])
        .spacing(12)
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

        let warning = self
            .warning
            .as_ref()
            .map(|w| padded_control(text::caption(w.clone())).into());
        content.push(column(warning.into_iter().collect()).into());

        content.push(sep());
        content.push(self.section(
            Section::Performance,
            widget::icon(embedded_icon(PERFORMANCE_ICON)).size(20).into(),
            "Performance",
            performance_summary(state),
            || self.performance_body(state),
        ));

        if state.keyboard.is_some() || state.logo_on.is_some() {
            content.push(sep());
            content.push(self.section(
                Section::Lighting,
                themed_icon("keyboard-brightness-symbolic"),
                "Lighting",
                lighting_summary(state),
                || self.lighting_body(state),
            ));
        }

        if let Some(on) = state.battery_limit_on {
            content.push(sep());
            let summary = if on {
                format!("Stops charging at {} %", state.battery_limit)
            } else {
                "Charges to 100 %".into()
            };
            content.push(self.section(
                Section::Battery,
                themed_icon("battery-level-80-charging-symbolic"),
                "Battery",
                summary,
                || self.battery_body(state, on),
            ));
        }

        content.push(sep());
        let info = &state.info;
        content.push(self.section(
            Section::Device,
            themed_icon("dialog-information-symbolic"),
            "Device",
            device_summary(info),
            || {
                [
                    ("Model", info.model.clone()),
                    ("Part number", info.sku.clone()),
                    ("BIOS", info.bios.clone()),
                    ("EC firmware", info.firmware.map(|(a, b)| format!("{a}.{b}"))),
                    ("USB ID", (info.pid != 0).then(|| format!("1532:{:04x}", info.pid))),
                    ("Interface", Some(info.path.display().to_string())),
                ]
                .into_iter()
                .filter_map(|(key, value)| value.map(|v| info_row(key, v)))
                .collect()
            },
        ));

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
            expanded: Some(Section::Performance),
            seg: Segments::new(),
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
        let task = self.handle(message);
        if let Some(s) = &self.state {
            self.seg.sync(s);
        }
        task
    }

    fn view(&self) -> Element<'_, Message> {
        let btn = self
            .core
            .applet
            .icon_button_from_handle(app_icon())
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

// ---- summaries shown on collapsed section headers ---------------------------

fn level_name(level: Option<u8>) -> &'static str {
    level.and_then(|l| LEVELS.get(l as usize).copied()).unwrap_or("?")
}

fn performance_summary(s: &State) -> String {
    let mode = match s.power_mode {
        Some(POWER_CUSTOM) => format!(
            "CPU {} · GPU {}",
            level_name(s.cpu_level),
            level_name(s.gpu_level)
        ),
        Some(POWER_BALANCED) => "Balanced".into(),
        Some(other) => format!("Mode {other}"),
        None => "Fans".into(),
    };
    let fans = match (s.fan_manual, s.fan_rpm) {
        (true, Some(rpm)) => format!("fans {rpm} rpm"),
        (true, None) => "manual fans".into(),
        (false, _) => "auto fans".into(),
    };
    format!("{mode} · {fans}")
}

fn lighting_summary(s: &State) -> String {
    let mut parts = Vec::new();
    if let Some(kb) = &s.keyboard {
        parts.push(format!("Keyboard {}", kb.mode.label().to_lowercase()));
    }
    match s.logo_on {
        Some(true) if s.logo_breathing => parts.push("logo breathing".into()),
        Some(true) => parts.push("logo on".into()),
        Some(false) => parts.push("logo off".into()),
        None => {}
    }
    parts.join(" · ")
}

fn device_summary(info: &DeviceInfo) -> String {
    let mut parts = Vec::new();
    if let Some(sku) = &info.sku {
        parts.push(sku.clone());
    }
    if let Some((a, b)) = info.firmware {
        parts.push(format!("firmware {a}.{b}"));
    }
    if parts.is_empty() {
        parts.push(info.path.display().to_string());
    }
    parts.join(" · ")
}

// ---- small view helpers ---------------------------------------------------

type Col<'a> = widget::Column<'a, Message, cosmic::Theme, cosmic::Renderer>;

fn column(children: Vec<Element<'_, Message>>) -> Col<'_> {
    widget::column::with_children(children)
}

fn sep<'a>() -> Element<'a, Message> {
    padded_control(divider::horizontal::default())
        .padding([4, 12])
        .into()
}

/// Clickable row that opens or closes a section, with a one-line summary of
/// its current settings so a closed section is still informative.
fn section_header<'a>(
    section: Section,
    open: bool,
    icon: Element<'a, Message>,
    title: &'static str,
    summary: String,
) -> Element<'a, Message> {
    let chevron = if open { "go-up-symbolic" } else { "go-down-symbolic" };
    menu_button(
        widget::row::with_children(vec![
            icon,
            widget::column::with_children(vec![
                text::body(title).into(),
                text::caption(summary).into(),
            ])
            .width(Length::Fill)
            .into(),
            icon::from_name(chevron).size(16).symbolic(true).into(),
        ])
        .spacing(12)
        .align_y(Alignment::Center),
    )
    .on_press(Message::Expand(section))
    .into()
}

/// A control with a small caption above it, so the control gets the full width.
fn labeled<'a>(label: &'a str, control: Element<'a, Message>) -> Element<'a, Message> {
    padded_control(column(vec![text::caption(label).into(), control]).spacing(4)).into()
}

fn toggle_row<'a>(label: &'a str, on: bool, msg: fn(bool) -> Message) -> Element<'a, Message> {
    padded_control(
        widget::row::with_children(vec![
            text::body(label).width(Length::Fill).into(),
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

fn info_row(key: &str, value: String) -> Element<'_, Message> {
    padded_control(
        widget::row::with_children(vec![
            text::body(key).width(Length::Fill).into(),
            text::body(value).into(),
        ])
        .spacing(12),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_clears_a_row_that_lacks_the_value() {
        // Keyboard effects span two rows; picking one in the bottom row must
        // un-highlight the top row.
        let mut top = model(KbMode::ALL[..3].iter().map(|&m| (m.label(), m)));
        select(&mut top, Some(KbMode::Static));
        assert_eq!(top.active_data::<KbMode>(), Some(&KbMode::Static));
        select(&mut top, Some(KbMode::Wave));
        assert_eq!(top.active_data::<KbMode>(), None);
    }
}
