//! The settings window: one native, decorated window next to the Workspace windows
//! (it does not shrink with them). Right-clicking a Module opens it on that
//! instance. Tabs hold everything the instance, its window, the workspace and the
//! theme offer; every change goes through `WorkspaceAction`.

use std::time::Duration;

use eframe::egui::{self, Pos2, RichText, Vec2, ViewportCommand, pos2, vec2};

use self::widgets::record;
#[cfg(any(test, feature = "automation"))]
use super::window::ControlKey;
use super::{
    ColorScope, ModuleInstanceId, WindowId, Workspace, WorkspaceAction, desktop::DesktopProbe,
    window::Controls,
};
use crate::{
    dataflow::{ChannelSelection, NodeRef, OutputId},
    render::Color,
    settings::NodeParam,
    theme::{ColorRole, Preset},
};

mod module_tab;
mod theme_tab;
mod widgets;
mod window_tab;
mod workspace_tab;

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
    /// The Module type picker; its entries are `ModuleType`.
    TypePicker,
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
    ViewToggle(&'static str),
    /// Back to the inherited color (shown while the scope sets its own).
    ResetColor {
        scope: ColorScope,
        role: ColorRole,
    },
    /// Every color this scope sets back to the inherited one.
    ResetColors(ColorScope),
    /// The instance's analysis parameters back to its type's defaults.
    ResetAnalysis,
    /// The instance's view settings of one section back to their defaults.
    ResetSection(&'static str),
    Swatch {
        scope: ColorScope,
        role: ColorRole,
        index: u8,
    },
    SplitRight,
    SplitDown,
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
    ReferenceLevel,
    TypeStyle(&'static str),
}

/// Quick colors offered next to the full picker: Catppuccin Mocha accents.
pub(crate) const SWATCHES: [Color; 8] = [
    Color::rgb(0x94, 0xe2, 0xd5), // teal
    Color::rgb(0x74, 0xc7, 0xec), // sapphire
    Color::rgb(0xcb, 0xa6, 0xf7), // mauve
    Color::rgb(0xf3, 0x8b, 0xa8), // red
    Color::rgb(0xfa, 0xb3, 0x87), // peach
    Color::rgb(0xf9, 0xe2, 0xaf), // yellow
    Color::rgb(0xa6, 0xe3, 0xa1), // green
    Color::rgb(0xcd, 0xd6, 0xf4), // text
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

pub(crate) struct SettingsWindow {
    tab: Tab,
    /// The instance the Module tab edits.
    instance: Option<ModuleInstanceId>,
    /// The window the Window tab edits when no instance is targeted.
    window: Option<WindowId>,
    type_style: Option<&'static str>,
    name: String,
    confirm_reset: bool,
    /// A text field (a name, a typed number) had keyboard focus at the end of the
    /// last pass: Escape belongs to it. egui drops the focus on Escape before the
    /// next pass runs, so the pass that sees Escape cannot ask egui itself.
    text_focus: bool,
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

impl SettingsWindow {
    fn new(initial_position: Option<Pos2>, opening: u64) -> Self {
        Self {
            tab: Tab::Module,
            instance: None,
            window: None,
            type_style: None,
            name: String::new(),
            confirm_reset: false,
            text_focus: false,
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
        super::window::apply_chrome_style(&ctx, &self.chrome);
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
        if escape && !settings.text_focus && !egui::Popup::is_any_open(&ctx) {
            actions.push(WorkspaceAction::CloseSettings);
        }
        let mut controls = Controls::default();
        let frame = egui::Frame::central_panel(ui.style()).inner_margin(egui::Margin::same(10));
        frame.show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            let spacing = ui.spacing_mut();
            spacing.item_spacing = vec2(6.0, 8.0);
            spacing.button_padding = vec2(8.0, 3.0);
            spacing.interact_size.y = 22.0;
            ui.horizontal(|ui| {
                // Equal tabs when they fit; at the minimum width each keeps its text.
                let gap = 4.0;
                ui.spacing_mut().item_spacing.x = gap;
                ui.spacing_mut().button_padding.x = 4.0;
                let text = Tab::ALL.map(|tab| {
                    egui::WidgetText::from(tab.label())
                        .into_galley(
                            ui,
                            Some(egui::TextWrapMode::Extend),
                            f32::INFINITY,
                            egui::TextStyle::Button,
                        )
                        .size()
                        .x
                });
                let count = Tab::ALL.len() as f32;
                let room = ui.available_width() - gap * (count - 1.0);
                let equal = room / count;
                let padding = ((room - text.iter().sum::<f32>()) / count).max(8.0);
                let fits = text.iter().all(|text| text + 8.0 <= equal);
                for (tab, text) in Tab::ALL.into_iter().zip(text) {
                    let width = if fits { equal } else { text + padding };
                    let response = ui.add_sized(
                        [width, 26.0],
                        egui::Button::new(tab.label())
                            .selected(settings.tab == tab)
                            .frame_when_inactive(true),
                    );
                    record(&mut controls, SettingsItem::Tab(tab), &response);
                    if response.clicked() {
                        settings.tab = tab;
                        settings.confirm_reset = false;
                    }
                }
            });
            ui.add_space(2.0);
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| match settings.tab {
                    Tab::Module => match instance {
                        Some(instance) => {
                            self.module_tab(ui, instance, &mut controls, &mut actions)
                        }
                        None => {
                            ui.label(
                                RichText::new(
                                    "No module selected. Right-click a module to edit it.",
                                )
                                .weak(),
                            );
                        }
                    },
                    Tab::Window => match window {
                        Some(window) => self.window_tab(ui, window, &mut controls, &mut actions),
                        None => {
                            ui.label(RichText::new("No window selected.").weak());
                        }
                    },
                    Tab::Workspace => {
                        self.workspace_tab(ui, &mut settings, &mut controls, &mut actions);
                    }
                    Tab::Theme => {
                        self.theme_tab(ui, &mut settings.type_style, &mut controls, &mut actions)
                    }
                });
        });
        settings.text_focus = ctx.egui_wants_keyboard_input();
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
}
