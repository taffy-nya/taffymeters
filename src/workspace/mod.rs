//! Workspace: owns the application's Module instances, the windows and each
//! window's BSP layout. Layout trees only reference instances by ID, so every
//! layout operation (split, move, detach, merge) leaves the Module objects and
//! their analysis subscriptions untouched; only closing an instance drops it.

mod arrange;
mod desktop;
#[cfg(feature = "automation")]
mod inspect;
mod instances;
mod layout;
mod moving;
#[cfg(windows)]
mod native;
mod persist;
mod placement;
mod settings_window;
mod window;

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Duration,
};

use eframe::egui::{self, Pos2, Rect, Vec2, ViewportCommand, vec2};

use crate::{
    LiveAudio,
    audio::{SourceState, SourceStatus},
    dataflow::{ChannelSelection, ControlError, NodeRef, NodeSpec, OutputId, SharedAnalysisId},
    extensions::{Registry, RegistryError},
    module::{Module, ModuleServices},
    modules,
    persistence::{InputConfig, ModuleFile, PersistError, PresetRef, WorkspaceStore},
    render::Color,
    settings::SettingValue,
    theme::{ColorRole, Palette, Preset, RoleOverrides, ThemeSettings},
};

pub(crate) use desktop::{DesktopProbe, NativeDesktop};
#[cfg(feature = "automation")]
pub(crate) use desktop::{Display, Edge, display_at};
#[cfg(feature = "automation")]
pub(crate) use layout::Axis;
use layout::{Layout, LayoutError};
pub(crate) use layout::{Side, SplitId};
#[cfg(feature = "automation")]
pub(crate) use moving::{MoveRequest, WindowTracker};
use settings_window::SettingsWindow;
#[cfg(feature = "automation")]
pub(crate) use settings_window::{SWATCHES, SettingsItem, Tab};
use window::WindowUiState;
#[cfg(feature = "automation")]
pub(crate) use window::{ControlKey, ControlTarget, EdgeLine, ResizeEdge};

