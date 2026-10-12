//! The settings window: one native, decorated window next to the Workspace windows
//! (it does not shrink with them). Right-clicking a Module opens it on that
//! instance. Tabs hold everything the instance, its window, the workspace and the
//! theme offer; every change goes through `WorkspaceAction`.

use std::time::Duration;

use eframe::egui::{self, Pos2, Vec2, ViewportCommand, pos2, vec2};

use super::{
    ColorScope, ModuleInstanceId, Side, WindowId, Workspace, WorkspaceAction,
    desktop::DesktopProbe,
    window::{ControlKey, Controls, channel_options, color32},
};
use crate::{
    audio::SourceState,
    dataflow::{ChannelSelection, FftConfig, Keep, NodeRef, NodeSpec, OutputId},
    persistence::{same_name, valid_name},
    render::Color,
    settings::{
        DataSettings, FFT_HOP_DIVISORS, FFT_WINDOWS, NodeEntry, NodeParam, SettingKind,
        SettingValue, node_label, with_fft_window,
    },
    theme::{ColorRole, Preset, RoleOverrides, TEXT_SIZE_RANGE},
};

/// Logical size the settings window opens with.
const SETTINGS_SIZE: Vec2 = vec2(420.0, 600.0);
const SETTINGS_MIN_SIZE: Vec2 = vec2(280.0, 220.0);
/// Physical gap between the source window and the settings window.
const SETTINGS_GAP_PX: f32 = 8.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Tab {
    Module,
    Window,
    Workspace,
    Theme,
}

impl Tab {
    const ALL: [Self; 4] = [Self::Module, Self::Window, Self::Workspace, Self::Theme];

    fn label(self) -> &'static str {
        match self {
            Self::Module => "Module",
            Self::Window => "Window",
            Self::Workspace => "Workspace",
            Self::Theme => "Theme",
        }
    }
}

