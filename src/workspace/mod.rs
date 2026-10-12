//! Workspace: owns the application's Module instances, the windows and each
//! window's BSP layout. Layout trees only reference instances by ID, so every
//! layout operation (split, move, detach, merge) leaves the Module objects and
//! their analysis subscriptions untouched; only closing an instance drops it.

mod desktop;
mod layout;
mod persist;
mod settings_window;
mod window;

use std::{
    collections::{BTreeMap, HashMap},
    time::Duration,
};

use eframe::egui::{self, Pos2, Rect, Vec2, ViewportCommand, pos2, vec2};

use crate::{
    LiveAudio,
    audio::{SourceState, SourceStatus},
    dataflow::{
        ChannelSelection, ControlError, Keep, NodeRef, NodeSpec, OutputId, SharedAnalysisId,
    },
    extensions::{Registry, RegistryError},
    module::{Module, ModuleServices},
    modules::{self, SPECTRUM_MODULE_ID},
    persistence::{InputConfig, ModuleFile, PersistError, WorkspaceStore},
    render::Color,
    settings::SettingValue,
    theme::{ColorRole, Palette, Preset, RoleOverrides, ThemeSettings},
};

pub(crate) use desktop::{DesktopProbe, NativeDesktop};
#[cfg(any(test, feature = "automation"))]
pub(crate) use layout::Axis;
use layout::{Layout, LayoutError, fits};
pub(crate) use layout::{Side, SplitId};
use settings_window::SettingsWindow;
#[cfg(feature = "automation")]
pub(crate) use settings_window::{SWATCHES, SettingsItem, Tab};
use window::WindowUiState;
#[cfg(feature = "automation")]
pub(crate) use window::{ControlKey, ControlTarget, ResizeEdge};

/// Initial logical size of the first window.
pub(crate) const INITIAL_WINDOW_SIZE: Vec2 = vec2(900.0, 420.0);
/// Minimum logical size of a window without any Module.
pub(crate) const EMPTY_WINDOW_MIN_SIZE: Vec2 = vec2(140.0, 48.0);
/// Logical offset of a detached window from its source window.
const DETACH_OFFSET_POINTS: f32 = 32.0;
/// Display time a detached window may take to show its first frame.
pub(crate) const DETACH_TIMEOUT: Duration = Duration::from_secs(5);

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
    /// Create a default instance of the same type next to `instance_id`.
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
    SelectModule {
        instance_id: ModuleInstanceId,
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
    CloseWindow {
        window_id: WindowId,
    },
    SetAlwaysOnTop {
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
    /// Copy configuration, analysis spec and bindings into a new instance on the
    /// right; the copy gets fresh private state.
    DuplicateModule {
        instance_id: ModuleInstanceId,
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
    SetTextSize {
        size: f32,
    },
    SaveWorkspace,
    SaveWorkspaceAs {
        name: String,
    },
    /// Save the current workspace, then replace it with the saved one.
    SwitchWorkspace {
        name: String,
    },
    /// Save the current workspace, then start a new default one.
    NewWorkspace {
        name: String,
    },
    ClearNotices,
    /// Replace the current workspace with one default Spectrum window and the Dark
    /// theme, keeping its name, and save it.
    ResetWorkspace,
    /// Save the workspace and close every window (the application exits).
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
    /// Its file could not be read; saving would overwrite it.
    Protected(String),
    WorkspaceExists(String),
    NoSuchWorkspace(String),
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
            Self::Protected(name) => write!(
                f,
                "workspace {name:?} could not be read; save it under another name"
            ),
            Self::WorkspaceExists(name) => write!(f, "workspace {name:?} already exists"),
            Self::NoSuchWorkspace(name) => write!(f, "workspace {name:?} is not saved"),
            Self::CopyIncomplete(reason) => write!(f, "could not copy the module: {reason}"),
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
    preserved: Option<ModuleFile>,
    /// Saved style this build does not provide; kept until the user picks one.
    preserved_style: Option<String>,
}

#[derive(Clone, Copy, Debug)]
struct PendingDetach {
    instance_id: ModuleInstanceId,
    source: WindowId,
    requested_at: Duration,
}

/// Desktop placement observed from the native window, in physical pixels.
#[derive(Clone, Copy, Debug, Default)]
struct DesktopState {
    outer: Option<Rect>,
    /// Position this application commanded and has not seen applied yet.
    expected: Option<Pos2>,
    /// The user moved the window and the move has not been snapped yet.
    moving: bool,
    /// This window asked the OS to start a native move loop (body drag).
    native_drag: bool,
}

pub(crate) struct WindowState {
    layout: Layout,
    active: Option<ModuleInstanceId>,
    always_on_top: bool,
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
    pending_detach: Option<PendingDetach>,
    opened: bool,
    desktop: DesktopState,
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
            active: None,
            always_on_top,
            maximized: false,
            content_size: size,
            pixels_per_point: 1.0,
            initial_position: None,
            initial_size: size,
            initial_min_size: EMPTY_WINDOW_MIN_SIZE,
            initial_always_on_top: always_on_top,
            pending_detach: None,
            opened: false,
            desktop: DesktopState::default(),
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
            .with_inner_size(self.initial_size)
            .with_min_inner_size(self.initial_min_size)
            .with_decorations(false);
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

#[cfg(any(test, feature = "automation"))]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ObservedModule {
    pub(crate) instance_id: ModuleInstanceId,
    pub(crate) type_id: &'static str,
    pub(crate) window_id: Option<WindowId>,
    pub(crate) channel: Option<ChannelSelection>,
    pub(crate) active_style: Option<&'static str>,
    /// The applied analysis spec.
    pub(crate) data: Option<crate::dataflow::DataSpec>,
    pub(crate) view: Vec<(&'static str, SettingValue)>,
    pub(crate) colors: RoleOverrides,
    pub(crate) palette: Palette,
    /// Saved type of an instance that could not be restored.
    pub(crate) missing: Option<String>,
    /// Address of the Module object; unchanged while the same object is kept.
    pub(crate) object: usize,
}

#[cfg(any(test, feature = "automation"))]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ObservedSplit {
    pub(crate) id: SplitId,
    pub(crate) axis: Axis,
    pub(crate) ratio: f32,
    pub(crate) manual: bool,
}

#[cfg(any(test, feature = "automation"))]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ObservedWindow {
    pub(crate) id: WindowId,
    pub(crate) leaves: Vec<ModuleInstanceId>,
    pub(crate) leaf_rects: Vec<(ModuleInstanceId, Rect)>,
    pub(crate) splits: Vec<ObservedSplit>,
    pub(crate) active: Option<ModuleInstanceId>,
    pub(crate) always_on_top: bool,
    pub(crate) group: Option<u64>,
    pub(crate) opened: bool,
    pub(crate) pending_detach: Option<ModuleInstanceId>,
    pub(crate) min_size: Vec2,
    pub(crate) divider_drag: Option<SplitId>,
}

#[cfg(any(test, feature = "automation"))]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WorkspaceObservation {
    pub(crate) windows: Vec<ObservedWindow>,
    pub(crate) modules: Vec<ObservedModule>,
    pub(crate) move_source: Option<ModuleInstanceId>,
    pub(crate) source_status: SourceStatus,
    pub(crate) control_error: Option<String>,
    pub(crate) modules_created: u64,
    pub(crate) name: String,
    pub(crate) notices: Vec<String>,
    pub(crate) theme: ThemeSettings,
}

