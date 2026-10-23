//! Every settings change goes through `WorkspaceAction`.

mod color_picker;
mod module_tab;
mod theme_tab;
mod widgets;
mod window_tab;
mod workspace_tab;

#[cfg(feature = "automation")]
use std::collections::HashMap;
use std::time::Duration;

use eframe::egui::{self, Pos2, RichText, Vec2, ViewportCommand, pos2, vec2};

use crate::{
    audio::Endpoints,
    dataflow::{ChannelSelection, NodeRef, OutputId},
    render::Color,
    settings::NodeParam,
    theme::{ColorRole, ThemePreset},
};

use self::widgets::record;
#[cfg(feature = "automation")]
use super::window::{ControlKey, ControlTarget};
use super::{ColorScope, ModuleInstanceId, WindowId, Workspace, WorkspaceAction, window::Controls};

// Logical points.
const SETTINGS_SIZE: Vec2 = vec2(420.0, 600.0);
const SETTINGS_MIN_SIZE: Vec2 = vec2(280.0, 220.0);
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Highlight {
    Window {
        window: WindowId,
        group_hovered: bool,
    },
    Module(ModuleInstanceId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SettingsItem {
    Tab(Tab),
    TypePicker,
    ModuleType(&'static str),
    Style(&'static str),
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
    ResetColor {
        scope: ColorScope,
        role: ColorRole,
    },
    ResetColors(ColorScope),
    ResetAnalysis,
    ResetSection(&'static str),
    Swatch {
        scope: ColorScope,
        role: ColorRole,
        index: u8,
    },
    ColorPicker {
        scope: ColorScope,
        role: ColorRole,
        part: PickerPart,
    },
    SplitRight,
    SplitDown,
    Detach,
    MoveTo,
    CloseModule,
    AlwaysOnTop,
    Lock,
    Maximize,
    Group,
    LeaveGroup,
    NewWindow,
    CloseWindow,
    AddModule(&'static str),
    PresetName,
    SavePresetAs,
    SavePreset,
    /// Index in built-in declaration order.
    BuiltinPreset(u16),
    /// Index in the sorted user preset list.
    SavedPreset(u16),
    DeletePreset(u16),
    ConfirmDelete,
    CancelDelete,
    Pause,
    Notice(u16),
    CopyNotices,
    InputPicker,
    /// Position in the open input picker, from the top.
    InputChoice(u16),
    ClearNotices,
    Quit,
    Preset(ThemePreset),
    TextSize,
    ReferenceLevel,
    TypeStyle(&'static str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum PickerPart {
    Button,
    /// 0–3: red, green, blue, alpha.
    Channel(u8),
    Hex,
    Plane,
    Hue,
    Alpha,
}

// Catppuccin accents: Mocha on dark, Latte on light; matching order in both palettes.
pub(crate) fn swatches(dark: bool) -> &'static [Color; 8] {
    if dark {
        &MOCHA_SWATCHES
    } else {
        &LATTE_SWATCHES
    }
}

const MOCHA_SWATCHES: [Color; 8] = [
    Color::rgb(0x94, 0xe2, 0xd5), // teal
    Color::rgb(0x74, 0xc7, 0xec), // sapphire
    Color::rgb(0xcb, 0xa6, 0xf7), // mauve
    Color::rgb(0xf3, 0x8b, 0xa8), // red
    Color::rgb(0xfa, 0xb3, 0x87), // peach
    Color::rgb(0xf9, 0xe2, 0xaf), // yellow
    Color::rgb(0xa6, 0xe3, 0xa1), // green
    Color::rgb(0xcd, 0xd6, 0xf4), // text
];

const LATTE_SWATCHES: [Color; 8] = [
    Color::rgb(0x17, 0x92, 0x99), // teal
    Color::rgb(0x20, 0x9f, 0xb5), // sapphire
    Color::rgb(0x88, 0x39, 0xef), // mauve
    Color::rgb(0xd2, 0x0f, 0x39), // red
    Color::rgb(0xfe, 0x64, 0x0b), // peach
    Color::rgb(0xdf, 0x8e, 0x1d), // yellow
    Color::rgb(0x40, 0xa0, 0x2b), // green
    Color::rgb(0x4c, 0x4f, 0x69), // text
];

pub(crate) struct SettingsWindow {
    tab: Tab,
    instance: Option<ModuleInstanceId>,
    /// The window the Window tab edits when no instance is targeted.
    window: Option<WindowId>,
    type_style: Option<&'static str>,
    name: String,
    confirm_delete: Option<String>,
    /// Copy reads "Copied" until the pointer leaves it.
    notices_copied: bool,
    /// egui clears focus on Escape before our UI pass; retain the previous pass's text focus
    /// so canceling an edit does not also close the settings window.
    text_focus: bool,
    /// Listed when the input picker opens and kept while it stays open.
    endpoints: Option<Endpoints>,
    highlight: Option<Highlight>,
    /// The instance and window the pages showed in the last pass.
    shown: (Option<ModuleInstanceId>, Option<WindowId>),
    /// Builder inputs stay fixed while the window lives.
    initial_position: Option<Pos2>,
    /// Distinguishes each opening: a reopened window is a new viewport that starts
    /// from the builder, not from egui's memory of the closed one.
    opening: u64,
    placed: bool,
    commands: Vec<ViewportCommand>,
    #[cfg(feature = "automation")]
    targets: HashMap<ControlKey, ControlTarget>,
    #[cfg(feature = "automation")]
    frame_token: u64,
    #[cfg(feature = "automation")]
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
            confirm_delete: None,
            notices_copied: false,
            text_focus: false,
            endpoints: None,
            highlight: None,
            shown: (None, None),
            initial_position,
            opening,
            placed: false,
            commands: Vec::new(),
            #[cfg(feature = "automation")]
            targets: HashMap::new(),
            #[cfg(feature = "automation")]
            frame_token: 0,
            #[cfg(feature = "automation")]
            pass_token: 0,
        }
    }

    pub(super) fn follow_replacement(&mut self, old: ModuleInstanceId, new: ModuleInstanceId) {
        if self.instance == Some(old) {
            self.instance = Some(new);
        }
    }

    pub(super) fn window(&self) -> Option<WindowId> {
        self.window
    }

    pub(super) fn forget_targets(&mut self) {
        self.instance = None;
        self.window = None;
        self.confirm_delete = None;
        if matches!(self.tab, Tab::Module | Tab::Window) {
            self.tab = Tab::Workspace;
        }
    }
}

impl Workspace {
    pub(crate) fn settings_viewport_id(&self) -> Option<egui::ViewportId> {
        let settings = self.settings.as_ref()?;
        Some(egui::ViewportId::from_hash_of((
            "taffymeters.settings",
            settings.opening,
        )))
    }

    pub(crate) fn settings_builder(&self) -> Option<egui::ViewportBuilder> {
        let settings = self.settings.as_ref()?;
        let mut builder = egui::ViewportBuilder::default()
            .with_title("Taffymeters settings")
            .with_icon(crate::icon::small())
            .with_inner_size(SETTINGS_SIZE)
            .with_min_inner_size(SETTINGS_MIN_SIZE)
            .with_always_on_top();
        if let Some(position) = settings.initial_position {
            builder = builder.with_position(position);
        }
        Some(builder)
    }

    pub(crate) fn settings_open(&self) -> bool {
        self.settings.is_some()
    }

    #[cfg(feature = "automation")]
    pub(crate) fn settings_targets(&self) -> HashMap<ControlKey, ControlTarget> {
        self.settings
            .as_ref()
            .map(|settings| settings.targets.clone())
            .unwrap_or_default()
    }

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
        let position = source.outer.map(|outer| {
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
        settings.confirm_delete = None;
        settings.commands.push(ViewportCommand::Focus);
        Ok(())
    }

    pub(super) fn retarget_settings(&mut self, window_id: WindowId) {
        if let Some(settings) = &mut self.settings {
            settings.instance = None;
            settings.window = Some(window_id);
            settings.tab = Tab::Window;
            settings.confirm_delete = None;
        }
    }

    pub(super) fn select_in_settings(
        &mut self,
        window_id: WindowId,
        instance: Option<ModuleInstanceId>,
    ) -> Result<(), super::WorkspaceError> {
        self.window(window_id)?;
        let Some(settings) = &mut self.settings else {
            return Err(super::WorkspaceError::Unsupported(
                "selecting while the settings window is closed",
            ));
        };
        if instance.is_some() || settings.tab == Tab::Window {
            settings.instance = instance;
            settings.window = Some(window_id);
        }
        Ok(())
    }

    pub(super) fn settings_targets_now(&self) -> (Option<ModuleInstanceId>, Option<WindowId>) {
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

    pub(super) fn window_tab_target(&self) -> Option<(WindowId, bool)> {
        match self.settings.as_ref()?.highlight? {
            Highlight::Window {
                window,
                group_hovered,
            } => Some((window, group_hovered)),
            Highlight::Module(_) => None,
        }
    }

    pub(super) fn module_tab_target(&self) -> Option<ModuleInstanceId> {
        match self.settings.as_ref()?.highlight? {
            Highlight::Module(instance) => Some(instance),
            Highlight::Window { .. } => None,
        }
    }

    pub(crate) fn settings_ui(&mut self, ui: &mut egui::Ui, display_time: Duration) {
        // Keep action deadlines moving even when every Workspace window is minimized.
        self.tick(display_time);
        let ctx = ui.ctx().clone();
        super::window::apply_chrome_style(&ctx, &self.chrome);
        let (instance, window) = self.settings_targets_now();
        let Some(mut settings) = self.settings.take() else {
            return;
        };
        #[cfg(feature = "automation")]
        {
            if ctx.current_pass_index() == 0 {
                settings.frame_token += 1;
            }
            settings.pass_token += 1;
            settings.targets.clear();
        }
        // Discard drafts on retargeting so they cannot apply to the new page's controls.
        if settings.shown != (instance, window) {
            settings.shown = (instance, window);
            if let Some(id) = ctx.memory(egui::Memory::focused) {
                ctx.data_mut(|data| data.remove_temp::<String>(id));
                ctx.memory_mut(|memory| memory.surrender_focus(id));
            }
        }
        // Include native decorations when keeping the title bar and close button reachable.
        if !settings.placed
            && let Some(outer) = ctx.input(|input| input.viewport().outer_rect)
        {
            settings.placed = true;
            let ppp = ctx.pixels_per_point();
            let physical = egui::Rect::from_min_max(outer.min * ppp, outer.max * ppp);
            if let Some(target) = self
                .desktop
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
        let mut group_hovered = false;
        let frame = egui::Frame::central_panel(ui.style()).inner_margin(egui::Margin::same(10));
        frame.show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            let spacing = ui.spacing_mut();
            spacing.item_spacing = vec2(6.0, 8.0);
            spacing.button_padding = vec2(8.0, 3.0);
            spacing.interact_size.y = 22.0;
            // Equal-width tabs when possible; preserve readable text at the minimum width.
            ui.horizontal(|ui| {
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
                        settings.confirm_delete = None;
                    }
                }
            });
            ui.add_space(2.0);
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| match settings.tab {
                    Tab::Module => match instance {
                        Some(instance) => {
                            self.module_tab(ui, instance, &mut controls, &mut actions);
                        }
                        None => {
                            ui.label(RichText::new("Click a module to edit it").weak());
                        }
                    },
                    Tab::Window => match window {
                        Some(window) => {
                            group_hovered =
                                self.window_tab(ui, window, &mut controls, &mut actions);
                        }
                        None => {
                            ui.label(RichText::new("No window selected").weak());
                        }
                    },
                    Tab::Workspace => {
                        self.workspace_tab(ui, &mut settings, &mut controls, &mut actions);
                    }
                    Tab::Theme => {
                        self.theme_tab(ui, &mut settings.type_style, &mut controls, &mut actions);
                    }
                });
        });
        settings.text_focus = ctx.egui_wants_keyboard_input();
        #[cfg(feature = "automation")]
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
        // The Workspace windows draw the open tab's outline from this; they ran
        // before this pass, so a change needs another host pass.
        let highlight = match settings.tab {
            Tab::Window => window.map(|window| Highlight::Window {
                window,
                group_hovered,
            }),
            Tab::Module => instance.map(Highlight::Module),
            Tab::Workspace | Tab::Theme => None,
        };
        let mut repaint = settings.highlight != highlight;
        settings.highlight = highlight;
        self.settings = Some(settings);
        let windows_before = self.windows.len();
        repaint |= !actions.is_empty();
        for action in actions {
            let _ = self.apply_action(action);
        }
        if repaint || windows_before != self.windows.len() || self.settings.is_none() {
            ctx.request_repaint_of(egui::ViewportId::ROOT);
        }
    }
}
