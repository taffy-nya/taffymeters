//! The session, presets and how windows land on the desktop. Only logical
//! configuration crosses the file boundary; restoring creates new instances
//! through the factories, which build their subscriptions from the saved
//! `DataSpec`.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
};

use eframe::egui::{Rect, pos2, vec2};
use serde_json::Value;

use super::{
    ActionOutcome, EMPTY_WINDOW_MIN_SIZE, ModuleEntry, ModuleInstanceId, SplitId, WindowId,
    WindowState, Workspace, WorkspaceError,
    desktop::{
        DesktopProbe, Display, FALLBACK_DISPLAY, SavedDisplay, SavedWindow, display_at,
        match_displays, place, saved_frame,
    },
    layout::{Axis, Layout, Node, Split},
};
use crate::{
    LiveAudio,
    dataflow::{DataSpec, SharedAnalysisId},
    module::{Module, ModuleFrame, ModuleResponse},
    persistence::{
        DEFAULT_PRESET, DisplayFile, DisplayHint, FORMAT_VERSION, LayoutFile, ModuleFile,
        PersistError, PresetRef, SYSTEM_INPUT, SplitAxis, WindowFile, WorkspaceFile,
        WorkspaceStore, builtin_preset, builtin_presets, same_name, valid_name,
    },
    render::{Point, TextStyle},
    settings::SettingValue,
    theme::{ColorRole, RoleOverrides},
};

/// Type of the stand-in for an instance this build cannot restore.
pub(crate) const MISSING_MODULE_ID: &str = "taffymeters.missing";

/// The configuration of one instance, typed: what a copy takes over and what a
/// restore applies after the factory created the instance.
pub(super) struct ModuleConfig {
    pub(super) type_id: &'static str,
    pub(super) data: Option<DataSpec>,
    style: Option<String>,
    view: Vec<(String, SettingValue)>,
    colors: RoleOverrides,
}

impl ModuleConfig {
    pub(super) fn of(entry: &ModuleEntry) -> Self {
        Self {
            type_id: entry.type_id,
            data: entry.module.data().map(|controller| controller.spec()),
            style: entry
                .preserved_style
                .clone()
                .or_else(|| entry.module.active_style().map(str::to_owned)),
            view: entry
                .module
                .view_settings()
                .into_iter()
                .map(|setting| (setting.key.to_owned(), setting.value))
                .collect(),
            colors: entry.colors.clone(),
        }
    }
}

/// Stand-in leaf for a saved instance whose type is not installed or whose
/// configuration this build cannot use. It shows why and saves the original.
struct MissingModule {
    message: String,
}

impl Module for MissingModule {
    fn draw(&mut self, frame: ModuleFrame<'_>) -> ModuleResponse {
        frame.renderer.text(
            frame.rect,
            Point {
                x: frame.rect.min.x + 8.0,
                y: frame.rect.min.y + 8.0,
            },
            &self.message,
            TextStyle {
                size: frame.palette.text_size,
                color: frame.palette.color(ColorRole::Muted),
            },
        );
        ModuleResponse::default()
    }
}

fn axis_file(axis: Axis) -> SplitAxis {
    match axis {
        Axis::LeftRight => SplitAxis::LeftRight,
        Axis::TopBottom => SplitAxis::TopBottom,
    }
}

fn file_axis(axis: SplitAxis) -> Axis {
    match axis {
        SplitAxis::LeftRight => Axis::LeftRight,
        SplitAxis::TopBottom => Axis::TopBottom,
    }
}

impl Workspace {
    /// The last session in `store`, or the built-in Default preset when there is
    /// none. An unreadable session file is reported and protected: the process runs
    /// on the Default preset and never saves over it.
    pub fn open(
        live: LiveAudio,
        store: WorkspaceStore,
        desktop: Arc<dyn DesktopProbe + Send + Sync>,
    ) -> Result<Self, WorkspaceError> {
        let mut workspace = Self::empty(live, desktop)?;
        let loaded = store.load_session();
        workspace.store = Some(store);
        let file = match loaded {
            Ok(Some(file)) => file,
            Ok(None) => workspace.default_preset()?,
            Err(error) => {
                workspace.protected = true;
                workspace.push_notice(format!("{error}; it will not be overwritten"));
                workspace.default_preset()?
            }
        };
        workspace.preset = file.preset.clone();
        let displays = workspace.displays();
        let current = Self::primary(&displays);
        workspace.restore(file, &displays, current)?;
        Ok(workspace)
    }

