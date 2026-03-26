//! Settings GUI built with iced.
//!
//! Launch with: `rotocontrol --settings`
//! The tray "Settings" menu item forks the process and calls gui::run() in the child.

use crate::config::{self, Config, StreamOverride, UserOverride};
use crate::pipewire;

use iced::{
    widget::{
        button, checkbox, column, container, horizontal_rule, pick_list, row, scrollable, text,
        text_input, Column, Row,
    },
    Color, Element, Length, Padding, Task, Theme,
};

// ---- Colour palette -------------------------------------------------------

/// (device_index, RGB) – matches COLOR_POOL in main.rs.
const PALETTE: &[(Option<u8>, (u8, u8, u8))] = &[
    (None,     (0x50, 0x50, 0x50)), // Random (grey placeholder)
    (Some(0),  (0xFF, 0x94, 0xA6)), // Pink
    (Some(1),  (0xFF, 0xA5, 0x29)), // Orange
    (Some(3),  (0xF7, 0xF4, 0x7C)), // Yellow
    (Some(5),  (0x1A, 0xFF, 0x2F)), // Green
    (Some(7),  (0x5C, 0xFF, 0xE8)), // Teal
    (Some(9),  (0x54, 0x80, 0xE4)), // Blue
    (Some(11), (0xD8, 0x6C, 0xE4)), // Purple
    (Some(14), (0xFF, 0x36, 0x36)), // Red
];

fn palette_color(idx: Option<u8>) -> Color {
    let &(_, (r, g, b)) = PALETTE.iter().find(|(i, _)| *i == idx).unwrap_or(&PALETTE[0]);
    Color::from_rgb8(r, g, b)
}

/// A small coloured square button. White border = selected.
fn swatch<'a>(idx: Option<u8>, selected: bool, msg: Message) -> Element<'a, Message> {
    let bg = palette_color(idx);
    let border_col = if selected { Color::WHITE } else { Color::from_rgb8(0x28, 0x28, 0x28) };
    let border_w   = if selected { 2.0_f32 } else { 1.0 };
    let label = if idx.is_none() { "?" } else { "" };
    button(text(label).size(10).color(Color::from_rgb8(0x10, 0x10, 0x10)))
        .style(move |_theme: &Theme, _| button::Style {
            background: Some(iced::Background::Color(bg)),
            border: iced::Border { color: border_col, width: border_w, radius: 3.0.into() },
            text_color: Color::from_rgb8(0x10, 0x10, 0x10),
            shadow: iced::Shadow::default(),
        })
        .width(22)
        .height(22)
        .on_press(msg)
        .into()
}

/// A row of colour swatches (Random + 8 named colours).
fn color_picker_row<'a, F>(selected: Option<u8>, make: F) -> Element<'a, Message>
where
    F: Fn(Option<u8>) -> Message + 'a,
{
    let swatches: Vec<Element<Message>> = PALETTE.iter()
        .map(|(idx, _)| swatch(*idx, selected == *idx, make(*idx)))
        .collect();
    Row::with_children(swatches).spacing(3).into()
}

// ---- State ----------------------------------------------------------------

#[derive(Clone, PartialEq, Debug)]
enum Tab { PipeWire, Discord, TeamSpeak }

#[derive(Clone, Debug)]
struct StreamRow {
    binary: String,
    app_id: String,
    raw_name: String,
    name_input: String,
    color: Option<u8>,
    accent_color: Option<u8>,
    ignored: bool,
    is_live: bool,
    /// If the stream currently has bottom-text (media_name), show accent picker.
    has_media_name: bool,
    /// Whether to use playerctl for track title (mpris_player configured).
    use_mpris: bool,
    /// Preserved custom mpris player name (None = use binary name as default).
    mpris_player: Option<String>,
    /// PID of the process (from live PipeWire data).
    pid: Option<u32>,
    /// Key of the parent stream (binary/app_id) if this is a child process.
    parent_key: Option<String>,
    /// Whether to auto-link child process streams to this parent.
    auto_link_children: bool,
}