/// Probe-visible controls of the settings window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SettingsItem {
    Tab(Tab),
    ModuleType(&'static str),
    Style(&'static str),
    ClearVisual,
    /// The instance's channel (analysis Channel node or display channel).
    Channel(ChannelSelection),
    FftWindow {
        node: NodeRef,
        size: usize,
    },
    FftHop {
        node: NodeRef,
        divisor: usize,
    },
    Number {
        node: NodeRef,
        param: NodeParam,
    },
    Keep(OutputId),
    ViewChoice {
        key: &'static str,
        id: &'static str,
    },
    ViewNumber(&'static str),
    Override {
        scope: ColorScope,
        role: ColorRole,
    },
    Swatch {
        scope: ColorScope,
        role: ColorRole,
        index: u8,
    },
    SplitRight,
    SplitDown,
    Duplicate,
    Detach,
    MoveTo,
    CloseModule,
    AlwaysOnTop,
    Maximize,
    Group,
    LeaveGroup,
    CloseWindow,
    AddModule(&'static str),
    Save,
    Name,
    SaveAs,
    NewWorkspace,
    /// Saved workspace by index in the sorted list.
    Switch(u16),
    Reset,
    ConfirmReset,
    CancelReset,
    Pause,
    ClearNotices,
    Quit,
    Preset(Preset),
    TextSize,
    TypeStyle(&'static str),
}

/// Quick colors offered next to the full picker.
pub(crate) const SWATCHES: [Color; 8] = [
    Color::rgb(92, 230, 199),
    Color::rgb(100, 190, 255),
    Color::rgb(165, 90, 245),
    Color::rgb(255, 70, 120),
    Color::rgb(255, 170, 60),
    Color::rgb(250, 230, 90),
    Color::rgb(120, 220, 100),
    Color::rgb(240, 240, 240),
];

/// Roles a Module type style can set.
const TYPE_ROLES: [ColorRole; 9] = [
    ColorRole::Surface,
    ColorRole::Text,
    ColorRole::Muted,
    ColorRole::Guide,
    ColorRole::Primary,
    ColorRole::Secondary,
    ColorRole::Low,
    ColorRole::Mid,
    ColorRole::High,
];

/// A slider value being dragged; committed once on release.
type Pending = Option<(SettingsItem, f32)>;

pub(crate) struct SettingsWindow {
    tab: Tab,
    /// The instance the Module tab edits.
    instance: Option<ModuleInstanceId>,
    /// The window the Window tab edits when no instance is targeted.
    window: Option<WindowId>,
    pending: Pending,
    type_style: Option<&'static str>,
    name: String,
    confirm_reset: bool,
    /// A text field has keyboard focus: Escape belongs to it.
    typing: bool,
    /// Builder inputs stay fixed while the window lives.
    initial_position: Option<Pos2>,
    /// Distinguishes each opening: a reopened window is a new viewport that starts
    /// from the builder, not from egui's memory of the closed one.
    opening: u64,
    placed: bool,
    commands: Vec<ViewportCommand>,
    #[cfg(any(test, feature = "automation"))]
    targets: std::collections::HashMap<ControlKey, super::window::ControlTarget>,
    #[cfg(any(test, feature = "automation"))]
    frame_token: u64,
    #[cfg(any(test, feature = "automation"))]
    pass_token: u64,
}

fn channel_label(index: u16, channel: ChannelSelection) -> String {
    match channel {
        ChannelSelection::Left => "Left".to_owned(),
        ChannelSelection::Right => "Right".to_owned(),
        ChannelSelection::MonoAverage => "Mono".to_owned(),
        ChannelSelection::Discrete(_) => format!("Ch {}", index + 1),
    }
}

fn record(controls: &mut Controls, item: SettingsItem, response: &egui::Response) {
    controls.record(ControlKey::Settings(item), response);
}

/// One slider; returns the value to commit when the drag ends (or on a keyboard
/// change).
#[allow(clippy::too_many_arguments)]
fn number_row(
    ui: &mut egui::Ui,
    label: &str,
    key: SettingsItem,
    current: f32,
    (min, max): (f32, f32),
    unit: &str,
    logarithmic: bool,
    pending: &mut Pending,
    controls: &mut Controls,
) -> Option<f32> {
    let mut value = match pending {
        Some((control, value)) if *control == key => *value,
        _ => current,
    };
    let mut commit = None;
    ui.horizontal(|ui| {
        ui.label(label);
        let response = ui.add(
            egui::Slider::new(&mut value, min..=max)
                .logarithmic(logarithmic)
                .show_value(false),
        );
        record(controls, key, &response);
        ui.label(format!("{value:.1} {unit}"));
        if response.changed() {
            *pending = Some((key, value));
        }
        if response.drag_stopped() || (response.changed() && !response.is_pointer_button_down_on())
        {
            *pending = None;
            if (value - current).abs() > f32::EPSILON {
                commit = Some(value);
            }
        }
    });
    commit
}

fn choice(
    ui: &mut egui::Ui,
    selected: bool,
    label: &str,
    item: SettingsItem,
    controls: &mut Controls,
) -> bool {
    let response = ui.selectable_label(selected, label);
    record(controls, item, &response);
    response.clicked() && !selected
}

fn button(
    ui: &mut egui::Ui,
    enabled: bool,
    label: &str,
    item: SettingsItem,
    controls: &mut Controls,
) -> bool {
    let response = ui.add_enabled(enabled, egui::Button::new(label));
    record(controls, item, &response);
    response.clicked()
}

fn color_row(
    ui: &mut egui::Ui,
    scope: ColorScope,
    role: ColorRole,
    current: Color,
    overridden: bool,
    controls: &mut Controls,
    actions: &mut Vec<WorkspaceAction>,
) {
    ui.horizontal(|ui| {
        let mut on = overridden;
        let response = ui.checkbox(&mut on, role.label());
        record(controls, SettingsItem::Override { scope, role }, &response);
        if response.changed() {
            actions.push(WorkspaceAction::SetColor {
                scope,
                role,
                color: on.then_some(current),
            });
        }
        if !overridden {
            let (rect, _) = ui.allocate_exact_size(vec2(14.0, 14.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 2.0, color32(current));
            return;
        }
        for (index, swatch) in SWATCHES.iter().enumerate() {
            let response = ui.add(
                egui::Button::new("")
                    .fill(color32(*swatch))
                    .min_size(vec2(14.0, 14.0)),
            );
            record(
                controls,
                SettingsItem::Swatch {
                    scope,
                    role,
                    index: index as u8,
                },
                &response,
            );
            if response.clicked() {
                actions.push(WorkspaceAction::SetColor {
                    scope,
                    role,
                    color: Some(*swatch),
                });
            }
        }
        let mut picked = color32(current);
        if egui::color_picker::color_edit_button_srgba(
            ui,
            &mut picked,
            egui::color_picker::Alpha::OnlyBlend,
        )
        .changed()
        {
            let [red, green, blue, alpha] = picked.to_srgba_unmultiplied();
            actions.push(WorkspaceAction::SetColor {
                scope,
                role,
                color: Some(Color::rgba(red, green, blue, alpha)),
            });
        }
    });
}

impl SettingsWindow {
    fn new(initial_position: Option<Pos2>, opening: u64) -> Self {
        Self {
            tab: Tab::Module,
            instance: None,
            window: None,
            pending: None,
            type_style: None,
            name: String::new(),
            confirm_reset: false,
            typing: false,
            initial_position,
            opening,
            placed: false,
            commands: Vec::new(),
            #[cfg(any(test, feature = "automation"))]
            targets: std::collections::HashMap::new(),
            #[cfg(any(test, feature = "automation"))]
            frame_token: 0,
            #[cfg(any(test, feature = "automation"))]
            pass_token: 0,
        }
    }

    /// A replaced instance keeps the Module tab on its successor.
    pub(super) fn follow_replacement(&mut self, old: ModuleInstanceId, new: ModuleInstanceId) {
        if self.instance == Some(old) {
            self.instance = Some(new);
        }
    }

    /// The workspace was replaced (switch, new, reset): nothing is targeted.
    pub(super) fn forget_targets(&mut self) {
        self.instance = None;
        self.window = None;
        self.pending = None;
        self.confirm_reset = false;
        if matches!(self.tab, Tab::Module | Tab::Window) {
            self.tab = Tab::Workspace;
        }
    }
}

impl Workspace {
    /// Viewport of the open settings window.
    pub(crate) fn settings_viewport_id(&self) -> Option<egui::ViewportId> {
        let settings = self.settings.as_ref()?;
        Some(egui::ViewportId::from_hash_of((
            "taffymeters.settings",
            settings.opening,
        )))
    }

    /// Builder of the open settings window.
    pub(crate) fn settings_builder(&self) -> Option<egui::ViewportBuilder> {
        let settings = self.settings.as_ref()?;
        let mut builder = egui::ViewportBuilder::default()
            .with_title("Taffymeters settings")
            .with_inner_size(SETTINGS_SIZE)
            .with_min_inner_size(SETTINGS_MIN_SIZE)
            .with_always_on_top();
        if let Some(position) = settings.initial_position {
            builder = builder.with_position(position);
        }
        Some(builder)
    }

    #[cfg(feature = "automation")]
    pub(crate) fn settings_open(&self) -> bool {
        self.settings.is_some()
    }

    #[cfg(feature = "automation")]
    pub(crate) fn settings_targets(
        &self,
    ) -> std::collections::HashMap<ControlKey, super::window::ControlTarget> {
        self.settings
            .as_ref()
            .map(|settings| settings.targets.clone())
            .unwrap_or_default()
    }

    /// Show the settings window on `instance` (or on `window_id` without one).
    pub(super) fn open_settings(
        &mut self,
        window_id: WindowId,
        instance: Option<ModuleInstanceId>,
    ) -> Result<(), super::WorkspaceError> {
        let source = self.window(window_id)?;
        if let Some(instance) = instance
            && self.located(instance)? != window_id
        {
            return Err(super::WorkspaceError::StaleInstance(instance));
        }
        // Beside the source window, in logical points of its scale; a position off
        // every monitor is corrected on the first frame.
        let position = source.desktop.outer.map(|outer| {
            let ppp = source.pixels_per_point;
            pos2((outer.max.x + SETTINGS_GAP_PX) / ppp, outer.min.y / ppp)
        });
        let settings = self.settings.get_or_insert_with(|| {
            self.settings_openings += 1;
            SettingsWindow::new(position, self.settings_openings)
        });
        settings.instance = instance;
        settings.window = Some(window_id);
        settings.tab = if instance.is_some() {
            Tab::Module
        } else {
            Tab::Window
        };
        settings.pending = None;
        settings.confirm_reset = false;
        settings.commands.push(ViewportCommand::Focus);
        Ok(())
    }

    /// Instance and window the tabs edit, when they still exist.
    fn settings_targets_now(&self) -> (Option<ModuleInstanceId>, Option<WindowId>) {
        let Some(settings) = &self.settings else {
            return (None, None);
        };
        let instance = settings
            .instance
            .filter(|id| self.modules.contains_key(id) && self.window_of(*id).is_some());
        let window = instance
            .and_then(|id| self.window_of(id))
            .or(settings.window.filter(|id| self.windows.contains_key(id)));
        (instance, window)
    }

    /// One UI pass of the settings window inside its own viewport.
    pub(crate) fn settings_ui(
        &mut self,
        ui: &mut egui::Ui,
        display_time: Duration,
        probe: &dyn DesktopProbe,
    ) {
        // Actions taken here (e.g. detach) are timed on the app clock, which this
        // pass keeps moving even when every Workspace window is minimized.
        self.tick(display_time);
        let ctx = ui.ctx().clone();
        if ctx.global_style().visuals.dark_mode != self.chrome.dark {
            ctx.set_visuals(if self.chrome.dark {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            });
        }
        // Targets are resolved while the settings state is still in place.
        let (instance, window) = self.settings_targets_now();
        let Some(mut settings) = self.settings.take() else {
            return;
        };
        #[cfg(any(test, feature = "automation"))]
        {
            if ctx.current_pass_index() == 0 {
                settings.frame_token += 1;
            }
            settings.pass_token += 1;
            settings.targets.clear();
        }
        // First placement: a window opened beside a source near the screen edge
        // moves fully into that monitor's work area, title bar and close button
        // included (windows larger than the work area keep their top-left inside).
        if !settings.placed
            && let Some(outer) = ctx.input(|input| input.viewport().outer_rect)
        {
            settings.placed = true;
            let ppp = ctx.pixels_per_point();
            let physical = egui::Rect::from_min_max(outer.min * ppp, outer.max * ppp);
            if let Some(target) = probe
                .work_area(physical.center())
                .and_then(|work| super::desktop::reachable_position(physical, work, f32::INFINITY))
            {
                settings
                    .commands
                    .push(ViewportCommand::OuterPosition(target / ppp));
            }
        }
        let mut actions = Vec::new();
        let escape = ctx.input(|input| input.key_pressed(egui::Key::Escape));
        if escape && !settings.typing && !egui::Popup::is_any_open(&ctx) {
            actions.push(WorkspaceAction::CloseSettings);
        }
        let mut controls = Controls::default();
        let frame = egui::Frame::central_panel(ui.style());
        frame.show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            ui.horizontal(|ui| {
                for tab in Tab::ALL {
                    let response = ui.selectable_label(settings.tab == tab, tab.label());
                    record(&mut controls, SettingsItem::Tab(tab), &response);
                    if response.clicked() {
                        settings.tab = tab;
                        settings.confirm_reset = false;
                    }
                }
            });
            ui.separator();
            settings.typing = false;
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| match settings.tab {
                    Tab::Module => match instance {
                        Some(instance) => self.module_tab(
                            ui,
                            instance,
                            &mut settings.pending,
                            &mut controls,
                            &mut actions,
                        ),
                        None => {
                            ui.label("No module selected. Right-click a module to edit it.");
                        }
                    },
                    Tab::Window => match window {
                        Some(window) => self.window_tab(ui, window, &mut controls, &mut actions),
                        None => {
                            ui.label("No window selected.");
                        }
                    },
                    Tab::Workspace => {
                        self.workspace_tab(ui, &mut settings, &mut controls, &mut actions);
                    }
                    Tab::Theme => self.theme_tab(
                        ui,
                        &mut settings.type_style,
                        &mut settings.pending,
                        &mut controls,
                        &mut actions,
                    ),
                });
        });
        #[cfg(any(test, feature = "automation"))]
        for (key, response) in controls.responses {
            settings.targets.insert(
                key,
                super::window::ControlTarget {
                    rect: response.rect,
                    enabled: response.enabled(),
                    hovered: response.hovered(),
                    frame_token: settings.frame_token,
                    pass_token: settings.pass_token,
                },
            );
        }
        for command in settings.commands.drain(..) {
            ctx.send_viewport_cmd(command);
        }
        self.settings = Some(settings);
        let windows_before = self.windows.len();
        for action in actions {
            let _ = self.apply_action(action);
        }
        if windows_before != self.windows.len() || self.settings.is_none() {
            ctx.request_repaint_of(egui::ViewportId::ROOT);
        }
    }

    fn module_tab(
        &self,
        ui: &mut egui::Ui,
        instance_id: ModuleInstanceId,
        pending: &mut Pending,
        controls: &mut Controls,
        actions: &mut Vec<WorkspaceAction>,
    ) {
        let entry = &self.modules[&instance_id];
        ui.heading(format!("{} #{}", entry.label, instance_id.0));
        ui.horizontal_wrapped(|ui| {
            ui.label("Type");
            for (type_id, label) in self.registry.module_types() {
                if choice(
                    ui,
                    type_id == entry.type_id,
                    label,
                    SettingsItem::ModuleType(type_id),
                    controls,
                ) {
                    actions.push(WorkspaceAction::ReplaceModule {
                        instance_id,
                        type_id,
                    });
                }
            }
        });
        if let Some(file) = &entry.preserved {
            ui.label(format!(
                "Module type {} cannot be used here; its saved configuration is kept unchanged.",
                file.type_id
            ));
        } else {
            self.module_settings(ui, instance_id, pending, controls, actions);
        }
        ui.separator();
        ui.strong("Layout");
        ui.horizontal_wrapped(|ui| {
            let installed = entry.preserved.is_none();
            for (item, side, label) in [
                (SettingsItem::SplitRight, Side::Right, "Split right"),
                (SettingsItem::SplitDown, Side::Below, "Split down"),
            ] {
                if button(
                    ui,
                    installed && self.can_split(instance_id, side),
                    label,
                    item,
                    controls,
                ) {
                    actions.push(WorkspaceAction::SplitModule { instance_id, side });
                }
            }
            if button(
                ui,
                installed && self.can_split(instance_id, Side::Right),
                "Duplicate",
                SettingsItem::Duplicate,
                controls,
            ) {
                actions.push(WorkspaceAction::DuplicateModule { instance_id });
            }
            let leaves = self
                .window_of(instance_id)
                .map_or(0, |window| self.windows[&window].layout.leaves().len());
            if button(
                ui,
                leaves > 1,
                "Detach to new window",
                SettingsItem::Detach,
                controls,
            ) {
                actions.push(WorkspaceAction::DetachModule { instance_id });
            }
            if button(ui, true, "Move to…", SettingsItem::MoveTo, controls) {
                actions.push(WorkspaceAction::BeginMove { instance_id });
            }
            if button(
                ui,
                true,
                "Close module",
                SettingsItem::CloseModule,
                controls,
            ) {
                actions.push(WorkspaceAction::CloseModule { instance_id });
            }
        });
        if self.move_source == Some(instance_id) {
            ui.label("Click an edge target in any window (Esc or right click there cancels).");
        }
    }

    fn module_settings(
        &self,
        ui: &mut egui::Ui,
        instance_id: ModuleInstanceId,
        pending: &mut Pending,
        controls: &mut Controls,
        actions: &mut Vec<WorkspaceAction>,
    ) {
        let entry = &self.modules[&instance_id];
        let module = &entry.module;
        let styles = module.style_options();
        if !styles.is_empty() {
            // While a saved style this build lacks is kept, the fallback is not a
            // choice the user made: every option stays selectable.
            let active = entry
                .preserved_style
                .is_none()
                .then(|| module.active_style())
                .flatten();
            if let Some(kept) = &entry.preserved_style {
                ui.label(format!(
                    "Saved style {kept} is not available; showing the default"
                ));
            }
            ui.horizontal_wrapped(|ui| {
                ui.label("Style");
                for option in styles {
                    if choice(
                        ui,
                        active == Some(option.id),
                        option.label,
                        SettingsItem::Style(option.id),
                        controls,
                    ) {
                        actions.push(WorkspaceAction::SetStyle {
                            instance_id,
                            style_id: option.id,
                        });
                    }
                }
                if button(
                    ui,
                    true,
                    "Clear visual state",
                    SettingsItem::ClearVisual,
                    controls,
                ) {
                    actions.push(WorkspaceAction::ClearVisualState { instance_id });
                }
            });
        }
        // The channel is one row however the Module implements it (an analysis
        // Channel node or a display choice), matching the 1 / 2 / M shortcuts.
        let channel = module.channel();
        if let Some(selected) = channel {
            let descriptor = self.services.source.descriptor().clone();
            ui.horizontal_wrapped(|ui| {
                ui.label("Channel");
                let options = channel_options(&descriptor)
                    .chain(std::iter::once((0, ChannelSelection::MonoAverage)));
                for (index, option) in options {
                    if choice(
                        ui,
                        selected == option,
                        &channel_label(index, option),
                        SettingsItem::Channel(option),
                        controls,
                    ) {
                        actions.push(WorkspaceAction::SetChannel {
                            instance_id,
                            channel: option,
                        });
                    }
                }
            });
        }
        if let Some(controller) = module.data() {
            let spec = controller.spec();
            let settings = DataSettings::describe(&spec);
            let channel_nodes = settings
                .nodes
                .iter()
                .filter(|node| matches!(node.spec, NodeSpec::Channel(_)))
                .count();
            // The channel row already shows a single Channel node.
            let skip_channel = channel.is_some() && channel_nodes == 1;
            let nodes: Vec<&NodeEntry> = settings
                .nodes
                .iter()
                .filter(|node| !(skip_channel && matches!(node.spec, NodeSpec::Channel(_))))
                .collect();
            let histories = settings
                .outputs
                .iter()
                .filter(|output| matches!(output.keep, Keep::Seconds(_)))
                .count();
            if !nodes.is_empty() || histories > 0 {
                ui.separator();
                ui.strong("Analysis");
            }
            for node in nodes {
                self.node_rows(ui, instance_id, node, pending, controls, actions);
            }
            for output in &settings.outputs {
                let label = module.output_label(output.output).unwrap_or("Output");
                if let Keep::Seconds(seconds) = output.keep
                    && let Some(seconds) = number_row(
                        ui,
                        &format!("{label} length"),
                        SettingsItem::Keep(output.output),
                        seconds,
                        (0.5, 60.0),
                        "s",
                        false,
                        pending,
                        controls,
                    )
                {
                    actions.push(WorkspaceAction::SetKeep {
                        instance_id,
                        output: output.output,
                        seconds,
                    });
                }
            }
        }
        let mut section = "";
        for setting in module.view_settings() {
            if matches!(setting.kind, SettingKind::Channel) && channel.is_some() {
                continue;
            }
            if setting.section != section {
                section = setting.section;
                ui.separator();
                ui.strong(section);
            }
            match setting.kind {
                SettingKind::Number {
                    min,
                    max,
                    unit,
                    logarithmic,
                } => {
                    let current = setting.value.number().unwrap_or(min);
                    if let Some(value) = number_row(
                        ui,
                        setting.label,
                        SettingsItem::ViewNumber(setting.key),
                        current,
                        (min, max),
                        unit,
                        logarithmic,
                        pending,
                        controls,
                    ) {
                        actions.push(WorkspaceAction::SetViewSetting {
                            instance_id,
                            key: setting.key,
                            value: SettingValue::Number(value),
                        });
                    }
                }
                SettingKind::Choice(choices) => {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(setting.label);
                        for option in choices {
                            if choice(
                                ui,
                                setting.value == SettingValue::Choice(option.id.into()),
                                option.label,
                                SettingsItem::ViewChoice {
                                    key: setting.key,
                                    id: option.id,
                                },
                                controls,
                            ) {
                                actions.push(WorkspaceAction::SetViewSetting {
                                    instance_id,
                                    key: setting.key,
                                    value: SettingValue::Choice(option.id.to_owned()),
                                });
                            }
                        }
                    });
                }
                // Modules without the channel capability have no display channel.
                SettingKind::Channel => {}
            }
        }
        let roles = module.color_roles();
        if !roles.is_empty() {
            ui.separator();
            ui.strong("Colors (checked: this module only)");
            for role in roles {
                color_row(
                    ui,
                    ColorScope::Instance(instance_id),
                    *role,
                    entry.palette.color(*role),
                    entry.colors.contains_key(role),
                    controls,
                    actions,
                );
            }
        }
    }

    fn node_rows(
        &self,
        ui: &mut egui::Ui,
        instance_id: ModuleInstanceId,
        entry: &NodeEntry,
        pending: &mut Pending,
        controls: &mut Controls,
        actions: &mut Vec<WorkspaceAction>,
    ) {
        let node = entry.node;
        let mut edit = |spec: NodeSpec| {
            actions.push(WorkspaceAction::EditNode {
                instance_id,
                node,
                spec,
            });
        };
        for param in NodeParam::of(&entry.spec) {
            match (param, entry.spec) {
                (NodeParam::Channel, NodeSpec::Channel(current)) => {
                    let descriptor = self.services.source.descriptor().clone();
                    ui.horizontal_wrapped(|ui| {
                        ui.label(param.label());
                        let options = channel_options(&descriptor)
                            .chain(std::iter::once((0, ChannelSelection::MonoAverage)));
                        for (index, option) in options {
                            let response = ui
                                .selectable_label(current == option, channel_label(index, option));
                            if response.clicked() && current != option {
                                edit(NodeSpec::Channel(option));
                            }
                        }
                    });
                }
                (NodeParam::FftWindow, NodeSpec::Fft(fft)) => {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(param.label());
                        for size in FFT_WINDOWS {
                            if choice(
                                ui,
                                fft.window_size == size,
                                &size.to_string(),
                                SettingsItem::FftWindow { node, size },
                                controls,
                            ) {
                                edit(NodeSpec::Fft(with_fft_window(fft, size)));
                            }
                        }
                    });
                }
                (NodeParam::FftHop, NodeSpec::Fft(fft)) => {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(param.label());
                        for divisor in FFT_HOP_DIVISORS {
                            let hop = (fft.window_size / divisor).max(1);
                            if choice(
                                ui,
                                fft.hop_size == hop,
                                &format!("1/{divisor}"),
                                SettingsItem::FftHop { node, divisor },
                                controls,
                            ) {
                                edit(NodeSpec::Fft(FftConfig {
                                    hop_size: hop,
                                    ..fft
                                }));
                            }
                        }
                    });
                }
                (param, spec) => {
                    let (Some((min, max, unit)), Some(current)) =
                        (param.range(), param.number(&spec))
                    else {
                        continue;
                    };
                    let label = if param.label().starts_with(node_label(&spec)) {
                        param.label().to_owned()
                    } else {
                        format!("{} {}", node_label(&spec), param.label())
                    };
                    if let Some(value) = number_row(
                        ui,
                        &label,
                        SettingsItem::Number {
                            node,
                            param: *param,
                        },
                        current,
                        (min, max),
                        unit,
                        false,
                        pending,
                        controls,
                    ) && let Some(spec) = param.with_number(spec, value)
                    {
                        edit(spec);
                    }
                }
            }
        }
    }

    fn window_tab(
        &self,
        ui: &mut egui::Ui,
        window_id: WindowId,
        controls: &mut Controls,
        actions: &mut Vec<WorkspaceAction>,
    ) {
        let window = &self.windows[&window_id];
        ui.heading(format!("Window #{}", window_id.0));
        if window.layout.is_empty() && window.pending_detach.is_none() {
            ui.horizontal_wrapped(|ui| {
                ui.label("Add module");
                for (type_id, label) in self.registry.module_types() {
                    if button(ui, true, label, SettingsItem::AddModule(type_id), controls) {
                        actions.push(WorkspaceAction::AddModule { window_id, type_id });
                    }
                }
            });
        }
        let mut on_top = window.always_on_top;
        let response = ui.checkbox(&mut on_top, "Always on top");
        record(controls, SettingsItem::AlwaysOnTop, &response);
        if response.changed() {
            actions.push(WorkspaceAction::SetAlwaysOnTop {
                window_id,
                enabled: on_top,
            });
        }
        ui.horizontal_wrapped(|ui| {
            let label = if window.maximized {
                "Restore"
            } else {
                "Maximize"
            };
            if button(ui, true, label, SettingsItem::Maximize, controls) {
                actions.push(WorkspaceAction::ToggleMaximized { window_id });
            }
            if button(
                ui,
                true,
                "Group with touching windows",
                SettingsItem::Group,
                controls,
            ) {
                actions.push(WorkspaceAction::GroupTouching { window_id });
            }
            if window.group.is_some()
                && button(ui, true, "Leave group", SettingsItem::LeaveGroup, controls)
            {
                actions.push(WorkspaceAction::LeaveGroup { window_id });
            }
            if button(
                ui,
                true,
                "Close window",
                SettingsItem::CloseWindow,
                controls,
            ) {
                actions.push(WorkspaceAction::CloseWindow { window_id });
            }
        });
    }

    fn workspace_tab(
        &self,
        ui: &mut egui::Ui,
        settings: &mut SettingsWindow,
        controls: &mut Controls,
        actions: &mut Vec<WorkspaceAction>,
    ) {
        ui.heading(format!("Workspace: {}", self.name()));
        let has_store = self.store.is_some();
        if button(ui, self.can_save(), "Save", SettingsItem::Save, controls) {
            actions.push(WorkspaceAction::SaveWorkspace);
        }
        ui.horizontal(|ui| {
            ui.label("Name");
            let response = ui.add_enabled(
                has_store,
                egui::TextEdit::singleline(&mut settings.name).desired_width(160.0),
            );
            record(controls, SettingsItem::Name, &response);
            settings.typing = response.has_focus();
        });
        let name = settings.name.trim().to_owned();
        let valid = valid_name(&name);
        // Windows names files case-insensitively.
        let exists = self
            .saved_workspaces()
            .iter()
            .any(|saved| same_name(saved, &name));
        // New saves the current workspace first: its own name cannot be the new one.
        let current = same_name(&name, &self.name);
        if !name.is_empty() && !valid {
            ui.label("Not a valid name");
        } else if exists {
            ui.label("A workspace with this name exists");
        } else if current {
            ui.label("This is the current workspace's name");
        }
        ui.horizontal(|ui| {
            let allowed = has_store && valid && !exists;
            if button(ui, allowed, "Save as", SettingsItem::SaveAs, controls) {
                actions.push(WorkspaceAction::SaveWorkspaceAs { name: name.clone() });
                settings.name.clear();
            }
            if button(
                ui,
                allowed && !current,
                "New",
                SettingsItem::NewWorkspace,
                controls,
            ) {
                actions.push(WorkspaceAction::NewWorkspace { name: name.clone() });
                settings.name.clear();
            }
        });
        let saved = self.saved_workspaces();
        if !saved.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label("Switch to");
                for (index, saved_name) in saved.iter().enumerate() {
                    if choice(
                        ui,
                        *saved_name == self.name,
                        saved_name,
                        SettingsItem::Switch(index as u16),
                        controls,
                    ) {
                        actions.push(WorkspaceAction::SwitchWorkspace {
                            name: saved_name.clone(),
                        });
                    }
                }
            });
        }
        if settings.confirm_reset {
            ui.label("Replace this workspace with one default Spectrum and the Dark theme?");
            ui.horizontal(|ui| {
                if button(ui, true, "Reset", SettingsItem::ConfirmReset, controls) {
                    actions.push(WorkspaceAction::ResetWorkspace);
                    settings.confirm_reset = false;
                }
                if button(ui, true, "Cancel", SettingsItem::CancelReset, controls) {
                    settings.confirm_reset = false;
                }
            });
        } else if button(ui, true, "Reset workspace…", SettingsItem::Reset, controls) {
            settings.confirm_reset = true;
        }
        ui.separator();
        ui.strong("Source");
        let status = self.source_status();
        ui.label(super::window::source_status_text(&status));
        let label = match status.state {
            SourceState::Paused => "Resume source (Space)",
            _ => "Pause source (Space)",
        };
        if button(
            ui,
            matches!(status.state, SourceState::Running | SourceState::Paused),
            label,
            SettingsItem::Pause,
            controls,
        ) {
            actions.push(WorkspaceAction::TogglePause);
        }
        if !self.notices.is_empty() {
            ui.separator();
            for notice in &self.notices {
                ui.label(notice);
            }
            if button(
                ui,
                true,
                "Clear notices",
                SettingsItem::ClearNotices,
                controls,
            ) {
                actions.push(WorkspaceAction::ClearNotices);
            }
        }
        ui.separator();
        let quit = if self.can_save() {
            "Quit (save workspace)"
        } else {
            "Quit"
        };
        if button(ui, true, quit, SettingsItem::Quit, controls) {
            actions.push(WorkspaceAction::Quit);
        }
    }

    fn theme_tab(
        &self,
        ui: &mut egui::Ui,
        selected_type: &mut Option<&'static str>,
        pending: &mut Pending,
        controls: &mut Controls,
        actions: &mut Vec<WorkspaceAction>,
    ) {
        let preset = self.theme.preset();
        ui.horizontal(|ui| {
            ui.label("Preset");
            for option in Preset::ALL {
                if choice(
                    ui,
                    preset == option,
                    option.label(),
                    SettingsItem::Preset(option),
                    controls,
                ) {
                    actions.push(WorkspaceAction::SetThemePreset { preset: option });
                }
            }
        });
        if let Some(size) = number_row(
            ui,
            "Text size",
            SettingsItem::TextSize,
            self.chrome.text_size,
            TEXT_SIZE_RANGE,
            "pt",
            false,
            pending,
            controls,
        ) {
            actions.push(WorkspaceAction::SetTextSize { size });
        }
        ui.separator();
        ui.strong("Global colors (checked: changed in this workspace)");
        for role in ColorRole::ALL {
            color_row(
                ui,
                ColorScope::Global,
                role,
                self.chrome.color(role),
                self.theme.colors.contains_key(&role),
                controls,
                actions,
            );
        }
        ui.separator();
        ui.strong("Module type style");
        ui.horizontal_wrapped(|ui| {
            for (type_id, label) in self.registry.module_types() {
                let response = ui.selectable_label(*selected_type == Some(type_id), label);
                record(controls, SettingsItem::TypeStyle(type_id), &response);
                if response.clicked() {
                    *selected_type = Some(type_id);
                }
            }
        });
        if let Some(type_id) = *selected_type {
            let style = self.theme.type_styles.get(type_id);
            let resolved = self.theme.resolve(type_id, &RoleOverrides::new());
            for role in TYPE_ROLES {
                color_row(
                    ui,
                    ColorScope::Type(type_id),
                    role,
                    resolved.color(role),
                    style.is_some_and(|style| style.contains_key(&role)),
                    controls,
                    actions,
                );
            }
        }
    }
}