    /// The built-in Default preset, recorded as the session's origin.
    fn default_preset(&self) -> Result<WorkspaceFile, WorkspaceError> {
        let preset = PresetRef::Builtin(DEFAULT_PRESET.to_owned());
        let mut file = self.preset_file(&preset)?;
        file.preset = Some(preset);
        Ok(file)
    }

    /// Give a store to a workspace built in memory (automation scenarios).
    #[cfg(feature = "automation")]
    pub(crate) fn with_store(mut self, store: WorkspaceStore) -> Self {
        self.store = Some(store);
        self
    }

    pub(crate) fn preset(&self) -> Option<&PresetRef> {
        self.preset.as_ref()
    }

    /// Saved preset names (empty without a store).
    pub(crate) fn saved_presets(&self) -> Vec<String> {
        self.store
            .as_ref()
            .map(WorkspaceStore::list_presets)
            .unwrap_or_default()
    }

    pub(crate) fn can_save(&self) -> bool {
        self.store.is_some() && !self.protected
    }

    /// The monitors windows are placed on (a stand-in when none is reported).
    fn displays(&self) -> Vec<Display> {
        let displays = self.desktop.displays();
        if displays.is_empty() {
            vec![FALLBACK_DISPLAY]
        } else {
            displays
        }
    }

    fn primary(displays: &[Display]) -> usize {
        displays
            .iter()
            .position(|display| display.primary)
            .unwrap_or(0)
    }

    /// Physical outer frame of a window: where it is, else where it will open,
    /// else centered on `fallback`.
    fn outer_frame(window: &WindowState, fallback: &Display) -> Rect {
        window.desktop.outer.unwrap_or_else(|| {
            let ppp = window.pixels_per_point;
            match window.initial_position {
                Some(position) => Rect::from_min_size(position * ppp, window.content_size * ppp),
                None => Rect::from_center_size(
                    fallback.work.center(),
                    window.content_size * fallback.scale,
                ),
            }
        })
    }

    /// Apply style, view settings and colors to a new instance. An unavailable
    /// style is tolerated (default style, the saved ID is kept) and returned as a
    /// warning; a view setting the instance cannot take makes the configuration
    /// unusable (`Err`), so the caller keeps the original instead.
    pub(super) fn apply_config(
        &mut self,
        id: ModuleInstanceId,
        config: &ModuleConfig,
    ) -> Result<Vec<String>, String> {
        let mut warnings = Vec::new();
        let entry = self.modules.get_mut(&id).expect("instance just created");
        if let Some(style) = &config.style
            && entry.module.active_style() != Some(style.as_str())
        {
            match entry.module.set_style(style, &self.registry) {
                Ok(()) => {}
                Err(error) => {
                    warnings.push(format!("{} #{}: style {style}: {error}", entry.label, id.0));
                    entry.preserved_style = Some(style.clone());
                }
            }
        }
        for (key, value) in &config.view {
            let accepted = entry
                .module
                .view_settings()
                .into_iter()
                .find(|setting| setting.key == key)
                .and_then(|setting| setting.accept(value));
            match accepted {
                Some(value) => entry
                    .module
                    .set_view_setting(key, &value)
                    .map_err(|error| error.to_string())?,
                None => return Err(format!("view setting {key} cannot be used")),
            }
        }
        entry.colors = config.colors.clone();
        entry.palette = self.theme.resolve(entry.type_id, &entry.colors);
        Ok(warnings)
    }

    /// Save the session; problems become notices. Runs on exit, on Quit and before
    /// the last window closes. Without windows there is nothing worth saving.
    pub(crate) fn autosave(&mut self) {
        if !self.can_save() || self.windows.is_empty() {
            return;
        }
        let file = self.saved_file();
        let store = self.store.as_ref().expect("can_save checked the store");
        if let Err(error) = store.save_session(&file) {
            self.push_notice(format!("could not save the workspace: {error}"));
        }
    }