#[derive(Clone, Debug)]
struct UserRow {
    name_input: String,
    color: Option<u8>,
    priority_input: String,
}
impl UserRow {
    fn empty() -> Self { UserRow { name_input: String::new(), color: None, priority_input: String::new() } }
    fn from_override(ov: &UserOverride) -> Self {
        UserRow {
            name_input: ov.name.clone(),
            color: ov.color,
            priority_input: ov.priority.map(|p| p.to_string()).unwrap_or_default(),
        }
    }
}

struct App {
    tab: Tab,
    config: Config,
    streams: Vec<StreamRow>,
    discord_users: Vec<UserRow>,
    ts3_users: Vec<UserRow>,
    discord_active: Vec<String>,
    ts3_active: Vec<String>,
    discord_add_sel: Option<String>,
    ts3_add_sel: Option<String>,
    status: String,
    hide_offline: bool,
    /// Keys of parent streams whose child tree is expanded.
    expanded_streams: std::collections::HashSet<String>,
}

// ---- Messages -------------------------------------------------------------

#[derive(Clone, Debug)]
enum Message {
    TabSelected(Tab),
    // PipeWire streams
    StreamNameChanged(usize, String),
    StreamColorSelected(usize, Option<u8>),
    StreamAccentColorSelected(usize, Option<u8>),
    StreamIgnoreToggled(usize, bool),
    StreamMprisToggled(usize, bool),
    StreamAutoLinkToggled(usize, bool),
    ToggleStreamExpand(String),
    RemoveStream(usize),
    SetHideOffline(bool),
    Refresh,
    // Discord users
    AddDiscordUser,
    DiscordActiveUserSelected(String),
    AddDiscordUserFromActive,
    DiscordUserNameChanged(usize, String),
    DiscordUserColorSelected(usize, Option<u8>),
    DiscordUserPriorityChanged(usize, String),
    RemoveDiscordUser(usize),
    // TS3 users
    AddTs3User,
    Ts3ActiveUserSelected(String),
    AddTs3UserFromActive,
    Ts3UserNameChanged(usize, String),
    Ts3UserColorSelected(usize, Option<u8>),
    Ts3UserPriorityChanged(usize, String),
    RemoveTs3User(usize),
    // Enable toggles
    SetPipewireEnabled(bool),
    SetDiscordEnabled(bool),
    SetTeamspeakEnabled(bool),
    // Save
    Save,
}

// ---- Init -----------------------------------------------------------------

pub fn run() -> iced::Result {
    iced::application("Proto-Control Settings", update, view)
        .window_size((780.0, 580.0))
        .scale_factor(|_| 0.8)
        .theme(|_| Theme::Dark)
        .run_with(init)
}

fn init() -> (App, Task<Message>) {
    let config = Config::load();
    let pw_streams = pipewire::list_streams(&config).unwrap_or_default();
    let streams = build_stream_rows(&config, &pw_streams);
    let discord_users = config.discord_users.iter().map(UserRow::from_override).collect();
    let ts3_users = config.teamspeak_users.iter().map(UserRow::from_override).collect();
    let discord_active = read_member_state("discord_members.json");
    let ts3_active = read_member_state("ts3_members.json");

    (App {
        tab: Tab::PipeWire,
        config,
        streams,
        discord_users,
        ts3_users,
        discord_active,
        ts3_active,
        discord_add_sel: None,
        ts3_add_sel: None,
        status: String::new(),
        hide_offline: false,
        expanded_streams: std::collections::HashSet::new(),
    }, Task::none())
}

