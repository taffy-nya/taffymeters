//! Saving, restoring and switching workspaces. Only logical configuration crosses
//! the file boundary; restoring creates new instances through the factories, which
//! build their subscriptions from the saved `DataSpec`.

use std::collections::{BTreeMap, HashMap, HashSet};

use eframe::egui::{Pos2, pos2, vec2};
use serde_json::Value;

use super::{
    ActionOutcome, EMPTY_WINDOW_MIN_SIZE, ModuleEntry, ModuleInstanceId, SplitId, WindowId,
    WindowState, Workspace, WorkspaceError,
    layout::{Axis, Layout, Node, Split},
};
use crate::{
    LiveAudio,
    builtins::SPECTRUM_MODULE_ID,
    dataflow::{DataSpec, SharedAnalysisId},
    module::{Module, ModuleFrame, ModuleResponse},
    persistence::{
        FORMAT_VERSION, InputConfig, LayoutFile, ModuleFile, SYSTEM_INPUT, SplitAxis, WindowFile,
        WorkspaceFile, WorkspaceStore, same_name, valid_name,
    },
    render::{Point, TextStyle},
    settings::SettingValue,
    theme::{ColorRole, RoleOverrides, ThemeSettings},
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
    /// The workspace last used in `store` (or a new default one). An unreadable
    /// file is reported and protected: the session runs on a default layout and
    /// never saves over it.
    pub fn open(
        live: LiveAudio,
        store: WorkspaceStore,
        always_on_top: bool,
    ) -> Result<Self, WorkspaceError> {
        let mut workspace = Self::empty(live)?;
        let name = store
            .last_used()
            .unwrap_or_else(|| crate::persistence::DEFAULT_WORKSPACE.to_owned());
        let loaded = store.load(&name);
        workspace.store = Some(store);
        workspace.name = name;
        match loaded {
            Ok(Some(file)) => workspace.restore(file)?,
            Ok(None) => {
                workspace.add_default_window(always_on_top)?;
            }
            Err(error) => {
                workspace.protected = true;
                workspace.push_notice(format!("{error}; it will not be overwritten"));
                workspace.add_default_window(always_on_top)?;
            }
        }
        Ok(workspace)
    }

    /// Give a store to a workspace built in memory (automation scenarios).
    #[cfg(feature = "automation")]
    pub(crate) fn with_store(mut self, store: WorkspaceStore, name: &str) -> Self {
        self.store = Some(store);
        self.name = name.to_owned();
        self
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Saved workspace names (empty without a store).
    pub(crate) fn saved_workspaces(&self) -> Vec<String> {
        self.store
            .as_ref()
            .map(WorkspaceStore::list)
            .unwrap_or_default()
    }

    pub(crate) fn can_save(&self) -> bool {
        self.store.is_some() && !self.protected
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

    /// Save under the current name.
    pub(crate) fn save(&mut self) -> Result<(), WorkspaceError> {
        if self.protected {
            return Err(WorkspaceError::Protected(self.name.clone()));
        }
        let file = self.saved_file();
        let store = self.store.as_ref().ok_or(WorkspaceError::NoStore)?;
        store.save(&file)?;
        // The workspace file is committed; failing to record it as the one to open
        // next time does not undo that (Save As keeps the new name).
        if let Err(error) = store.set_last_used(&self.name) {
            self.push_notice(format!(
                "saved, but could not record it as the workspace to open next time: {error}"
            ));
        }
        Ok(())
    }

    /// Save on exit, before closing the last window and before a switch; problems
    /// become notices. Without windows there is nothing worth saving.
    pub(crate) fn autosave(&mut self) {
        if !self.can_save() || self.windows.is_empty() {
            return;
        }
        if let Err(error) = self.save() {
            self.push_notice(format!("could not save workspace: {error}"));
        }
    }

    pub(super) fn save_as(&mut self, name: String) -> Result<ActionOutcome, WorkspaceError> {
        let store = self.store.as_ref().ok_or(WorkspaceError::NoStore)?;
        if !valid_name(&name) {
            return Err(crate::persistence::PersistError::InvalidName(name).into());
        }
        // A protected file is never the destination: it could not be read.
        if (name != self.name || self.protected) && store.path_of(&name).exists() {
            return Err(WorkspaceError::WorkspaceExists(name));
        }
        let (previous, was_protected) = (std::mem::replace(&mut self.name, name), self.protected);
        self.protected = false;
        if let Err(error) = self.save() {
            self.name = previous;
            self.protected = was_protected;
            return Err(error);
        }
        Ok(ActionOutcome::Applied)
    }

    pub(super) fn switch_to(&mut self, name: String) -> Result<ActionOutcome, WorkspaceError> {
        let store = self.store.as_ref().ok_or(WorkspaceError::NoStore)?;
        let file = store
            .load(&name)?
            .ok_or_else(|| WorkspaceError::NoSuchWorkspace(name.clone()))?;
        if self.can_save() {
            self.save()?;
        }
        self.clear();
        self.name = name;
        self.protected = false;
        self.restore(file)?;
        if let Some(store) = &self.store
            && let Err(error) = store.set_last_used(&self.name)
        {
            self.push_notice(format!(
                "could not record the workspace to open next time: {error}"
            ));
        }
        Ok(ActionOutcome::Applied)
    }

    pub(super) fn new_workspace(&mut self, name: String) -> Result<ActionOutcome, WorkspaceError> {
        let store = self.store.as_ref().ok_or(WorkspaceError::NoStore)?;
        if !valid_name(&name) {
            return Err(crate::persistence::PersistError::InvalidName(name).into());
        }
        // The current name (even unsaved, in any letter case: Windows names files
        // case-insensitively) would make the outgoing save the new file's target.
        if same_name(&name, &self.name) || store.path_of(&name).exists() {
            return Err(WorkspaceError::WorkspaceExists(name));
        }
        if self.can_save() {
            self.save()?;
        }
        let placement = self.first_placement();
        // Only once the replacement exists is the current content dropped.
        self.replace_with_default(placement)?;
        self.name = name;
        self.protected = false;
        self.save()?;
        Ok(ActionOutcome::Applied)
    }

    /// Replace the workspace's content with the defaults, keeping its name and the
    /// place of its first window; saved unless the file is protected.
    pub(super) fn reset_workspace(&mut self) -> Result<ActionOutcome, WorkspaceError> {
        let placement = self.first_placement();
        // Only once the replacement exists is the current content dropped.
        self.replace_with_default(placement)?;
        if self.can_save() {
            self.save()?;
        }
        Ok(ActionOutcome::Applied)
    }

    /// Always-on-top state and desktop position (logical points, scale) of the
    /// first window, for the default window that replaces the content.
    fn first_placement(&self) -> (bool, Option<(Pos2, f32)>) {
        let Some(window) = self.windows.values().next() else {
            return (false, None);
        };
        let ppp = window.pixels_per_point;
        let position = window
            .desktop
            .outer
            .map(|outer| (outer.min / ppp, ppp))
            .or(window.initial_position.map(|position| (position, ppp)));
        (window.always_on_top, position)
    }

    /// Replace everything with one default Spectrum window and the Dark theme. The
    /// new instance is created first: when that fails (e.g. a busy control queue),
    /// the current content stays as it was.
    fn replace_with_default(
        &mut self,
        (always_on_top, position): (bool, Option<(Pos2, f32)>),
    ) -> Result<(), WorkspaceError> {
        let first = self.create_module(SPECTRUM_MODULE_ID)?;
        let window_id = match Self::allocate(&mut self.next_window_id) {
            Ok(id) => WindowId(id),
            Err(error) => {
                self.modules.remove(&first);
                return Err(error);
            }
        };
        let entry = self.modules.remove(&first);
        self.clear();
        if let Some(entry) = entry {
            self.modules.insert(first, entry);
        }
        self.theme = ThemeSettings::dark();
        self.input = InputConfig::default();
        self.refresh_palettes();
        let mut window = WindowState::new(
            Layout::with_leaf(first),
            super::INITIAL_WINDOW_SIZE,
            always_on_top,
        );
        window.active = Some(first);
        if let Some((position, ppp)) = position {
            window.initial_position = Some(position);
            window.pixels_per_point = ppp;
        }
        self.windows.insert(window_id, window);
        Ok(())
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

    /// Build windows and instances from a saved workspace.
    pub(super) fn restore(&mut self, file: WorkspaceFile) -> Result<(), WorkspaceError> {
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
        let mut bindings: HashMap<u64, SharedAnalysisId> = HashMap::new();
        let mut groups: HashMap<u32, u64> = HashMap::new();
        for window_file in &file.windows {
            self.restore_window(window_file, &mut bindings, &mut groups)?;
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
        if self.windows.is_empty() {
            self.add_default_window(false)?;
        }
        self.binding_numbers = bindings
            .into_iter()
            .map(|(number, binding)| (binding, number))
            .collect();
        self.sync_visibility();
        Ok(())
    }

    fn restore_window(
        &mut self,
        file: &WindowFile,
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
        let size = vec2(file.inner_size[0], file.inner_size[1]);
        let size = if size.is_finite() {
            size.max(min_size)
        } else {
            super::INITIAL_WINDOW_SIZE.max(min_size)
        };
        let mut window = WindowState::new(layout, size, file.always_on_top);
        window.initial_min_size = min_size;
        let ppp = if file.pixels_per_point.is_finite() {
            file.pixels_per_point.clamp(0.25, 8.0)
        } else {
            1.0
        };
        window.pixels_per_point = ppp;
        if let Some([x, y]) = file.outer_position
            && x.is_finite()
            && y.is_finite()
        {
            window.initial_position = Some(pos2(x / ppp, y / ppp));
            // A maximized window only needs the position to pick its monitor; the
            // OS places it, and it is not moved back afterwards.
            if !file.maximized {
                window.restore_target = Some(pos2(x, y));
            }
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
                WindowFile {
                    outer_position: window
                        .desktop
                        .outer
                        .map(|outer| outer.min)
                        .or_else(|| {
                            window
                                .initial_position
                                .map(|position| position * window.pixels_per_point)
                        })
                        .map(|position: Pos2| [position.x, position.y]),
                    pixels_per_point: window.pixels_per_point,
                    inner_size: [window.content_size.x, window.content_size.y],
                    always_on_top: window.always_on_top,
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
            name: self.name.clone(),
            input: self.input.clone(),
            theme: self.theme.clone(),
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