    fn preset_file(&self, preset: &PresetRef) -> Result<WorkspaceFile, WorkspaceError> {
        let missing = || WorkspaceError::NoSuchPreset(preset.name().to_owned());
        match preset {
            PresetRef::Builtin(name) => Ok(builtin_preset(name).ok_or_else(missing)??),
            PresetRef::Saved(name) => self
                .store
                .as_ref()
                .ok_or(WorkspaceError::NoStore)?
                .load_preset(name)?
                .ok_or_else(missing),
        }
    }

    /// Replace everything with `preset`, placed on the display of the window the
    /// settings were opened from (or of the first window).
    pub(super) fn load_preset(
        &mut self,
        preset: PresetRef,
    ) -> Result<ActionOutcome, WorkspaceError> {
        let file = self.preset_file(&preset)?;
        let displays = self.displays();
        let primary = Self::primary(&displays);
        let anchor = self
            .settings
            .as_ref()
            .and_then(|settings| settings.window())
            .and_then(|id| self.windows.get(&id))
            .or_else(|| self.windows.values().next());
        let current = anchor
            .and_then(|window| {
                display_at(
                    &displays,
                    Self::outer_frame(window, &displays[primary]).center(),
                )
            })
            .unwrap_or(primary);
        self.clear();
        self.preset = Some(preset);
        self.restore(file, &displays, current)?;
        Ok(ActionOutcome::Applied)
    }

    /// The workspace as a preset file (no session origin).
    fn preset_contents(&mut self) -> WorkspaceFile {
        let mut file = self.saved_file();
        file.preset = None;
        file
    }

    pub(super) fn save_preset_as(&mut self, name: String) -> Result<ActionOutcome, WorkspaceError> {
        let store = self.store.clone().ok_or(WorkspaceError::NoStore)?;
        if !valid_name(&name) {
            return Err(PersistError::InvalidName(name).into());
        }
        // Windows names files case-insensitively.
        if builtin_presets().any(|builtin| same_name(builtin, &name))
            || store
                .list_presets()
                .iter()
                .any(|saved| same_name(saved, &name))
        {
            return Err(WorkspaceError::PresetExists(name));
        }
        let file = self.preset_contents();
        store.save_preset(&name, &file)?;
        self.preset = Some(PresetRef::Saved(name));
        Ok(ActionOutcome::Applied)
    }

    pub(super) fn save_preset(&mut self) -> Result<ActionOutcome, WorkspaceError> {
        let store = self.store.clone().ok_or(WorkspaceError::NoStore)?;
        let Some(PresetRef::Saved(name)) = self.preset.clone() else {
            return Err(WorkspaceError::NoSavedPreset);
        };
        let file = self.preset_contents();
        store.save_preset(&name, &file)?;
        Ok(ActionOutcome::Applied)
    }

    pub(super) fn delete_preset(&mut self, name: &str) -> Result<ActionOutcome, WorkspaceError> {
        let store = self.store.as_ref().ok_or(WorkspaceError::NoStore)?;
        store.delete_preset(name)?;
        if matches!(&self.preset, Some(PresetRef::Saved(origin)) if same_name(origin, name)) {
            self.preset = None;
        }
        Ok(ActionOutcome::Applied)
    }

    /// Drop every window and instance (and with them their subscriptions).
    fn clear(&mut self) {
        self.windows.clear();
        self.modules.clear();
        self.move_source = None;
        self.binding_numbers.clear();
        self.notices.clear();
        if let Some(settings) = &mut self.settings {
            settings.forget_targets();
        }
    }