fn read_member_state(filename: &str) -> Vec<String> {
    config::state_path(filename)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Unique key for a stream row. For sub-streams that share a binary but have
/// different application names, includes the raw_name to disambiguate.
fn stream_row_key(row: &StreamRow) -> String {
    if !row.binary.is_empty() {
        // Include raw_name to distinguish sub-streams from the same binary
        format!("{}:{}", row.binary, row.raw_name)
    } else if !row.app_id.is_empty() {
        row.app_id.clone()
    } else {
        row.raw_name.clone()
    }
}

fn build_stream_rows(config: &Config, pw_streams: &[pipewire::AudioStream]) -> Vec<StreamRow> {
    let mut rows = Vec::new();
    // Dedup by (binary+raw_name) so sub-streams from the same process show separately
    let mut seen = std::collections::HashSet::new();

    for stream in pw_streams {
        let dedup_key = format!("{}:{}", stream.binary, stream.raw_name);
        if dedup_key == ":" { continue; }
        if !seen.insert(dedup_key) { continue; }

        let config_key = if !stream.binary.is_empty() {
            &stream.binary
        } else if !stream.app_id.is_empty() {
            &stream.app_id
        } else {
            &stream.raw_name
        };

        let ov = config.streams.iter().find(|o| {
            o.binary.as_deref() == Some(config_key) || o.app_id.as_deref() == Some(config_key)
                || o.name == *config_key
        });
        // mpris_player: prefer user config, fall back to built-in default
        let resolved = config.resolve(&stream.binary, &stream.app_id, &stream.raw_name);
        let mpris_player = ov.and_then(|o| o.mpris_player.clone()).or(resolved.mpris_player);
        rows.push(StreamRow {
            binary: stream.binary.clone(),
            app_id: stream.app_id.clone(),
            raw_name: stream.raw_name.clone(),
            name_input: ov.map(|o| o.name.clone()).unwrap_or_else(|| stream.raw_name.clone()),
            color: ov.and_then(|o| o.color),
            accent_color: ov.and_then(|o| o.accent_color),
            ignored: ov.map(|o| o.ignored).unwrap_or(false),
            is_live: true,
            has_media_name: stream.media_name.is_some(),
            use_mpris: mpris_player.is_some(),
            mpris_player,
            pid: stream.pid,
            parent_key: None, // filled in below
            auto_link_children: ov.map(|o| o.auto_link_children).unwrap_or(false),
        });
    }

    // Saved overrides for apps not currently running
    let seen_keys: std::collections::HashSet<String> = rows.iter()
        .map(|r| stream_row_key(r))
        .collect();
    for ov in &config.streams {
        let key = ov.binary.as_deref()
            .or(ov.app_id.as_deref())
            .unwrap_or(&ov.name)
            .to_string();
        if key.is_empty() || seen_keys.contains(&key) { continue; }
        rows.push(StreamRow {
            binary: ov.binary.clone().unwrap_or_default(),
            app_id: ov.app_id.clone().unwrap_or_default(),
            raw_name: ov.name.clone(),
            name_input: ov.name.clone(),
            color: ov.color,
            accent_color: ov.accent_color,
            ignored: ov.ignored,
            is_live: false,
            has_media_name: false,
            use_mpris: ov.mpris_player.is_some(),
            mpris_player: ov.mpris_player.clone(),
            pid: None,
            parent_key: None,
            auto_link_children: ov.auto_link_children,
        });
    }

    // Detect sub-streams: streams sharing a PID or with a parent/child PID
    // relationship are grouped. The first stream seen for a PID is the "parent".
    // Build PID -> first row key mapping.
    let mut pid_to_parent_key: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
    for row in &rows {
        if let Some(pid) = row.pid {
            pid_to_parent_key.entry(pid).or_insert_with(|| stream_row_key(row));
        }
    }

    for row in &mut rows {
        if let Some(pid) = row.pid {
            let my_key = stream_row_key(row);
            // Same-PID grouping: if another row with this PID was seen first, it's the parent
            if let Some(parent_key) = pid_to_parent_key.get(&pid) {
                if *parent_key != my_key {
                    row.parent_key = Some(parent_key.clone());
                    continue;
                }
            }
            // Process ancestry: check if our PID is a child of another stream's PID
            for (&other_pid, parent_key) in &pid_to_parent_key {
                if other_pid == pid { continue; }
                if pipewire::is_descendant(pid, other_pid) {
                    row.parent_key = Some(parent_key.clone());
                    break;
                }
            }
        }
    }

    rows
}

// ---- Update ---------------------------------------------------------------

fn update(app: &mut App, msg: Message) -> Task<Message> {
    match msg {
        Message::TabSelected(t) => { app.tab = t; }

        // PipeWire
        Message::SetHideOffline(v) => { app.hide_offline = v; }
        Message::StreamNameChanged(i, s) => { if let Some(r) = app.streams.get_mut(i) { r.name_input = s; } }
        Message::StreamColorSelected(i, c) => { if let Some(r) = app.streams.get_mut(i) { r.color = c; } }
        Message::StreamAccentColorSelected(i, c) => { if let Some(r) = app.streams.get_mut(i) { r.accent_color = c; } }
        Message::StreamIgnoreToggled(i, v) => { if let Some(r) = app.streams.get_mut(i) { r.ignored = v; } }
        Message::StreamMprisToggled(i, v) => { if let Some(r) = app.streams.get_mut(i) { r.use_mpris = v; } }
        Message::StreamAutoLinkToggled(i, v) => { if let Some(r) = app.streams.get_mut(i) { r.auto_link_children = v; } }
        Message::ToggleStreamExpand(key) => {
            if !app.expanded_streams.remove(&key) {
                app.expanded_streams.insert(key);
            }
        }
        Message::RemoveStream(i) => { if i < app.streams.len() { app.streams.remove(i); } }
        Message::Refresh => {
            let pw = pipewire::list_streams(&app.config).unwrap_or_default();
            app.streams = build_stream_rows(&app.config, &pw);
            app.discord_active = read_member_state("discord_members.json");
            app.ts3_active = read_member_state("ts3_members.json");
            app.status = String::new();
        }

        // Discord
        Message::AddDiscordUser => { app.discord_users.push(UserRow::empty()); }
        Message::DiscordActiveUserSelected(s) => { app.discord_add_sel = Some(s); }
        Message::AddDiscordUserFromActive => {
            if let Some(nick) = app.discord_add_sel.take() {
                if !app.discord_users.iter().any(|r| r.name_input == nick) {
                    let mut row = UserRow::empty();
                    row.name_input = nick;
                    app.discord_users.push(row);
                }
            }
        }
        Message::DiscordUserNameChanged(i, s) => { if let Some(r) = app.discord_users.get_mut(i) { r.name_input = s; } }
        Message::DiscordUserColorSelected(i, c) => { if let Some(r) = app.discord_users.get_mut(i) { r.color = c; } }
        Message::DiscordUserPriorityChanged(i, s) => { if let Some(r) = app.discord_users.get_mut(i) { r.priority_input = s; } }
        Message::RemoveDiscordUser(i) => { if i < app.discord_users.len() { app.discord_users.remove(i); } }

        // TS3
        Message::AddTs3User => { app.ts3_users.push(UserRow::empty()); }
        Message::Ts3ActiveUserSelected(s) => { app.ts3_add_sel = Some(s); }
        Message::AddTs3UserFromActive => {
            if let Some(nick) = app.ts3_add_sel.take() {
                if !app.ts3_users.iter().any(|r| r.name_input == nick) {
                    let mut row = UserRow::empty();
                    row.name_input = nick;
                    app.ts3_users.push(row);
                }
            }
        }
        Message::Ts3UserNameChanged(i, s) => { if let Some(r) = app.ts3_users.get_mut(i) { r.name_input = s; } }
        Message::Ts3UserColorSelected(i, c) => { if let Some(r) = app.ts3_users.get_mut(i) { r.color = c; } }
        Message::Ts3UserPriorityChanged(i, s) => { if let Some(r) = app.ts3_users.get_mut(i) { r.priority_input = s; } }
        Message::RemoveTs3User(i) => { if i < app.ts3_users.len() { app.ts3_users.remove(i); } }

        // Enable toggles
        Message::SetPipewireEnabled(v) => { app.config.pipewire_enabled = v; }
        Message::SetDiscordEnabled(v) => {
            if let Some(ref mut dc) = app.config.discord { dc.enabled = v; }
        }
        Message::SetTeamspeakEnabled(v) => {
            if let Some(ref mut ts) = app.config.teamspeak { ts.enabled = v; }
        }

        // Save
        Message::Save => {
            app.config.streams.clear();

            for row in &app.streams {
                let has_override = row.name_input != row.raw_name
                    || row.color.is_some()
                    || row.accent_color.is_some()
                    || row.ignored
                    || row.use_mpris
                    || row.auto_link_children;
                if !has_override { continue; }

                let binary = if row.binary.is_empty() { None } else { Some(row.binary.clone()) };
                let app_id = if row.app_id.is_empty() { None } else { Some(row.app_id.clone()) };
                // When use_mpris is set: keep custom mpris_player name if present,
                // otherwise default to binary name (works for most playerctl players).
                let mpris_player = if row.use_mpris {
                    Some(row.mpris_player.clone()
                        .unwrap_or_else(|| row.binary.clone()))
                } else {
                    None
                };

                app.config.streams.push(StreamOverride {
                    binary,
                    app_id,
                    name: row.name_input.clone(),
                    mpris_player,
                    color: row.color,
                    accent_color: row.accent_color,
                    ignored: row.ignored,
                    auto_link_children: row.auto_link_children,
                });
            }

            app.config.discord_users = app.discord_users.iter()
                .filter(|r| !r.name_input.trim().is_empty())
                .map(|r| UserOverride {
                    name: r.name_input.trim().to_string(),
                    color: r.color,
                    priority: r.priority_input.parse().ok(),
                })
                .collect();

            app.config.teamspeak_users = app.ts3_users.iter()
                .filter(|r| !r.name_input.trim().is_empty())
                .map(|r| UserOverride {
                    name: r.name_input.trim().to_string(),
                    color: r.color,
                    priority: r.priority_input.parse().ok(),
                })
                .collect();

            match app.config.save() {
                Ok(()) => app.status = "Saved! Changes apply within ~1 second.".into(),
                Err(e) => app.status = format!("Save error: {}", e),
            }
        }
    }
    Task::none()
}

// ---- View -----------------------------------------------------------------

fn view(app: &App) -> Element<'_, Message> {
    let tab_bar = row![
        tab_btn("PipeWire", Tab::PipeWire, &app.tab),
        tab_btn("Discord",  Tab::Discord,  &app.tab),
        tab_btn("TeamSpeak", Tab::TeamSpeak, &app.tab),
    ]
    .spacing(4)
    .padding(Padding { top: 8.0, right: 8.0, bottom: 0.0, left: 8.0 });

    let content: Element<Message> = match app.tab {
        Tab::PipeWire  => view_pipewire(app),
        Tab::Discord   => view_discord(app),
        Tab::TeamSpeak => view_teamspeak(app),
    };

    let bottom = row![
        text(&app.status).size(12),
        iced::widget::Space::with_width(Length::Fill),
        button("Save").on_press(Message::Save),
    ]
    .spacing(8)
    .padding(8)
    .align_y(iced::alignment::Vertical::Center);

    column![
        tab_bar,
        horizontal_rule(1),
        scrollable(content).height(Length::Fill),
        horizontal_rule(1),
        bottom,
    ]
    .height(Length::Fill)
    .into()
}