#[cfg(any(test, feature = "automation"))]
impl WorkspaceObservation {
    pub(crate) fn window(&self, id: WindowId) -> Option<&ObservedWindow> {
        self.windows.iter().find(|window| window.id == id)
    }

    pub(crate) fn module(&self, id: ModuleInstanceId) -> Option<&ObservedModule> {
        self.modules.iter().find(|module| module.instance_id == id)
    }
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
    control_error: Option<String>,
    /// Shared repaint clock of animating windows (real time).
    frame_clock: Option<std::time::Instant>,
    /// Name under which the workspace saves.
    name: String,
    store: Option<WorkspaceStore>,
    /// The file under `name` could not be read; saving must not replace it.
    protected: bool,
    theme: ThemeSettings,
    /// Colors of the window chrome (global roles).
    chrome: Palette,
    input: InputConfig,
    /// Restore and save problems shown to the user until cleared.
    notices: Vec<String>,
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
    fn empty(live: LiveAudio) -> Result<Self, WorkspaceError> {
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
            frame_clock: None,
            name: crate::persistence::DEFAULT_WORKSPACE.to_owned(),
            store: None,
            protected: false,
            chrome: theme.global_palette(),
            theme,
            input: InputConfig::default(),
            notices: Vec::new(),
            binding_numbers: HashMap::new(),
            settings: None,
            settings_openings: 0,
            display_time: Duration::ZERO,
            live: Some(live),
        })
    }

    /// A workspace with one window holding one default Spectrum, not saved
    /// (automation and tests; the product opens its saved workspace).
    #[cfg(any(test, feature = "automation"))]
    pub fn new(live: LiveAudio, always_on_top: bool) -> Result<Self, WorkspaceError> {
        let mut workspace = Self::empty(live)?;
        workspace.add_default_window(always_on_top)?;
        Ok(workspace)
    }

    /// One window with one default Spectrum.
    fn add_default_window(&mut self, always_on_top: bool) -> Result<WindowId, WorkspaceError> {
        let first = self.create_module(SPECTRUM_MODULE_ID)?;
        let mut window =
            WindowState::new(Layout::with_leaf(first), INITIAL_WINDOW_SIZE, always_on_top);
        window.active = Some(first);
        let id = WindowId(Self::allocate(&mut self.next_window_id)?);
        self.windows.insert(id, window);
        Ok(id)
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

    /// Report whether a native window is minimized. Instances in a minimized window
    /// may stop work that only serves the display; their analysis keeps running.
    pub(crate) fn set_window_minimized(&mut self, id: WindowId, minimized: bool) {
        if let Some(window) = self.windows.get_mut(&id)
            && window.minimized != minimized
        {
            window.minimized = minimized;
            self.sync_visibility();
        }
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

    /// Create a default instance through the factory.
    fn create_module(&mut self, type_id: &str) -> Result<ModuleInstanceId, WorkspaceError> {
        self.create_instance(type_id, None)
    }

    /// Create through the factory, with the saved analysis `data` when given; the
    /// ID is consumed only on success.
    fn create_instance(
        &mut self,
        type_id: &str,
        data: Option<&crate::dataflow::DataSpec>,
    ) -> Result<ModuleInstanceId, WorkspaceError> {
        let next = self
            .next_instance_id
            .checked_add(1)
            .ok_or(WorkspaceError::IdExhausted)?;
        let (type_id, label) = self
            .registry
            .module_type(type_id)
            .ok_or_else(|| RegistryError::UnknownModule(type_id.to_owned()))?;
        let module = self.registry.create_module(type_id, &self.services, data)?;
        let id = ModuleInstanceId(self.next_instance_id);
        self.next_instance_id = next;
        self.insert_entry(id, type_id, label, module, None);
        Ok(id)
    }

    fn insert_entry(
        &mut self,
        id: ModuleInstanceId,
        type_id: &'static str,
        label: &'static str,
        module: Box<dyn Module>,
        preserved: Option<ModuleFile>,
    ) {
        self.modules_created += 1;
        self.modules.insert(
            id,
            ModuleEntry {
                type_id,
                label,
                module,
                visible: true,
                colors: RoleOverrides::new(),
                palette: self.theme.resolve(type_id, &RoleOverrides::new()),
                preserved,
                preserved_style: None,
            },
        );
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
        }
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

    /// Whether `incoming` can be placed on `side` of `target` in its current area.
    fn can_place_beside(&self, target: ModuleInstanceId, side: Side, incoming_min: Vec2) -> bool {
        let Some(window_id) = self.window_of(target) else {
            return false;
        };
        let Some(rect) = self.leaf_rect(window_id, target) else {
            return false;
        };
        fits(
            rect.size(),
            side.axis(),
            self.module_min_size(target),
            incoming_min,
        )
    }

    pub(crate) fn can_split(&self, instance: ModuleInstanceId, side: Side) -> bool {
        self.can_place_beside(instance, side, self.module_min_size(instance))
    }

    pub(crate) fn move_target_valid(&self, source: ModuleInstanceId, target: MoveTarget) -> bool {
        if !self.modules.contains_key(&source) || self.window_of(source).is_none() {
            return false;
        }
        match target {
            MoveTarget::Beside { instance_id, side } => {
                instance_id != source
                    && self.can_place_beside(instance_id, side, self.module_min_size(source))
            }
            MoveTarget::EmptyWindow(window_id) => self
                .windows
                .get(&window_id)
                .is_some_and(|window| window.layout.is_empty() && window.pending_detach.is_none()),
        }
    }

    fn select_after_removal(window: &mut WindowState, removed: ModuleInstanceId) {
        if window.active == Some(removed) || window.active.is_none() {
            window.active = window.layout.leaves().first().copied();
        }
    }

    pub(crate) fn apply_action(
        &mut self,
        action: WorkspaceAction,
    ) -> Result<ActionOutcome, WorkspaceError> {
        let result = self.apply(action);
        self.sync_visibility();
        match &result {
            Ok(_) => self.control_error = None,
            Err(error) => self.control_error = Some(error.to_string()),
        }
        result
    }

    fn apply(&mut self, action: WorkspaceAction) -> Result<ActionOutcome, WorkspaceError> {
        match action {
            WorkspaceAction::AddModule { window_id, type_id } => {
                let window = self.window(window_id)?;
                if !window.layout.is_empty() || window.pending_detach.is_some() {
                    return Err(WorkspaceError::Layout(LayoutError::NotEmpty));
                }
                let id = self.create_module(type_id)?;
                let window = self.window_mut(window_id)?;
                window.layout.place_root(id)?;
                window.active = Some(id);
                Ok(ActionOutcome::Created(id))
            }
            WorkspaceAction::SplitModule { instance_id, side } => {
                let window_id = self.located(instance_id)?;
                if !self.can_split(instance_id, side) {
                    return Err(WorkspaceError::InsufficientSpace("split this module"));
                }
                let type_id = self.modules[&instance_id].type_id;
                let split_id = SplitId(self.next_split_id);
                let next_split = self
                    .next_split_id
                    .checked_add(1)
                    .ok_or(WorkspaceError::IdExhausted)?;
                let new = self.create_module(type_id)?;
                let window = self.window_mut(window_id)?;
                if let Err(error) = window
                    .layout
                    .insert_beside(instance_id, side, new, split_id)
                {
                    self.modules.remove(&new);
                    return Err(error.into());
                }
                window.active = Some(new);
                self.next_split_id = next_split;
                Ok(ActionOutcome::Created(new))
            }
            WorkspaceAction::ReplaceModule {
                instance_id,
                type_id,
            } => {
                let window_id = self.located(instance_id)?;
                let new = self.create_module(type_id)?;
                let window = self.window_mut(window_id)?;
                if let Err(error) = window.layout.replace_leaf(instance_id, new) {
                    self.modules.remove(&new);
                    return Err(error.into());
                }
                if window.active == Some(instance_id) {
                    window.active = Some(new);
                }
                if self.move_source == Some(instance_id) {
                    self.move_source = None;
                }
                self.modules.remove(&instance_id);
                if let Some(settings) = &mut self.settings {
                    settings.follow_replacement(instance_id, new);
                }
                Ok(ActionOutcome::Created(new))
            }
            WorkspaceAction::CloseModule { instance_id } => {
                let window_id = self.located(instance_id)?;
                let window = self.window_mut(window_id)?;
                window.layout.remove_leaf(instance_id)?;
                Self::select_after_removal(window, instance_id);
                if self.move_source == Some(instance_id) {
                    self.move_source = None;
                }
                self.modules.remove(&instance_id);
                Ok(ActionOutcome::Applied)
            }
            WorkspaceAction::SelectModule { instance_id } => {
                let window_id = self.located(instance_id)?;
                self.window_mut(window_id)?.active = Some(instance_id);
                Ok(ActionOutcome::Applied)
            }
            WorkspaceAction::SetChannel {
                instance_id,
                channel,
            } => {
                self.located(instance_id)?;
                let module = &mut self
                    .modules
                    .get_mut(&instance_id)
                    .ok_or(WorkspaceError::StaleInstance(instance_id))?
                    .module;
                let current = module
                    .channel()
                    .ok_or(WorkspaceError::Unsupported("channel selection"))?;
                if current != channel {
                    module.set_channel(channel)?;
                }
                Ok(ActionOutcome::Applied)
            }
            WorkspaceAction::SetStyle {
                instance_id,
                style_id,
            } => {
                self.located(instance_id)?;
                let entry = self
                    .modules
                    .get_mut(&instance_id)
                    .ok_or(WorkspaceError::StaleInstance(instance_id))?;
                let options = entry.module.style_options();
                if options.is_empty() {
                    return Err(WorkspaceError::Unsupported("style selection"));
                }
                if !options.iter().any(|option| option.id == style_id) {
                    return Err(RegistryError::UnknownStyle(style_id.to_owned()).into());
                }
                entry.module.set_style(style_id, &self.registry)?;
                entry.preserved_style = None;
                Ok(ActionOutcome::Applied)
            }
            WorkspaceAction::ClearVisualState { instance_id } => {
                self.located(instance_id)?;
                let entry = self
                    .modules
                    .get_mut(&instance_id)
                    .ok_or(WorkspaceError::StaleInstance(instance_id))?;
                if entry.module.style_options().is_empty() {
                    return Err(WorkspaceError::Unsupported("visual state clearing"));
                }
                entry.module.clear_visual_state();
                Ok(ActionOutcome::Applied)
            }
            WorkspaceAction::TogglePause => {
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
            WorkspaceAction::SetSplitRatio {
                window_id,
                split_id,
                ratio,
                manual,
            } => {
                self.window_mut(window_id)?
                    .layout
                    .set_ratio(split_id, ratio, manual)?;
                Ok(ActionOutcome::Applied)
            }
            WorkspaceAction::ResetSplit {
                window_id,
                split_id,
            } => {
                self.window_mut(window_id)?.layout.reset_chain(split_id)?;
                Ok(ActionOutcome::Applied)
            }
            WorkspaceAction::DetachModule { instance_id } => self.detach(instance_id),
            WorkspaceAction::CancelDetach { window_id } => {
                let window = self.window(window_id)?;
                if window.pending_detach.is_none() || window.opened {
                    return Err(WorkspaceError::Unsupported(
                        "cancel a detach that is not pending",
                    ));
                }
                self.windows.remove(&window_id);
                Ok(ActionOutcome::Applied)
            }
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
            WorkspaceAction::CloseWindow { window_id } => {
                self.window(window_id)?;
                if self.windows.len() == 1 {
                    // Closing the last window ends the session: keep its layout.
                    self.autosave();
                }
                self.close_window(window_id)?;
                Ok(ActionOutcome::Applied)
            }
            WorkspaceAction::SetAlwaysOnTop { window_id, enabled } => {
                let window = self.window_mut(window_id)?;
                window.always_on_top = enabled;
                window
                    .commands
                    .push(ViewportCommand::WindowLevel(if enabled {
                        egui::WindowLevel::AlwaysOnTop
                    } else {
                        egui::WindowLevel::Normal
                    }));
                Ok(ActionOutcome::Applied)
            }
            WorkspaceAction::ToggleMaximized { window_id } => {
                let window = self.window_mut(window_id)?;
                let maximized = !window.maximized;
                window.commands.push(ViewportCommand::Maximized(maximized));
                Ok(ActionOutcome::Applied)
            }
            WorkspaceAction::GroupTouching { window_id } => self.group_touching(window_id),
            WorkspaceAction::LeaveGroup { window_id } => {
                let group = self
                    .window(window_id)?
                    .group
                    .ok_or(WorkspaceError::Unsupported("leave group without a group"))?;
                self.window_mut(window_id)?.group = None;
                let remaining: Vec<WindowId> = self
                    .windows
                    .iter()
                    .filter(|(_, window)| window.group == Some(group))
                    .map(|(id, _)| *id)
                    .collect();
                if remaining.len() < 2 {
                    for id in remaining {
                        if let Some(window) = self.windows.get_mut(&id) {
                            window.group = None;
                        }
                    }
                }
                Ok(ActionOutcome::Applied)
            }
            WorkspaceAction::DuplicateModule { instance_id } => self.duplicate(instance_id),
            WorkspaceAction::EditNode {
                instance_id,
                node,
                spec,
            } => {
                // Each instance owns its analysis: an edit changes only this instance.
                // The dataflow keeps sharing whatever stays identical.
                let controller = self.controller(instance_id)?;
                let same_kind = controller.with_spec(|applied| {
                    applied.node(node).is_some_and(|current| {
                        std::mem::discriminant(&current) == std::mem::discriminant(&spec)
                    })
                });
                if !same_kind {
                    return Err(WorkspaceError::Unsupported("changing a node's kind"));
                }
                controller.update(|applied| applied.set_node(node, spec))?;
                self.reconcile_views();
                Ok(ActionOutcome::Applied)
            }
            WorkspaceAction::SetKeep {
                instance_id,
                output,
                seconds,
            } => {
                let controller = self.controller(instance_id)?;
                let current =
                    controller.with_spec(|spec| spec.outputs().get(output.index()).map(|o| o.keep));
                if !matches!(current, Some(Keep::Seconds(_))) {
                    return Err(WorkspaceError::Unsupported("history length of this output"));
                }
                controller.update(|spec| spec.set_keep(output, Keep::Seconds(seconds)))?;
                self.reconcile_views();
                Ok(ActionOutcome::Applied)
            }
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
            } => {
                self.located(instance_id)?;
                let entry = self
                    .modules
                    .get_mut(&instance_id)
                    .ok_or(WorkspaceError::StaleInstance(instance_id))?;
                let setting = entry
                    .module
                    .view_settings()
                    .into_iter()
                    .find(|setting| setting.key == key)
                    .ok_or_else(|| crate::module::ModuleError::UnknownSetting(key.to_owned()))?;
                let value = setting
                    .accept(&value)
                    .ok_or_else(|| crate::module::ModuleError::InvalidSetting(key.to_owned()))?;
                entry.module.set_view_setting(key, &value)?;
                Ok(ActionOutcome::Applied)
            }
            WorkspaceAction::SetColor { scope, role, color } => {
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
            WorkspaceAction::SetThemePreset { preset } => {
                self.theme.preset = preset.id().to_owned();
                self.refresh_palettes();
                Ok(ActionOutcome::Applied)
            }
            WorkspaceAction::SetTextSize { size } => {
                if !size.is_finite() {
                    return Err(WorkspaceError::Unsupported("a non-finite text size"));
                }
                let (min, max) = crate::theme::TEXT_SIZE_RANGE;
                self.theme.text_size = Some(size.clamp(min, max));
                self.refresh_palettes();
                Ok(ActionOutcome::Applied)
            }
            WorkspaceAction::SaveWorkspace => {
                self.save()?;
                Ok(ActionOutcome::Applied)
            }
            WorkspaceAction::SaveWorkspaceAs { name } => self.save_as(name),
            WorkspaceAction::SwitchWorkspace { name } => self.switch_to(name),
            WorkspaceAction::NewWorkspace { name } => self.new_workspace(name),
            WorkspaceAction::ClearNotices => {
                self.notices.clear();
                Ok(ActionOutcome::Applied)
            }
            WorkspaceAction::ResetWorkspace => self.reset_workspace(),
            WorkspaceAction::Quit => {
                self.autosave();
                self.settings = None;
                self.windows.clear();
                self.modules.clear();
                self.move_source = None;
                Ok(ActionOutcome::Applied)
            }
        }
    }

    /// The analysis controller of a placed instance.
    fn controller(
        &self,
        instance_id: ModuleInstanceId,
    ) -> Result<crate::dataflow::DataController, WorkspaceError> {
        self.located(instance_id)?;
        self.modules[&instance_id]
            .module
            .data()
            .cloned()
            .ok_or(WorkspaceError::Unsupported("analysis settings"))
    }

    /// View settings whose range follows the analysis (a visible span bounded by
    /// the History length) are brought back into range after an analysis edit, so
    /// the instance, its saved form and its copies agree.
    fn reconcile_views(&mut self) {
        for entry in self.modules.values_mut() {
            for setting in entry.module.view_settings() {
                if let Some(accepted) = setting.accept(&setting.value)
                    && accepted != setting.value
                {
                    let _ = entry.module.set_view_setting(setting.key, &accepted);
                }
            }
        }
    }

    /// Copy an instance next to itself: same type, `DataSpec` (with bindings),
    /// style, view settings and colors; new subscription and fresh private state.
    fn duplicate(
        &mut self,
        instance_id: ModuleInstanceId,
    ) -> Result<ActionOutcome, WorkspaceError> {
        let window_id = self.located(instance_id)?;
        if self.modules[&instance_id].preserved.is_some() {
            return Err(WorkspaceError::Unsupported(
                "copying a module that is not installed",
            ));
        }
        if !self.can_split(instance_id, Side::Right) {
            return Err(WorkspaceError::InsufficientSpace("copy this module"));
        }
        let config = persist::ModuleConfig::of(&self.modules[&instance_id]);
        let split_id = SplitId(self.next_split_id);
        let next_split = self
            .next_split_id
            .checked_add(1)
            .ok_or(WorkspaceError::IdExhausted)?;
        let new = self.create_instance(config.type_id, config.data.as_ref())?;
        // A copy that lost settings is not a copy: nothing is committed. A style this
        // build lacks stays the kept, unavailable style of the original.
        if let Err(reason) = self.apply_config(new, &config) {
            self.modules.remove(&new);
            return Err(WorkspaceError::CopyIncomplete(reason));
        }
        let window = self.window_mut(window_id)?;
        if let Err(error) = window
            .layout
            .insert_beside(instance_id, Side::Right, new, split_id)
        {
            self.modules.remove(&new);
            return Err(error.into());
        }
        window.active = Some(new);
        self.next_split_id = next_split;
        Ok(ActionOutcome::Created(new))
    }

    fn detach(&mut self, instance_id: ModuleInstanceId) -> Result<ActionOutcome, WorkspaceError> {
        let source_id = self.located(instance_id)?;
        let source = self.window(source_id)?;
        if source.layout.leaves().len() < 2 {
            return Err(WorkspaceError::Unsupported(
                "detach the only module of a window",
            ));
        }
        if self.windows.values().any(|window| {
            window
                .pending_detach
                .is_some_and(|p| p.instance_id == instance_id)
        }) {
            return Err(WorkspaceError::Unsupported("detach a module twice"));
        }
        let size = self
            .leaf_rect(source_id, instance_id)
            .map_or(INITIAL_WINDOW_SIZE, |rect| rect.size())
            .max(self.module_min_size(instance_id));
        let ppp = source.pixels_per_point;
        let position = source.desktop.outer.map(|outer| {
            let physical = outer.min + Vec2::splat(DETACH_OFFSET_POINTS * ppp);
            pos2(physical.x / ppp, physical.y / ppp)
        });
        // The app-wide clock: the source window's own pass time stops while it is
        // minimized (detach can be requested from the settings window then).
        let requested_at = self.display_time;
        let always_on_top = source.always_on_top;
        let min_size = self.module_min_size(instance_id);
        let window_id = WindowId(Self::allocate(&mut self.next_window_id)?);
        let mut window = WindowState::new(Layout::default(), size, always_on_top);
        // The window will hold this module: its minimum, not an empty window's.
        window.initial_min_size = min_size;
        window.initial_position = position;
        window.pixels_per_point = ppp;
        window.pending_detach = Some(PendingDetach {
            instance_id,
            source: source_id,
            requested_at,
        });
        self.windows.insert(window_id, window);
        Ok(ActionOutcome::WindowCreated(window_id))
    }

    /// Called on the first frame of a pending window: move the instance from its
    /// source layout, or drop the empty window when the source became invalid.
    fn commit_detach(&mut self, window_id: WindowId) {
        let Some(pending) = self
            .windows
            .get(&window_id)
            .and_then(|window| window.pending_detach)
        else {
            return;
        };
        let current = self.window_of(pending.instance_id);
        if current != Some(pending.source) || !self.modules.contains_key(&pending.instance_id) {
            self.windows.remove(&window_id);
            self.control_error = Some("detach canceled: the module is no longer there".into());
            return;
        }
        let source = self
            .windows
            .get_mut(&pending.source)
            .expect("source window located above");
        source
            .layout
            .remove_leaf(pending.instance_id)
            .expect("instance located in source above");
        Self::select_after_removal(source, pending.instance_id);
        let source_empty = source.layout.is_empty();
        let window = self
            .windows
            .get_mut(&window_id)
            .expect("pending window exists");
        window
            .layout
            .place_root(pending.instance_id)
            .expect("pending window starts empty");
        window.active = Some(pending.instance_id);
        window.pending_detach = None;
        if source_empty {
            let _ = self.close_window(pending.source);
        }
        self.sync_visibility();
    }

    fn move_module(
        &mut self,
        instance_id: ModuleInstanceId,
        target: MoveTarget,
    ) -> Result<ActionOutcome, WorkspaceError> {
        let source_id = self.located(instance_id)?;
        let target_window = match target {
            MoveTarget::Beside {
                instance_id: target_leaf,
                ..
            } => self.located(target_leaf)?,
            MoveTarget::EmptyWindow(window_id) => {
                self.window(window_id)?;
                window_id
            }
        };
        if !self.move_target_valid(instance_id, target) {
            return Err(WorkspaceError::InsufficientSpace("place the module there"));
        }
        let split_id = match target {
            MoveTarget::Beside { .. } => Some(SplitId(Self::allocate(&mut self.next_split_id)?)),
            MoveTarget::EmptyWindow(_) => None,
        };
        // All preconditions hold; the two tree updates below cannot fail.
        let source = self.windows.get_mut(&source_id).expect("located");
        source
            .layout
            .remove_leaf(instance_id)
            .expect("instance located in source");
        Self::select_after_removal(source, instance_id);
        let source_empty = source.layout.is_empty();
        let window = self
            .windows
            .get_mut(&target_window)
            .expect("target located");
        match (target, split_id) {
            (
                MoveTarget::Beside {
                    instance_id: target_leaf,
                    side,
                },
                Some(split_id),
            ) => window
                .layout
                .insert_beside(target_leaf, side, instance_id, split_id)
                .expect("validated target"),
            _ => window
                .layout
                .place_root(instance_id)
                .expect("validated empty window"),
        }
        window.active = Some(instance_id);
        if self.move_source == Some(instance_id) {
            self.move_source = None;
        }
        if source_empty && source_id != target_window {
            self.close_window(source_id)?;
        }
        Ok(ActionOutcome::Applied)
    }

    fn close_window(&mut self, window_id: WindowId) -> Result<(), WorkspaceError> {
        let window = self
            .windows
            .remove(&window_id)
            .ok_or(WorkspaceError::StaleWindow(window_id))?;
        for instance in window.layout.leaves() {
            if self.move_source == Some(instance) {
                self.move_source = None;
            }
            self.modules.remove(&instance);
        }
        if let Some(group) = window.group {
            let remaining: Vec<WindowId> = self
                .windows
                .iter()
                .filter(|(_, other)| other.group == Some(group))
                .map(|(id, _)| *id)
                .collect();
            if remaining.len() < 2 {
                for id in remaining {
                    if let Some(other) = self.windows.get_mut(&id) {
                        other.group = None;
                    }
                }
            }
        }
        Ok(())
    }

    fn group_touching(&mut self, window_id: WindowId) -> Result<ActionOutcome, WorkspaceError> {
        let outer = self
            .window(window_id)?
            .desktop
            .outer
            .ok_or(WorkspaceError::Unsupported(
                "group before the window position is known",
            ))?;
        let touching: Vec<WindowId> = self
            .windows
            .iter()
            .filter(|(id, window)| {
                **id != window_id
                    && window
                        .desktop
                        .outer
                        .is_some_and(|other| desktop::touching(outer, other))
            })
            .map(|(id, _)| *id)
            .collect();
        if touching.is_empty() {
            return Err(WorkspaceError::Unsupported(
                "group without a touching window",
            ));
        }
        let group = match self.window(window_id)?.group {
            Some(group) => group,
            None => Self::allocate(&mut self.next_group_id)?,
        };
        let mut merged: Vec<u64> = touching
            .iter()
            .filter_map(|id| self.windows.get(id).and_then(|window| window.group))
            .collect();
        merged.push(group);
        for (id, window) in &mut self.windows {
            if *id == window_id
                || touching.contains(id)
                || window.group.is_some_and(|g| merged.contains(&g))
            {
                window.group = Some(group);
            }
        }
        Ok(ActionOutcome::Applied)
    }

    /// Advance time-based state shared by all windows (detach timeouts). Every pass
    /// that runs (the host's logic, each drawn window, the settings window) ticks,
    /// so the clock also moves while the windows involved are minimized.
    pub(crate) fn tick(&mut self, display_time: Duration) {
        self.display_time = self.display_time.max(display_time);
        let now = self.display_time;
        let expired: Vec<WindowId> = self
            .windows
            .iter()
            .filter(|(_, window)| {
                window.pending_detach.is_some_and(|pending| {
                    !window.opened
                        && (now.saturating_sub(pending.requested_at) >= DETACH_TIMEOUT
                            || !self.modules.contains_key(&pending.instance_id)
                            || !self.windows.contains_key(&pending.source))
                })
            })
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            self.windows.remove(&id);
            self.control_error = Some("detach canceled: the new window did not open".into());
        }
    }

    /// Apply one window's observed native placement. Returns windows that received
    /// position commands and need a pass to deliver them.
    fn observe_placement(
        &mut self,
        window_id: WindowId,
        outer_points: Option<Rect>,
        pixels_per_point: f32,
        maximized: bool,
        minimized: bool,
        probe: &dyn DesktopProbe,
    ) -> Vec<WindowId> {
        let mut followers_to_wake = Vec::new();
        let Some(window) = self.windows.get_mut(&window_id) else {
            return followers_to_wake;
        };
        window.maximized = maximized;
        window.pixels_per_point = pixels_per_point;
        let Some(outer_points) = outer_points else {
            return followers_to_wake;
        };
        let outer = Rect::from_min_max(
            outer_points.min * pixels_per_point,
            outer_points.max * pixels_per_point,
        );
        let previous = window.desktop.outer.replace(outer);
        let Some(previous) = previous else {
            // First placement: land on the saved desktop position, or bring a window
            // that opened off every monitor back where it can be used. The OS owns
            // the placement of maximized and minimized windows; during a drag the
            // correction waits for the release (handled below on later passes).
            if maximized || minimized {
                window.restore_target = None;
            }
            let target = (!maximized && !minimized && !probe.primary_button_down())
                .then(|| Self::placement_target(window, outer, pixels_per_point, probe))
                .flatten();
            if let Some(target) = target {
                window.desktop.expected = Some(target);
                window
                    .commands
                    .push(ViewportCommand::OuterPosition(target / pixels_per_point));
                followers_to_wake.push(window_id);
            }
            return followers_to_wake;
        };
        let delta = outer.min - previous.min;
        let button_down = probe.primary_button_down();
        let mut user_delta = None;
        // Maximize, minimize and edge resizes change the frame without being a move:
        // they neither snap nor drag the group along.
        let reframed = maximized || minimized || previous.size() != outer.size();
        if delta.length() > 0.5 && !reframed {
            if window.desktop.native_drag || window.desktop.expected.is_none() {
                // The OS move loop this window started, or an external move.
                window.desktop.expected = None;
                window.desktop.moving = true;
                user_delta = Some(delta);
            } else if window.desktop.expected.is_some_and(|expected| {
                (expected - outer.min).length() <= desktop::TOUCH_TOLERANCE_PX
            }) {
                window.desktop.expected = None;
            }
            // Otherwise an earlier position command is still landing: never echoed
            // back to the group.
        } else if window.desktop.expected.is_some() && !window.desktop.native_drag && !button_down {
            // A command landed elsewhere (the OS adjusted it) and nothing moves now.
            window.desktop.expected = None;
        }
        if !button_down && !window.desktop.moving {
            window.desktop.native_drag = false;
        }
        let mut snap = Vec2::ZERO;
        if window.desktop.moving && !button_down && !maximized && !minimized {
            window.desktop.moving = false;
            window.desktop.native_drag = false;
            let group = window.group;
            let others: Vec<Rect> = self
                .windows
                .iter()
                .filter(|(id, other)| {
                    **id != window_id && (group.is_none() || other.group != group)
                })
                .filter_map(|(_, other)| other.desktop.outer)
                .collect();
            let threshold = desktop::SNAP_DISTANCE_POINTS * pixels_per_point;
            snap = desktop::snap_offset(outer, &others, probe.work_area(outer.center()), threshold);
            if snap != Vec2::ZERO {
                let window = self.windows.get_mut(&window_id).expect("checked above");
                let target = outer.min + snap;
                window.desktop.expected = Some(target);
                window.commands.push(ViewportCommand::OuterPosition(pos2(
                    target.x / pixels_per_point,
                    target.y / pixels_per_point,
                )));
                followers_to_wake.push(window_id);
            }
        }
        let shift = user_delta.unwrap_or(Vec2::ZERO) + snap;
        if shift != Vec2::ZERO
            && let Some(group) = self.windows[&window_id].group
        {
            for (id, follower) in &mut self.windows {
                if *id == window_id || follower.group != Some(group) || follower.maximized {
                    continue;
                }
                let Some(base) = follower
                    .desktop
                    .expected
                    .or(follower.desktop.outer.map(|r| r.min))
                else {
                    continue;
                };
                let target = base + shift;
                let ppp = follower.pixels_per_point;
                follower.desktop.expected = Some(target);
                follower.commands.push(ViewportCommand::OuterPosition(pos2(
                    target.x / ppp,
                    target.y / ppp,
                )));
                followers_to_wake.push(*id);
            }
        }
        // A restore correction that waited for a release, then a window left
        // without a usable area on any monitor (its display was disconnected, or it
        // was restored there): it comes back into the nearest one.
        let window = self.windows.get_mut(&window_id).expect("checked above");
        if user_delta.is_some() {
            // The user placed the window; the saved position no longer applies.
            window.restore_target = None;
        }
        if !button_down
            && !maximized
            && !minimized
            && !window.desktop.moving
            && window.desktop.expected.is_none()
            && let Some(target) = Self::placement_target(window, outer, pixels_per_point, probe)
        {
            window.desktop.expected = Some(target);
            window
                .commands
                .push(ViewportCommand::OuterPosition(target / pixels_per_point));
            followers_to_wake.push(window_id);
        }
        followers_to_wake
    }

    /// Where `outer` (physical pixels) must go to show enough of itself on the
    /// nearest monitor, if it does not already.
    fn rescue_target(outer: Rect, pixels_per_point: f32, probe: &dyn DesktopProbe) -> Option<Pos2> {
        let work = probe.work_area(outer.center())?;
        desktop::reachable_position(outer, work, desktop::MIN_VISIBLE_POINTS * pixels_per_point)
    }

    /// The saved position still to land on (once), otherwise a position that makes
    /// the window reachable again.
    fn placement_target(
        window: &mut WindowState,
        outer: Rect,
        pixels_per_point: f32,
        probe: &dyn DesktopProbe,
    ) -> Option<Pos2> {
        window
            .restore_target
            .take()
            .filter(|target| (*target - outer.min).length() > desktop::TOUCH_TOLERANCE_PX)
            .or_else(|| Self::rescue_target(outer, pixels_per_point, probe))
    }

    #[cfg(any(test, feature = "automation"))]
    pub(crate) fn observation(&self) -> WorkspaceObservation {
        let mut modules: Vec<ObservedModule> = self
            .modules
            .iter()
            .map(|(id, entry)| ObservedModule {
                instance_id: *id,
                type_id: entry.type_id,
                window_id: self.window_of(*id),
                channel: entry.module.channel(),
                active_style: entry.module.active_style(),
                data: entry.module.data().map(|controller| controller.spec()),
                view: entry
                    .module
                    .view_settings()
                    .into_iter()
                    .map(|setting| (setting.key, setting.value))
                    .collect(),
                colors: entry.colors.clone(),
                palette: entry.palette,
                missing: entry.preserved.as_ref().map(|file| file.type_id.clone()),
                object: std::ptr::from_ref::<dyn Module>(entry.module.as_ref()).cast::<u8>()
                    as usize,
            })
            .collect();
        modules.sort_by_key(|module| module.instance_id);
        WorkspaceObservation {
            windows: self
                .windows
                .iter()
                .map(|(id, window)| {
                    let geometry = window.layout.geometry(
                        Rect::from_min_size(Pos2::ZERO, window.content_size),
                        &|leaf| self.module_min_size(leaf),
                    );
                    ObservedWindow {
                        id: *id,
                        leaves: window.layout.leaves(),
                        leaf_rects: geometry.leaves,
                        splits: window
                            .layout
                            .split_ids()
                            .into_iter()
                            .filter_map(|split| {
                                window.layout.split(split).map(|s| ObservedSplit {
                                    id: split,
                                    axis: s.axis,
                                    ratio: s.ratio,
                                    manual: s.manual,
                                })
                            })
                            .collect(),
                        active: window.active,
                        always_on_top: window.always_on_top,
                        group: window.group,
                        opened: window.opened,
                        pending_detach: window.pending_detach.map(|p| p.instance_id),
                        min_size: self.window_min_size(window),
                        divider_drag: window.ui.divider_drag.map(|drag| drag.split_id),
                    }
                })
                .collect(),
            modules,
            move_source: self.move_source,
            source_status: self.services.source.source_status(),
            control_error: self.control_error.clone(),
            modules_created: self.modules_created,
            name: self.name.clone(),
            notices: self.notices.clone(),
            theme: self.theme.clone(),
        }
    }

    /// Ownership invariant: every live instance sits in exactly one layout leaf and
    /// every leaf refers to a live instance.
    #[cfg(any(test, feature = "automation"))]
    pub(crate) fn check_ownership(&self) -> Result<(), String> {
        let mut seen = HashMap::new();
        for (window_id, window) in &self.windows {
            for leaf in window.layout.leaves() {
                if let Some(previous) = seen.insert(leaf, *window_id) {
                    return Err(format!(
                        "instance {} appears in windows {} and {}",
                        leaf.0, previous.0, window_id.0
                    ));
                }
                if !self.modules.contains_key(&leaf) {
                    return Err(format!("leaf {} has no instance", leaf.0));
                }
            }
        }
        for id in self.modules.keys() {
            if !seen.contains_key(id) {
                return Err(format!("instance {} is not in any layout", id.0));
            }
        }
        Ok(())
    }

    #[cfg(any(test, feature = "automation"))]
    pub(crate) fn dataflow_handle_for_automation(&self) -> crate::dataflow::DataflowHandle {
        self.services.dataflow.clone()
    }

    #[cfg(any(test, feature = "automation"))]
    pub(crate) fn source_descriptor_for_automation(&self) -> &crate::audio::AudioDescriptor {
        self.services.source.descriptor().as_ref()
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        self.shutdown();
    }
}