    /// Build windows and instances from a file, placing its displays on
    /// `displays` (the first saved one on `current` unless it is recognized).
    fn restore(
        &mut self,
        file: WorkspaceFile,
        displays: &[Display],
        current: usize,
    ) -> Result<(), WorkspaceError> {
        self.theme = file.theme;
        if crate::theme::Preset::from_id(&self.theme.preset).is_none() {
            self.push_notice(format!(
                "theme preset {:?} is not available; showing Dark",
                self.theme.preset
            ));
        }
        self.refresh_palettes();
        if file.input.kind != SYSTEM_INPUT {
            self.push_notice(format!(
                "input {:?} is not available in this build; using system audio and keeping the saved input",
                file.input.kind
            ));
        }
        self.input = file.input;
        self.reference_dbfs = file.reference_dbfs;
        let saved: Vec<SavedDisplay> = file
            .displays
            .iter()
            .map(|display| SavedDisplay {
                size: vec2(display.work_area[0], display.work_area[1]),
                hint: display
                    .hint
                    .as_ref()
                    .map(|hint| (pos2(hint.origin_px[0], hint.origin_px[1]), hint.scale)),
            })
            .collect();
        let targets: Vec<Display> = match_displays(&saved, displays, current)
            .into_iter()
            .map(|index| displays[index])
            .collect();
        let windows: Vec<SavedWindow> = file
            .windows
            .iter()
            .map(|window| {
                let [left, top, right, bottom] = window.frame;
                SavedWindow {
                    display: window.display,
                    frame: Rect::from_min_max(pos2(left, top), pos2(right, bottom)),
                    group: window.group,
                }
            })
            .collect();
        let outers = place(&saved, &windows, &targets);
        let mut bindings: HashMap<u64, SharedAnalysisId> = HashMap::new();
        let mut groups: HashMap<u32, u64> = HashMap::new();
        for (window_file, outer) in file.windows.iter().zip(outers) {
            let scale = targets[window_file.display].scale;
            self.restore_window(window_file, outer, scale, &mut bindings, &mut groups)?;
        }
        // A group needs two windows.
        let mut members: HashMap<u64, usize> = HashMap::new();
        for group in self.windows.values().filter_map(|window| window.group) {
            *members.entry(group).or_default() += 1;
        }
        for window in self.windows.values_mut() {
            if window.group.is_some_and(|group| members[&group] < 2) {
                window.group = None;
            }
        }
        self.binding_numbers = bindings
            .into_iter()
            .map(|(number, binding)| (binding, number))
            .collect();
        self.sync_visibility();
        Ok(())
    }

    /// One window at `outer` (physical pixels) on a display of `scale`.
    fn restore_window(
        &mut self,
        file: &WindowFile,
        outer: Rect,
        scale: f32,
        bindings: &mut HashMap<u64, SharedAnalysisId>,
        groups: &mut HashMap<u32, u64>,
    ) -> Result<WindowId, WorkspaceError> {
        let root = match &file.layout {
            Some(layout) => Some(self.restore_node(layout, bindings)?),
            None => None,
        };
        let layout = Layout::from_root(root);
        // A populated window may be smaller than an empty one: its minimum is its
        // layout's.
        let min_size = layout
            .min_size(&|id| self.module_min_size(id))
            .unwrap_or(EMPTY_WINDOW_MIN_SIZE);
        let size = (outer.size() / scale).max(min_size);
        let mut window = WindowState::new(layout, size, file.always_on_top);
        window.initial_min_size = min_size;
        window.pinned = file.pinned;
        window.pixels_per_point = scale;
        window.initial_position = Some(outer.min / scale);
        // A maximized window only needs the position to pick its monitor; the OS
        // places it, and it is not moved back afterwards.
        if !file.maximized {
            window.restore_target = Some(outer.min);
        }
        window.initial_maximized = file.maximized;
        window.active = file
            .active
            .and_then(|index| window.layout.leaves().get(index as usize).copied());
        if let Some(number) = file.group {
            let group = match groups.get(&number) {
                Some(group) => *group,
                None => {
                    let group = Self::allocate(&mut self.next_group_id)?;
                    groups.insert(number, group);
                    group
                }
            };
            window.group = Some(group);
        }
        let id = WindowId(Self::allocate(&mut self.next_window_id)?);
        self.windows.insert(id, window);
        Ok(id)
    }

    fn restore_node(
        &mut self,
        file: &LayoutFile,
        bindings: &mut HashMap<u64, SharedAnalysisId>,
    ) -> Result<Node, WorkspaceError> {
        Ok(match file {
            LayoutFile::Leaf { module } => Node::Leaf(self.restore_module(module, bindings)?),
            LayoutFile::Split {
                axis,
                ratio,
                manual,
                first,
                second,
            } => {
                let first = self.restore_node(first, bindings)?;
                let second = self.restore_node(second, bindings)?;
                Node::Split(Box::new(Split {
                    id: SplitId(Self::allocate(&mut self.next_split_id)?),
                    axis: file_axis(*axis),
                    ratio: if ratio.is_finite() {
                        ratio.clamp(0.0, 1.0)
                    } else {
                        0.5
                    },
                    manual: *manual,
                    first,
                    second,
                }))
            }
        })
    }