fn tab_btn<'a>(label: &'static str, tab: Tab, current: &Tab) -> Element<'a, Message> {
    let active = &tab == current;
    let b = button(text(label)).on_press(Message::TabSelected(tab));
    if active { b.style(button::primary).into() } else { b.style(button::secondary).into() }
}

// ---- PipeWire tab ---------------------------------------------------------

fn view_pipewire(app: &App) -> Element<'_, Message> {
    let header = row![
        text("PipeWire Streams").size(16),
        iced::widget::Space::with_width(Length::Fill),
        checkbox("Hide offline", app.hide_offline)
            .on_toggle(Message::SetHideOffline)
            .size(14)
            .text_size(12),
        checkbox("Enabled", app.config.pipewire_enabled)
            .on_toggle(Message::SetPipewireEnabled)
            .size(14)
            .text_size(12),
        button("Refresh").on_press(Message::Refresh),
    ]
    .spacing(8)
    .padding(Padding { top: 8.0, right: 8.0, bottom: 4.0, left: 8.0 })
    .align_y(iced::alignment::Vertical::Center);

    let mut children: Vec<Element<Message>> = vec![header.into()];

    if app.streams.is_empty() {
        children.push(
            text("No streams found. Play some audio and click Refresh.").into()
        );
    }

    // Check which streams have children (for showing auto-link toggle)
    let has_children: std::collections::HashSet<String> = app.streams.iter()
        .filter_map(|s| s.parent_key.clone())
        .collect();

    // Render root streams first, then their children indented below
    for (i, stream) in app.streams.iter().enumerate() {
        if stream.parent_key.is_some() { continue; } // rendered under parent
        if app.hide_offline && !stream.is_live { continue; }
        let key = stream_row_key(stream);
        let is_parent = has_children.contains(&key);
        let is_expanded = app.expanded_streams.contains(&key);
        children.push(stream_card(i, stream, false, is_parent, is_expanded, false));

        // Render children of this stream (only when expanded)
        if is_parent && is_expanded {
            for (j, child) in app.streams.iter().enumerate() {
                if child.parent_key.as_deref() == Some(&key) {
                    if app.hide_offline && !child.is_live { continue; }
                    children.push(stream_card(j, child, true, false, false, stream.auto_link_children));
                }
            }
        }
    }

    Column::with_children(children).spacing(6).padding(8).width(Length::Fill).into()
}