/// Initial logical size of the first window.
pub(crate) const INITIAL_WINDOW_SIZE: Vec2 = vec2(900.0, 420.0);
/// Minimum logical size of a window without any Module.
pub(crate) const EMPTY_WINDOW_MIN_SIZE: Vec2 = vec2(140.0, 48.0);
/// Logical offset of a detached window from its source window.
const DETACH_OFFSET_POINTS: f32 = 32.0;
/// Display time a detached window may take to show its first frame.
pub(crate) const DETACH_TIMEOUT: Duration = Duration::from_secs(5);
/// Display time the status badge shows an operation error or a new notice.
pub(crate) const MESSAGE_LIFETIME: Duration = Duration::from_secs(6);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct ModuleInstanceId(pub(crate) u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct WindowId(pub(crate) u64);

/// The window created with the process.
pub(crate) const FIRST_WINDOW: WindowId = WindowId(1);

impl WindowId {
    /// Every Workspace window is a child viewport of egui's hidden host root.
    pub(crate) fn viewport_id(self) -> egui::ViewportId {
        egui::ViewportId::from_hash_of(("taffymeters.window", self.0))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum MoveTarget {
    Beside {
        instance_id: ModuleInstanceId,
        side: Side,
    },
    EmptyWindow(WindowId),
}

/// Whose colors a theme edit changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ColorScope {
    /// The workspace's global roles.
    Global,
    /// The style of every instance of a Module type.
    Type(&'static str),
    /// One instance's explicit overrides.
    Instance(ModuleInstanceId),
}

#[derive(Clone, Debug)]
pub(crate) enum WorkspaceAction {
    /// Create a default instance as the only leaf of an empty window.
    AddModule {
        window_id: WindowId,
        type_id: &'static str,
    },
    /// Copy the instance next to `instance_id`: same type, configuration, analysis
    /// spec and bindings, with a new subscription and fresh private state.
    SplitModule {
        instance_id: ModuleInstanceId,
        side: Side,
    },
    /// Create a default instance of `type_id` in the leaf and close the old one.
    ReplaceModule {
        instance_id: ModuleInstanceId,
        type_id: &'static str,
    },
    CloseModule {
        instance_id: ModuleInstanceId,
    },
    /// Point the open settings window at the instance (a left click on it). See
    /// `Workspace::select_in_settings`.
    SelectModule {
        instance_id: ModuleInstanceId,
    },
    /// Point the open settings window's Window tab at an empty window (a left
    /// click on it).
    SelectWindow {
        window_id: WindowId,
    },
    SetChannel {
        instance_id: ModuleInstanceId,
        channel: ChannelSelection,
    },
    SetStyle {
        instance_id: ModuleInstanceId,
        style_id: &'static str,
    },
    ClearVisualState {
        instance_id: ModuleInstanceId,
    },
    TogglePause,
    SetSplitRatio {
        window_id: WindowId,
        split_id: SplitId,
        ratio: f32,
        manual: bool,
    },
    ResetSplit {
        window_id: WindowId,
        split_id: SplitId,
    },
    /// Open a new window that receives the instance once it shows its first frame.
    DetachModule {
        instance_id: ModuleInstanceId,
    },
    CancelDetach {
        window_id: WindowId,
    },
    BeginMove {
        instance_id: ModuleInstanceId,
    },
    CancelMove,
    MoveModule {
        instance_id: ModuleInstanceId,
        target: MoveTarget,
    },
    /// Open an empty window the size of `window_id`, offset like a detached one,
    /// and point the open settings window at it.
    NewWindow {
        window_id: WindowId,
    },
    CloseWindow {
        window_id: WindowId,
    },
    SetAlwaysOnTop {
        window_id: WindowId,
        enabled: bool,
    },
    /// Lock a window (settings "Lock"): it takes right clicks only, cannot be
    /// resized or maximized, and body drags move neither it nor its group.
    SetLocked {
        window_id: WindowId,
        enabled: bool,
    },
    ToggleMaximized {
        window_id: WindowId,
    },
    GroupTouching {
        window_id: WindowId,
    },
    LeaveGroup {
        window_id: WindowId,
    },
    /// Set the parameters of one node of the instance's analysis. Nodes in the
    /// shared group of an explicit binding change for every member.
    EditNode {
        instance_id: ModuleInstanceId,
        node: NodeRef,
        spec: NodeSpec,
    },
    /// History length of one output.
    SetKeep {
        instance_id: ModuleInstanceId,
        output: OutputId,
        seconds: f32,
    },
    /// Show the settings window on an instance, or on a window without one.
    OpenSettings {
        window_id: WindowId,
        instance_id: Option<ModuleInstanceId>,
    },
    CloseSettings,
    SetViewSetting {
        instance_id: ModuleInstanceId,
        key: &'static str,
        value: SettingValue,
    },
    /// Set (`Some`) or remove (`None`) an explicit color.
    SetColor {
        scope: ColorScope,
        role: ColorRole,
        color: Option<Color>,
    },
    SetThemePreset {
        preset: Preset,
    },
    /// `None` follows the preset's size.
    SetTextSize {
        size: Option<f32>,
    },
    /// Display reference level in dBFS; `None` follows the default.
    SetReferenceLevel {
        dbfs: Option<f32>,
    },
    /// Replace the whole workspace with a preset.
    LoadPreset {
        preset: PresetRef,
    },
    /// Save the workspace as a new user preset; the session then comes from it.
    SavePresetAs {
        name: String,
    },
    /// Save the workspace over the user preset the session comes from.
    SavePreset,
    DeletePreset {
        name: String,
    },
    ClearNotices,
    /// Save the session and close every window (the application exits).
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActionOutcome {
    Created(ModuleInstanceId),
    WindowCreated(WindowId),
    Applied,
}

#[derive(Debug)]
pub(crate) enum WorkspaceError {
    StaleInstance(ModuleInstanceId),
    StaleWindow(WindowId),
    Unsupported(&'static str),
    InsufficientSpace(&'static str),
    IdExhausted,
    Layout(LayoutError),
    Registry(RegistryError),
    Module(crate::module::ModuleError),
    Control(ControlError),
    LiveAudio(crate::live::LiveAudioError),
    Persist(PersistError),
    /// The workspace has no directory to save to.
    NoStore,
    PresetExists(String),
    NoSuchPreset(String),
    /// The session does not come from a user preset to save over.
    NoSavedPreset,
    /// A copy could not take over every setting of the original.
    CopyIncomplete(String),
}

impl std::fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StaleInstance(id) => write!(f, "module instance {} is no longer available", id.0),
            Self::StaleWindow(id) => write!(f, "window {} is no longer available", id.0),
            Self::Unsupported(action) => write!(f, "operation is not supported: {action}"),
            Self::InsufficientSpace(action) => write!(f, "not enough space to {action}"),
            Self::IdExhausted => f.write_str("workspace IDs are exhausted"),
            Self::Layout(error) => std::fmt::Display::fmt(error, f),
            Self::Registry(error) => std::fmt::Display::fmt(error, f),
            Self::Module(error) => std::fmt::Display::fmt(error, f),
            Self::Control(error) => std::fmt::Display::fmt(error, f),
            Self::LiveAudio(error) => std::fmt::Display::fmt(error, f),
            Self::Persist(error) => std::fmt::Display::fmt(error, f),
            Self::NoStore => f.write_str("this workspace has no save location"),
            Self::PresetExists(name) => write!(f, "preset {name:?} already exists"),
            Self::NoSuchPreset(name) => write!(f, "preset {name:?} is not available"),
            Self::NoSavedPreset => {
                f.write_str("this workspace does not come from a saved preset; use Save as")
            }
            Self::CopyIncomplete(reason) => write!(f, "copy incomplete: {reason}"),
        }
    }
}

impl std::error::Error for WorkspaceError {}

impl From<RegistryError> for WorkspaceError {
    fn from(error: RegistryError) -> Self {
        Self::Registry(error)
    }
}

impl From<crate::module::ModuleError> for WorkspaceError {
    fn from(error: crate::module::ModuleError) -> Self {
        Self::Module(error)
    }
}

impl From<LayoutError> for WorkspaceError {
    fn from(error: LayoutError) -> Self {
        Self::Layout(error)
    }
}

impl From<ControlError> for WorkspaceError {
    fn from(error: ControlError) -> Self {
        Self::Control(error)
    }
}

impl From<PersistError> for WorkspaceError {
    fn from(error: PersistError) -> Self {
        Self::Persist(error)
    }
}

struct ModuleEntry {
    type_id: &'static str,
    label: &'static str,
    module: Box<dyn Module>,
    /// Visibility last reported to the instance (new instances start visible).
    visible: bool,
    /// The instance's explicit colors; everything else follows the theme.
    colors: RoleOverrides,
    /// Resolved colors handed to the Module when it draws.
    palette: Palette,
    /// Saved configuration of an instance this build could not restore; written
    /// back unchanged.
    preserved: Option<Preserved>,
    /// Saved style this build does not provide; kept until the user picks one.
    preserved_style: Option<String>,
}

/// A saved instance this build could not restore and why.
struct Preserved {
    file: ModuleFile,
    reason: String,
}

#[derive(Clone, Copy, Debug)]
struct PendingDetach {
    instance_id: ModuleInstanceId,
    source: WindowId,
    requested_at: Duration,
}

pub(crate) struct WindowState {
    layout: Layout,
    always_on_top: bool,
    /// Locked ("Lock" in the settings): the window takes right clicks only, cannot
    /// be resized natively, and body drags move neither it nor its group.
    locked: bool,
    maximized: bool,
    /// Logical content size from the last UI pass (initially the requested size).
    content_size: Vec2,
    pixels_per_point: f32,
    initial_position: Option<Pos2>,
    initial_size: Vec2,
    /// Minimum inner size at creation (the layout's, later kept by `MinInnerSize`).
    initial_min_size: Vec2,
    /// Builder inputs stay fixed after creation; later changes are commands, so egui
    /// never sees a changed builder and recreates the native window.
    initial_always_on_top: bool,
    /// Native resizability at creation: off for a restored locked window.
    initial_resizable: bool,
    pending_detach: Option<PendingDetach>,
    opened: bool,
    /// Outer frame observed from the native window, physical pixels.
    outer: Option<Rect>,
    /// The desktop tracks the native window: moves go through it (`DesktopProbe`).
    tracked: bool,
    group: Option<u64>,
    commands: Vec<ViewportCommand>,
    ui: WindowUiState,
    /// The native window is minimized: its instances are not drawn.
    minimized: bool,
    /// Saved desktop position (physical pixels) to verify once the window shows:
    /// the builder converts logical points with a scale that may differ.
    restore_target: Option<Pos2>,
    initial_maximized: bool,
}

impl WindowState {
    fn new(layout: Layout, size: Vec2, always_on_top: bool) -> Self {
        Self {
            layout,
            always_on_top,
            locked: false,
            maximized: false,
            content_size: size,
            pixels_per_point: 1.0,
            initial_position: None,
            initial_size: size,
            initial_min_size: EMPTY_WINDOW_MIN_SIZE,
            initial_always_on_top: always_on_top,
            initial_resizable: true,
            pending_detach: None,
            opened: false,
            outer: None,
            tracked: false,
            group: None,
            commands: Vec::new(),
            ui: WindowUiState::default(),
            minimized: false,
            restore_target: None,
            initial_maximized: false,
        }
    }

    pub(crate) fn viewport_builder(&self, id: WindowId) -> egui::ViewportBuilder {
        let mut builder = egui::ViewportBuilder::default()
            .with_title(format!("Taffymeters #{}", id.0))
            .with_icon(crate::icon::small())
            .with_inner_size(self.initial_size)
            .with_min_inner_size(self.initial_min_size)
            .with_decorations(false)
            .with_resizable(self.initial_resizable);
        if let Some(position) = self.initial_position {
            builder = builder.with_position(position);
        }
        if self.initial_always_on_top {
            builder = builder.with_always_on_top();
        }
        if self.initial_maximized {
            builder = builder.with_maximized(true);
        }
        builder
    }
}

/// Window list entry for the native adapter.
#[derive(Clone, Debug)]
pub(crate) struct WindowSummary {
    pub(crate) id: WindowId,
    pub(crate) builder: egui::ViewportBuilder,
}

pub struct Workspace {
    modules: HashMap<ModuleInstanceId, ModuleEntry>,
    windows: BTreeMap<WindowId, WindowState>,
    registry: Registry,
    services: ModuleServices,
    move_source: Option<ModuleInstanceId>,
    next_instance_id: u64,
    next_window_id: u64,
    next_split_id: u64,
    next_group_id: u64,
    modules_created: u64,
    /// The last operation's error and when it occurred; the badge drops it after
    /// `MESSAGE_LIFETIME`.
    control_error: Option<(String, Duration)>,
    /// Shared repaint clock of animating windows (real time).
    frame_clock: Option<std::time::Instant>,
    /// The preset the session was loaded from, if it still has one.
    preset: Option<PresetRef>,
    store: Option<WorkspaceStore>,
    /// The session file could not be read; saving must not replace it.
    protected: bool,
    /// Monitors, pointer state and native window moves; owned so restoring and
    /// saving can place windows.
    desktop: Arc<dyn DesktopProbe + Send + Sync>,
    /// Groups last told to the desktop (`DesktopProbe::set_groups`).
    published_groups: Vec<(WindowId, u64)>,
    theme: ThemeSettings,
    /// Colors of the window chrome (global roles).
    chrome: Palette,
    input: InputConfig,
    /// Display reference level this workspace sets (`None`: the default).
    reference_dbfs: Option<f32>,
    /// Restore and save problems, listed in the settings until cleared.
    notices: Vec<String>,
    /// The status badge shows the notices until this display time (a new notice
    /// extends it by `MESSAGE_LIFETIME`).
    notices_shown_until: Duration,
    /// File number of each binding of this workspace (restored or saved).
    binding_numbers: HashMap<SharedAnalysisId, u64>,
    /// The settings window, while open.
    settings: Option<SettingsWindow>,
    /// Openings of the settings window so far.
    settings_openings: u64,
    /// Latest display time of any pass (see `tick`).
    display_time: Duration,
    /// Declared last: the source and worker stop only after every Module dropped.
    live: Option<LiveAudio>,
}

impl Workspace {
    fn empty(
        live: LiveAudio,
        desktop: Arc<dyn DesktopProbe + Send + Sync>,
    ) -> Result<Self, WorkspaceError> {
        let mut registry = Registry::default();
        modules::register(&mut registry)?;
        let services = ModuleServices {
            source: live.reader(),
            dataflow: live.dataflow_handle(),
        };
        let theme = ThemeSettings::dark();
        Ok(Self {
            modules: HashMap::new(),
            windows: BTreeMap::new(),
            registry,
            services,
            move_source: None,
            next_instance_id: 1,
            next_window_id: FIRST_WINDOW.0,
            next_split_id: 1,
            next_group_id: 1,
            modules_created: 0,
            control_error: None,
            notices_shown_until: Duration::ZERO,
            frame_clock: None,
            preset: None,
            store: None,
            protected: false,
            desktop,
            published_groups: Vec::new(),
            chrome: theme.global_palette(),
            theme,
            input: InputConfig::default(),
            reference_dbfs: None,
            notices: Vec::new(),
            binding_numbers: HashMap::new(),
            settings: None,
            settings_openings: 0,
            display_time: Duration::ZERO,
            live: Some(live),
        })
    }

    /// A workspace with one window holding one default Spectrum, not saved, on the
    /// native desktop (automation only; the product opens its session).
    #[cfg(feature = "automation")]
    pub fn new(live: LiveAudio, always_on_top: bool) -> Result<Self, WorkspaceError> {
        let mut workspace = Self::empty(live, Arc::new(NativeDesktop))?;
        let first = workspace.create_module(modules::SPECTRUM_MODULE_ID)?;
        let window = WindowState::new(Layout::with_leaf(first), INITIAL_WINDOW_SIZE, always_on_top);
        let id = WindowId(Self::allocate(&mut workspace.next_window_id)?);
        workspace.windows.insert(id, window);
        Ok(workspace)
    }

    /// Replace the desktop the workspace places its windows on (simulated
    /// desktops of the automation harness).
    #[cfg(feature = "automation")]
    pub(crate) fn set_desktop(&mut self, desktop: Arc<dyn DesktopProbe + Send + Sync>) {
        self.desktop = desktop;
        self.published_groups.clear();
        for window in self.windows.values_mut() {
            window.tracked = false;
        }
    }

    /// Drop every Module before stopping capture and the worker.
    pub(crate) fn shutdown(&mut self) {
        self.settings = None;
        self.windows.clear();
        self.modules.clear();
        self.live.take();
    }

    pub(crate) fn windows(&self) -> Vec<WindowSummary> {
        self.windows
            .iter()
            .map(|(id, window)| WindowSummary {
                id: *id,
                builder: window.viewport_builder(*id),
            })
            .collect()
    }

    /// Callback run on the analysis worker after it publishes new results.
    pub(crate) fn set_publication_waker(&self, waker: Option<crate::dataflow::PublicationWaker>) {
        self.services.dataflow.set_publication_waker(waker);
    }

    pub(crate) fn has_window(&self, id: WindowId) -> bool {
        self.windows.contains_key(&id)
    }

    /// Tell each instance whether its window can be seen, when that changed.
    fn sync_visibility(&mut self) {
        for window in self.windows.values() {
            for leaf in window.layout.leaves() {
                if let Some(entry) = self.modules.get_mut(&leaf)
                    && entry.visible == window.minimized
                {
                    entry.visible = !window.minimized;
                    entry.module.set_visible(entry.visible);
                }
            }
        }
    }

    pub(crate) fn source_status(&self) -> SourceStatus {
        self.services.source.source_status()
    }

    fn allocate(counter: &mut u64) -> Result<u64, WorkspaceError> {
        let id = *counter;
        *counter = counter.checked_add(1).ok_or(WorkspaceError::IdExhausted)?;
        Ok(id)
    }

    /// Re-resolve every instance's colors after a theme or override change.
    fn refresh_palettes(&mut self) {
        self.chrome = self.theme.global_palette();
        for entry in self.modules.values_mut() {
            entry.palette = self.theme.resolve(entry.type_id, &entry.colors);
        }
    }

    fn push_notice(&mut self, notice: String) {
        if !self.notices.contains(&notice) {
            self.notices.push(notice);
            self.notices_shown_until = self.display_time + MESSAGE_LIFETIME;
        }
    }

    fn set_control_error(&mut self, message: String) {
        // Error texts follow Rust's lowercase convention; on screen they start a
        // sentence.
        let mut chars = message.chars();
        let message = chars.next().map_or_else(String::new, |first| {
            first.to_uppercase().chain(chars).collect()
        });
        self.control_error = Some((message, self.display_time));
    }

    /// The notices the status badge reports now.
    fn badge_notices(&self) -> &[String] {
        if self.display_time < self.notices_shown_until {
            &self.notices
        } else {
            &[]
        }
    }

    /// Display time until the badge next drops a message, if it shows one.
    pub(crate) fn badge_change_in(&self) -> Option<Duration> {
        let error_until = self
            .control_error
            .as_ref()
            .map(|(_, at)| *at + MESSAGE_LIFETIME);
        let notices_until = (!self.notices.is_empty()
            && self.display_time < self.notices_shown_until)
            .then_some(self.notices_shown_until);
        let until = error_until.into_iter().chain(notices_until).min()?;
        Some(until.saturating_sub(self.display_time))
    }

    fn window_of(&self, instance: ModuleInstanceId) -> Option<WindowId> {
        self.windows
            .iter()
            .find(|(_, window)| window.layout.contains(instance))
            .map(|(id, _)| *id)
    }

    fn located(&self, instance: ModuleInstanceId) -> Result<WindowId, WorkspaceError> {
        if !self.modules.contains_key(&instance) {
            return Err(WorkspaceError::StaleInstance(instance));
        }
        self.window_of(instance)
            .ok_or(WorkspaceError::StaleInstance(instance))
    }

    fn window(&self, id: WindowId) -> Result<&WindowState, WorkspaceError> {
        self.windows.get(&id).ok_or(WorkspaceError::StaleWindow(id))
    }

    fn window_mut(&mut self, id: WindowId) -> Result<&mut WindowState, WorkspaceError> {
        self.windows
            .get_mut(&id)
            .ok_or(WorkspaceError::StaleWindow(id))
    }

    pub(crate) fn module_min_size(&self, id: ModuleInstanceId) -> Vec2 {
        self.modules
            .get(&id)
            .map_or(EMPTY_WINDOW_MIN_SIZE, |entry| {
                let size = entry.module.min_size();
                vec2(size.width, size.height)
            })
    }

    fn window_min_size(&self, window: &WindowState) -> Vec2 {
        window
            .layout
            .min_size(&|id| self.module_min_size(id))
            .unwrap_or(EMPTY_WINDOW_MIN_SIZE)
    }

    fn leaf_rect(&self, window_id: WindowId, instance: ModuleInstanceId) -> Option<Rect> {
        let window = self.windows.get(&window_id)?;
        window
            .layout
            .geometry(
                Rect::from_min_size(Pos2::ZERO, window.content_size),
                &|id| self.module_min_size(id),
            )
            .leaf_rect(instance)
    }

    pub(crate) fn apply_action(
        &mut self,
        action: WorkspaceAction,
    ) -> Result<ActionOutcome, WorkspaceError> {
        let result = self.apply(action);
        self.sync_visibility();
        match &result {
            Ok(_) => self.control_error = None,
            Err(error) => self.set_control_error(error.to_string()),
        }
        result
    }

    fn apply(&mut self, action: WorkspaceAction) -> Result<ActionOutcome, WorkspaceError> {
        match action {
            WorkspaceAction::AddModule { window_id, type_id } => {
                self.add_module(window_id, type_id)
            }
            WorkspaceAction::SplitModule { instance_id, side } => {
                self.split_module(instance_id, side)
            }
            WorkspaceAction::ReplaceModule {
                instance_id,
                type_id,
            } => self.replace_module(instance_id, type_id),
            WorkspaceAction::CloseModule { instance_id } => self.close_module(instance_id),
            WorkspaceAction::SelectModule { instance_id } => {
                let window_id = self.located(instance_id)?;
                self.select_in_settings(window_id, Some(instance_id))?;
                Ok(ActionOutcome::Applied)
            }
            WorkspaceAction::SelectWindow { window_id } => {
                self.select_in_settings(window_id, None)?;
                Ok(ActionOutcome::Applied)
            }
            WorkspaceAction::SetChannel {
                instance_id,
                channel,
            } => self.set_channel(instance_id, channel),
            WorkspaceAction::SetStyle {
                instance_id,
                style_id,
            } => self.set_style(instance_id, style_id),
            WorkspaceAction::ClearVisualState { instance_id } => {
                self.clear_visual_state(instance_id)
            }
            WorkspaceAction::TogglePause => self.toggle_pause(),
            WorkspaceAction::SetSplitRatio {
                window_id,
                split_id,
                ratio,
                manual,
            } => self.set_split_ratio(window_id, split_id, ratio, manual),
            WorkspaceAction::ResetSplit {
                window_id,
                split_id,
            } => self.reset_split(window_id, split_id),
            WorkspaceAction::DetachModule { instance_id } => self.detach(instance_id),
            WorkspaceAction::CancelDetach { window_id } => self.cancel_detach(window_id),
            WorkspaceAction::BeginMove { instance_id } => {
                self.located(instance_id)?;
                self.move_source = Some(instance_id);
                Ok(ActionOutcome::Applied)
            }
            WorkspaceAction::CancelMove => {
                if self.move_source.take().is_none() {
                    return Err(WorkspaceError::Unsupported("cancel without a pending move"));
                }
                Ok(ActionOutcome::Applied)
            }
            WorkspaceAction::MoveModule {
                instance_id,
                target,
            } => self.move_module(instance_id, target),
            WorkspaceAction::NewWindow { window_id } => self.new_window(window_id),
            WorkspaceAction::CloseWindow { window_id } => self.close_window_action(window_id),
            WorkspaceAction::SetAlwaysOnTop { window_id, enabled } => {
                self.set_always_on_top(window_id, enabled)
            }
            WorkspaceAction::SetLocked { window_id, enabled } => {
                self.set_locked(window_id, enabled)
            }
            WorkspaceAction::ToggleMaximized { window_id } => self.toggle_maximized(window_id),
            WorkspaceAction::GroupTouching { window_id } => self.group_touching(window_id),
            WorkspaceAction::LeaveGroup { window_id } => self.leave_group(window_id),
            WorkspaceAction::EditNode {
                instance_id,
                node,
                spec,
            } => self.edit_node(instance_id, node, spec),
            WorkspaceAction::SetKeep {
                instance_id,
                output,
                seconds,
            } => self.set_keep(instance_id, output, seconds),
            WorkspaceAction::OpenSettings {
                window_id,
                instance_id,
            } => {
                self.open_settings(window_id, instance_id)?;
                Ok(ActionOutcome::Applied)
            }
            WorkspaceAction::CloseSettings => {
                if self.settings.take().is_none() {
                    return Err(WorkspaceError::Unsupported(
                        "closing settings that are not open",
                    ));
                }
                Ok(ActionOutcome::Applied)
            }
            WorkspaceAction::SetViewSetting {
                instance_id,
                key,
                value,
            } => self.set_view_setting(instance_id, key, value),
            WorkspaceAction::SetColor { scope, role, color } => self.set_color(scope, role, color),
            WorkspaceAction::SetThemePreset { preset } => {
                self.theme.preset = preset.id().to_owned();
                self.refresh_palettes();
                Ok(ActionOutcome::Applied)
            }
            WorkspaceAction::SetTextSize { size } => self.set_text_size(size),
            WorkspaceAction::SetReferenceLevel { dbfs } => self.set_reference_level(dbfs),
            WorkspaceAction::LoadPreset { preset } => self.load_preset(preset),
            WorkspaceAction::SavePresetAs { name } => self.save_preset_as(name),
            WorkspaceAction::SavePreset => self.save_preset(),
            WorkspaceAction::DeletePreset { name } => self.delete_preset(&name),
            WorkspaceAction::ClearNotices => {
                self.notices.clear();
                Ok(ActionOutcome::Applied)
            }
            WorkspaceAction::Quit => self.quit(),
        }
    }

    fn toggle_pause(&mut self) -> Result<ActionOutcome, WorkspaceError> {
        if !matches!(
            self.services.source.source_status().state,
            SourceState::Running | SourceState::Paused
        ) {
            return Err(WorkspaceError::Unsupported(
                "source pause or resume in its current state",
            ));
        }
        self.live
            .as_mut()
            .ok_or(WorkspaceError::Unsupported("pause after shutdown"))?
            .toggle_pause()
            .map_err(WorkspaceError::LiveAudio)?;
        Ok(ActionOutcome::Applied)
    }

    fn set_color(
        &mut self,
        scope: ColorScope,
        role: ColorRole,
        color: Option<Color>,
    ) -> Result<ActionOutcome, WorkspaceError> {
        let overrides = match scope {
            ColorScope::Global => &mut self.theme.colors,
            ColorScope::Type(type_id) => {
                if self.registry.module_type(type_id).is_none() {
                    return Err(RegistryError::UnknownModule(type_id.to_owned()).into());
                }
                self.theme
                    .type_styles
                    .entry(type_id.to_owned())
                    .or_default()
            }
            ColorScope::Instance(instance_id) => {
                self.located(instance_id)?;
                &mut self
                    .modules
                    .get_mut(&instance_id)
                    .ok_or(WorkspaceError::StaleInstance(instance_id))?
                    .colors
            }
        };
        match color {
            Some(color) => {
                overrides.insert(role, color);
            }
            None => {
                overrides.remove(&role);
            }
        }
        self.theme.type_styles.retain(|_, style| !style.is_empty());
        self.refresh_palettes();
        Ok(ActionOutcome::Applied)
    }

    fn set_text_size(&mut self, size: Option<f32>) -> Result<ActionOutcome, WorkspaceError> {
        if size.is_some_and(|size| !size.is_finite()) {
            return Err(WorkspaceError::Unsupported("a non-finite text size"));
        }
        let (min, max) = crate::theme::TEXT_SIZE_RANGE;
        self.theme.text_size = size.map(|size| size.clamp(min, max));
        self.refresh_palettes();
        Ok(ActionOutcome::Applied)
    }

    fn set_reference_level(&mut self, dbfs: Option<f32>) -> Result<ActionOutcome, WorkspaceError> {
        if dbfs.is_some_and(|dbfs| !dbfs.is_finite()) {
            return Err(WorkspaceError::Unsupported("a non-finite reference level"));
        }
        let (min, max) = crate::module::REFERENCE_DBFS_RANGE;
        self.reference_dbfs = dbfs.map(|dbfs| dbfs.clamp(min, max));
        Ok(ActionOutcome::Applied)
    }

    /// The reference level every Module draws against (see `ModuleFrame`).
    pub(crate) fn reference_dbfs(&self) -> f32 {
        let (min, max) = crate::module::REFERENCE_DBFS_RANGE;
        self.reference_dbfs
            .unwrap_or(crate::module::DEFAULT_REFERENCE_DBFS)
            .clamp(min, max)
    }

    fn quit(&mut self) -> Result<ActionOutcome, WorkspaceError> {
        self.autosave();
        self.settings = None;
        self.windows.clear();
        self.modules.clear();
        self.move_source = None;
        Ok(ActionOutcome::Applied)
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        self.shutdown();
    }
}