    /// One saved instance; a stand-in keeps the configuration when it cannot be
    /// used. Only ID exhaustion fails the restore.
    fn restore_module(
        &mut self,
        file: &ModuleFile,
        bindings: &mut HashMap<u64, SharedAnalysisId>,
    ) -> Result<ModuleInstanceId, WorkspaceError> {
        match self.module_config(file, bindings) {
            Ok(config) => match self.create_instance(config.type_id, config.data.as_ref()) {
                Ok(id) => match self.apply_config(id, &config) {
                    Ok(warnings) => {
                        for warning in warnings {
                            self.push_notice(warning);
                        }
                        Ok(id)
                    }
                    Err(reason) => {
                        // Running with defaults would overwrite the saved values.
                        self.modules.remove(&id);
                        self.missing_module(file, reason)
                    }
                },
                Err(WorkspaceError::IdExhausted) => Err(WorkspaceError::IdExhausted),
                Err(error) => self.missing_module(file, error.to_string()),
            },
            Err(reason) => self.missing_module(file, reason),
        }
    }

    fn module_config(
        &self,
        file: &ModuleFile,
        bindings: &mut HashMap<u64, SharedAnalysisId>,
    ) -> Result<ModuleConfig, String> {
        let (type_id, _) = self
            .registry
            .module_type(&file.type_id)
            .ok_or_else(|| "module type is not installed".to_owned())?;
        let data = match &file.data {
            None => None,
            Some(value) => {
                let mut spec: DataSpec = serde_json::from_value(value.clone())
                    .map_err(|error| format!("analysis: {error}"))?;
                spec.rebind(|number| {
                    *bindings
                        .entry(number.0)
                        .or_insert_with(|| self.services.dataflow.allocate_binding())
                });
                Some(spec)
            }
        };
        let view = file
            .view
            .iter()
            .map(|(key, value)| {
                serde_json::from_value::<SettingValue>(value.clone())
                    .map(|value| (key.clone(), value))
                    .map_err(|error| format!("view setting {key}: {error}"))
            })
            .collect::<Result<_, _>>()?;
        let colors = file
            .colors
            .iter()
            .map(|(role, color)| {
                let role = serde_json::from_value(Value::String(role.clone()))
                    .map_err(|error| format!("color role {role}: {error}"))?;
                let color = serde_json::from_value(color.clone())
                    .map_err(|error| format!("color: {error}"))?;
                Ok((role, color))
            })
            .collect::<Result<_, String>>()?;
        Ok(ModuleConfig {
            type_id,
            data,
            style: file.style.clone(),
            view,
            colors,
        })
    }

    fn missing_module(
        &mut self,
        file: &ModuleFile,
        reason: String,
    ) -> Result<ModuleInstanceId, WorkspaceError> {
        let next = self
            .next_instance_id
            .checked_add(1)
            .ok_or(WorkspaceError::IdExhausted)?;
        let id = ModuleInstanceId(self.next_instance_id);
        self.next_instance_id = next;
        self.push_notice(format!(
            "module {} #{} could not be restored ({reason}); its configuration is kept",
            file.type_id, id.0
        ));
        let module = Box::new(MissingModule {
            message: format!("{}: {reason}. Configuration kept.", file.type_id),
        });
        self.insert_entry(
            id,
            MISSING_MODULE_ID,
            "Missing module",
            module,
            Some(file.clone()),
        );
        Ok(id)
    }