fn stream_card(i: usize, stream: &StreamRow, is_child: bool, is_parent: bool, is_expanded: bool, is_linked: bool) -> Element<'_, Message> {
    // Display label: show raw_name for readability, fall back to binary/app_id
    let display_label = if !stream.raw_name.is_empty() { &stream.raw_name }
        else if !stream.binary.is_empty() { &stream.binary }
        else if !stream.app_id.is_empty() { &stream.app_id }
        else { "?" };
    let (status_label, status_color) = if stream.is_live {
        ("live", Color::from_rgb8(0x4C, 0xAF, 0x50))
    } else {
        ("offline", Color::from_rgb8(0x80, 0x80, 0x80))
    };

    let mut header_children: Vec<Element<Message>> = Vec::new();
    // Expand/collapse toggle for parent streams
    if is_parent {
        let arrow = if is_expanded { "▼" } else { "▶" };
        let toggle_key = stream_row_key(stream);
        header_children.push(
            button(text(arrow).size(11))
                .on_press(Message::ToggleStreamExpand(toggle_key))
                .style(button::text)
                .padding(Padding { top: 0.0, right: 2.0, bottom: 0.0, left: 0.0 })
                .into()
        );
    } else if is_child {
        header_children.push(text("  └").size(13).color(Color::from_rgb8(0x60, 0x60, 0x60)).into());
    }
    header_children.push(text(display_label).size(13).into());
    header_children.push(text(status_label).size(11).color(status_color).into());
    if is_linked {
        header_children.push(
            text("(linked)").size(11).color(Color::from_rgb8(0x5C, 0xFF, 0xE8)).into()
        );
    }
    let header = Row::with_children(header_children)
        .spacing(6)
        .align_y(iced::alignment::Vertical::Center);

    // Top colour row
    let top_color_row = row![
        text("Color:").size(12),
        color_picker_row(stream.color, move |c| Message::StreamColorSelected(i, c)),
    ]
    .spacing(8)
    .align_y(iced::alignment::Vertical::Center);

    // Accent colour row — only shown when stream currently shows bottom text
    let accent_row: Option<Element<Message>> = if stream.is_live && stream.has_media_name {
        Some(row![
            text("Accent:").size(12),
            color_picker_row(stream.accent_color, move |c| Message::StreamAccentColorSelected(i, c)),
        ]
        .spacing(8)
        .align_y(iced::alignment::Vertical::Center)
        .into())
    } else if stream.accent_color.is_some() {
        // Offline but has a saved accent — show it so it can be cleared
        Some(row![
            text("Accent:").size(12),
            color_picker_row(stream.accent_color, move |c| Message::StreamAccentColorSelected(i, c)),
        ]
        .spacing(8)
        .align_y(iced::alignment::Vertical::Center)
        .into())
    } else {
        None
    };

    let mut name_row_children: Vec<Element<Message>> = vec![
        text("Name:").size(12).into(),
        text_input("Display name", &stream.name_input)
            .on_input(move |s| Message::StreamNameChanged(i, s))
            .width(160)
            .size(12)
            .into(),
        iced::widget::Space::with_width(Length::Fill).into(),
    ];
    // Show auto-link checkbox for parent streams that have children
    if is_parent {
        name_row_children.push(
            checkbox("Auto-link children", stream.auto_link_children)
                .on_toggle(move |v| Message::StreamAutoLinkToggled(i, v))
                .size(14)
                .text_size(12)
                .into()
        );
    }
    name_row_children.extend([
        checkbox("Track title", stream.use_mpris)
            .on_toggle(move |v| Message::StreamMprisToggled(i, v))
            .size(14)
            .text_size(12)
            .into(),
        checkbox("Ignore", stream.ignored)
            .on_toggle(move |v| Message::StreamIgnoreToggled(i, v))
            .size(14)
            .text_size(12)
            .into(),
        button(text("x").size(11))
            .on_press(Message::RemoveStream(i))
            .style(button::danger)
            .into(),
    ]);
    let name_row = Row::with_children(name_row_children)
        .spacing(8)
        .align_y(iced::alignment::Vertical::Center);

    let mut col_children: Vec<Element<Message>> = vec![
        header.into(),
        name_row.into(),
        top_color_row.into(),
    ];
    if let Some(ar) = accent_row {
        col_children.push(ar);
    }

    let left_pad = if is_child { 24.0 } else { 0.0 };
    let border_color_override = if is_child {
        Color::from_rgb8(0x50, 0x50, 0x60)
    } else {
        Color::from_rgb8(0, 0, 0) // placeholder, overridden below
    };
    let is_child_copy = is_child;

    let card = container(
        Column::with_children(col_children).spacing(5).width(Length::Fill),
    )
    .padding(Padding::new(8.0))
    .width(Length::Fill)
    .style(move |theme: &Theme| {
        let bg = theme.extended_palette().background.weak.color;
        let border_col = if is_child_copy {
            border_color_override
        } else {
            theme.extended_palette().background.strong.color
        };
        container::Style {
            background: Some(iced::Background::Color(bg)),
            border: iced::Border {
                color: border_col,
                width: 1.0,
                radius: 4.0.into(),
            },
            ..Default::default()
        }
    });

    if is_child {
        container(card)
            .padding(Padding { top: 0.0, right: 0.0, bottom: 0.0, left: left_pad })
            .width(Length::Fill)
            .into()
    } else {
        card.into()
    }
}