    /// The saved form of the whole workspace.
    pub(super) fn saved_file(&mut self) -> WorkspaceFile {
        // Bindings keep the numbers they were restored with; new ones take numbers
        // no live binding or preserved configuration uses. Numbers of bindings no
        // instance uses any more are free again.
        let live: HashSet<SharedAnalysisId> = self
            .modules
            .values()
            .filter_map(|entry| entry.module.data())
            .flat_map(|controller| {
                controller.with_spec(|spec| {
                    spec.outputs()
                        .iter()
                        .filter_map(|output| output.binding)
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        self.binding_numbers
            .retain(|binding, _| live.contains(binding));
        let mut used: HashSet<u64> = self.binding_numbers.values().copied().collect();
        for entry in self.modules.values() {
            if let Some(file) = &entry.preserved {
                used.extend(file.bindings());
            }
        }
        let mut numbers = std::mem::take(&mut self.binding_numbers);
        let mut groups: BTreeMap<u64, u32> = BTreeMap::new();
        let displays = self.displays();
        let fallback = displays[Self::primary(&displays)];
        // Displays in the order windows first use them.
        let mut on: Vec<usize> = Vec::new();
        let windows = self
            .windows
            .values()
            .filter(|window| window.opened || window.pending_detach.is_none())
            .map(|window| {
                let group = window.group.map(|group| {
                    let next = groups.len() as u32 + 1;
                    *groups.entry(group).or_insert(next)
                });
                let leaves = window.layout.leaves();
                let outer = Self::outer_frame(window, &fallback);
                let index = display_at(&displays, outer.center()).unwrap_or(0);
                let display = on
                    .iter()
                    .position(|used| *used == index)
                    .unwrap_or_else(|| {
                        on.push(index);
                        on.len() - 1
                    });
                let frame = saved_frame(&displays[index], outer);
                WindowFile {
                    display,
                    frame: [frame.min.x, frame.min.y, frame.max.x, frame.max.y],
                    always_on_top: window.always_on_top,
                    pinned: window.pinned,
                    maximized: window.maximized,
                    group,
                    active: window
                        .active
                        .and_then(|active| leaves.iter().position(|leaf| *leaf == active))
                        .map(|index| index as u32),
                    layout: window
                        .layout
                        .root()
                        .map(|root| self.layout_file(root, &mut numbers, &mut used)),
                }
            })
            .collect();
        self.binding_numbers = numbers;
        WorkspaceFile {
            format_version: FORMAT_VERSION,
            preset: self.preset.clone(),
            input: self.input.clone(),
            reference_dbfs: self.reference_dbfs,
            theme: self.theme.clone(),
            displays: on
                .into_iter()
                .map(|index| {
                    let display = displays[index];
                    let size = display.logical_size();
                    DisplayFile {
                        work_area: [size.x, size.y],
                        hint: Some(DisplayHint {
                            origin_px: [display.work.min.x, display.work.min.y],
                            scale: display.scale,
                        }),
                    }
                })
                .collect(),
            windows,
        }
    }

    fn layout_file(
        &self,
        node: &Node,
        numbers: &mut HashMap<SharedAnalysisId, u64>,
        used: &mut HashSet<u64>,
    ) -> LayoutFile {
        match node {
            Node::Leaf(id) => LayoutFile::Leaf {
                module: self.module_file(&self.modules[id], numbers, used),
            },
            Node::Split(split) => LayoutFile::Split {
                axis: axis_file(split.axis),
                ratio: split.ratio,
                manual: split.manual,
                first: Box::new(self.layout_file(&split.first, numbers, used)),
                second: Box::new(self.layout_file(&split.second, numbers, used)),
            },
        }
    }

    fn module_file(
        &self,
        entry: &ModuleEntry,
        numbers: &mut HashMap<SharedAnalysisId, u64>,
        used: &mut HashSet<u64>,
    ) -> ModuleFile {
        if let Some(preserved) = &entry.preserved {
            return preserved.clone();
        }
        let data = entry.module.data().map(|controller| {
            let mut spec = controller.spec();
            spec.rebind(|binding| {
                let number = *numbers.entry(binding).or_insert_with(|| {
                    let number = (1..)
                        .find(|candidate| !used.contains(candidate))
                        .expect("free number");
                    used.insert(number);
                    number
                });
                SharedAnalysisId(number)
            });
            serde_json::to_value(spec).expect("a DataSpec serializes")
        });
        let style = entry
            .preserved_style
            .clone()
            .or_else(|| entry.module.active_style().map(str::to_owned));
        let view = entry
            .module
            .view_settings()
            .into_iter()
            .map(|setting| {
                (
                    setting.key.to_owned(),
                    serde_json::to_value(setting.value).expect("a setting serializes"),
                )
            })
            .collect();
        let colors = entry
            .colors
            .iter()
            .map(|(role, color)| {
                let Value::String(role) = serde_json::to_value(role).expect("a role serializes")
                else {
                    unreachable!("roles serialize as strings")
                };
                (
                    role,
                    serde_json::to_value(color).expect("a color serializes"),
                )
            })
            .collect();
        ModuleFile {
            type_id: entry.type_id.to_owned(),
            data,
            style,
            view,
            colors,
        }
    }
}