// ---- Discord tab ----------------------------------------------------------

fn view_discord(app: &App) -> Element<'_, Message> {
    let already_added: std::collections::HashSet<&str> =
        app.discord_users.iter().map(|r| r.name_input.as_str()).collect();

    let available: Vec<String> = app.discord_active.iter()
        .filter(|n| !already_added.contains(n.as_str()))
        .cloned()
        .collect();

    let active_picker: Element<Message> = if available.is_empty() {
        text("No active users (start Discord and join a voice channel)").size(12)
            .color(Color::from_rgb8(0x80, 0x80, 0x80))
            .into()
    } else {
        row![
            text("Active users:").size(12),
            pick_list(available, app.discord_add_sel.clone(), Message::DiscordActiveUserSelected)
                .text_size(12),
            button("Add").on_press(Message::AddDiscordUserFromActive),
        ]
        .spacing(8)
        .align_y(iced::alignment::Vertical::Center)
        .into()
    };

    let discord_enabled = app.config.discord.as_ref().map(|d| d.enabled).unwrap_or(false);
    let enabled_checkbox: Element<Message> = if app.config.discord.is_some() {
        checkbox("Enabled", discord_enabled)
            .on_toggle(Message::SetDiscordEnabled)
            .size(14)
            .text_size(12)
            .into()
    } else {
        text("Add [discord] to config.toml to enable").size(11)
            .color(Color::from_rgb8(0x80, 0x80, 0x80))
            .into()
    };
    let header = row![
        text("Discord User Settings").size(16),
        iced::widget::Space::with_width(Length::Fill),
        enabled_checkbox,
        button("+ Manual").on_press(Message::AddDiscordUser),
    ]
    .spacing(8)
    .padding(Padding { top: 8.0, right: 8.0, bottom: 4.0, left: 8.0 })
    .align_y(iced::alignment::Vertical::Center);

    let help = text("Set a color or priority for each user. Lower priority = appears first on device.")
        .size(12)
        .color(Color::from_rgb8(0xA0, 0xA0, 0xA0));

    let mut children: Vec<Element<Message>> = vec![
        header.into(),
        help.into(),
        active_picker,
    ];

    if app.discord_users.is_empty() {
        children.push(text("No saved user overrides.").size(12).into());
    }
    for (i, user) in app.discord_users.iter().enumerate() {
        children.push(discord_user_row(i, user));
    }

    Column::with_children(children).spacing(6).padding(8).width(Length::Fill).into()
}

fn discord_user_row(i: usize, user: &UserRow) -> Element<'_, Message> {
    row![
        text_input("Username", &user.name_input)
            .on_input(move |s| Message::DiscordUserNameChanged(i, s))
            .width(180)
            .size(13),
        color_picker_row(user.color, move |c| Message::DiscordUserColorSelected(i, c)),
        text("Priority:").size(12),
        text_input("0", &user.priority_input)
            .on_input(move |s| Message::DiscordUserPriorityChanged(i, s))
            .width(55)
            .size(12),
        button(text("x").size(11))
            .on_press(Message::RemoveDiscordUser(i))
            .style(button::danger),
    ]
    .spacing(8)
    .align_y(iced::alignment::Vertical::Center)
    .into()
}

// ---- TeamSpeak tab --------------------------------------------------------

fn view_teamspeak(app: &App) -> Element<'_, Message> {
    let already_added: std::collections::HashSet<&str> =
        app.ts3_users.iter().map(|r| r.name_input.as_str()).collect();

    let available: Vec<String> = app.ts3_active.iter()
        .filter(|n| !already_added.contains(n.as_str()))
        .cloned()
        .collect();

    let active_picker: Element<Message> = if available.is_empty() {
        text("No active users (join a TeamSpeak channel)").size(12)
            .color(Color::from_rgb8(0x80, 0x80, 0x80))
            .into()
    } else {
        row![
            text("Active users:").size(12),
            pick_list(available, app.ts3_add_sel.clone(), Message::Ts3ActiveUserSelected)
                .text_size(12),
            button("Add").on_press(Message::AddTs3UserFromActive),
        ]
        .spacing(8)
        .align_y(iced::alignment::Vertical::Center)
        .into()
    };

    let ts3_enabled = app.config.teamspeak.as_ref().map(|t| t.enabled).unwrap_or(false);
    let enabled_checkbox: Element<Message> = if app.config.teamspeak.is_some() {
        checkbox("Enabled", ts3_enabled)
            .on_toggle(Message::SetTeamspeakEnabled)
            .size(14)
            .text_size(12)
            .into()
    } else {
        text("Add [teamspeak] to config.toml to enable").size(11)
            .color(Color::from_rgb8(0x80, 0x80, 0x80))
            .into()
    };
    let header = row![
        text("TeamSpeak User Settings").size(16),
        iced::widget::Space::with_width(Length::Fill),
        enabled_checkbox,
        button("+ Manual").on_press(Message::AddTs3User),
    ]
    .spacing(8)
    .padding(Padding { top: 8.0, right: 8.0, bottom: 4.0, left: 8.0 })
    .align_y(iced::alignment::Vertical::Center);

    let help = text("Set a color or priority for each user. Lower priority = appears first on device.")
        .size(12)
        .color(Color::from_rgb8(0xA0, 0xA0, 0xA0));

    let mut children: Vec<Element<Message>> = vec![
        header.into(),
        help.into(),
        active_picker,
    ];

    if app.ts3_users.is_empty() {
        children.push(text("No saved user overrides.").size(12).into());
    }
    for (i, user) in app.ts3_users.iter().enumerate() {
        children.push(ts3_user_row(i, user));
    }

    Column::with_children(children).spacing(6).padding(8).width(Length::Fill).into()
}

fn ts3_user_row(i: usize, user: &UserRow) -> Element<'_, Message> {
    row![
        text_input("Nickname", &user.name_input)
            .on_input(move |s| Message::Ts3UserNameChanged(i, s))
            .width(180)
            .size(13),
        color_picker_row(user.color, move |c| Message::Ts3UserColorSelected(i, c)),
        text("Priority:").size(12),
        text_input("0", &user.priority_input)
            .on_input(move |s| Message::Ts3UserPriorityChanged(i, s))
            .width(55)
            .size(12),
        button(text("x").size(11))
            .on_press(Message::RemoveTs3User(i))
            .style(button::danger),
    ]
    .spacing(8)
    .align_y(iced::alignment::Vertical::Center)
    .into()
}
