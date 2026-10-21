use std::collections::HashMap;

use eframe::egui::{Pos2, Rect, Vec2};

use super::{Axis, ModuleInstanceId, SplitId, WindowId, Workspace};
use crate::{
    audio::SourceStatus,
    dataflow::{ChannelSelection, DataController},
    module::Module,
    settings::SettingValue,
    theme::{Palette, RoleOverrides, ThemeSettings},
};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ObservedModule {
    pub(crate) instance_id: ModuleInstanceId,
    pub(crate) type_id: &'static str,
    pub(crate) window_id: Option<WindowId>,
    pub(crate) channel: Option<ChannelSelection>,
    pub(crate) active_style: Option<&'static str>,
    pub(crate) data: Option<crate::dataflow::DataSpec>,
    pub(crate) view: Vec<(&'static str, SettingValue)>,
    pub(crate) colors: RoleOverrides,
    pub(crate) palette: Palette,
    /// Saved type of an instance that could not be restored.
    pub(crate) missing: Option<String>,
    /// Module object address, used to detect replacement rather than reconfiguration.
    pub(crate) object: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ObservedSplit {
    pub(crate) id: SplitId,
    pub(crate) axis: Axis,
    pub(crate) ratio: f32,
    pub(crate) manual: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ObservedWindow {
    pub(crate) id: WindowId,
    pub(crate) leaves: Vec<ModuleInstanceId>,
    /// Window-local logical points.
    pub(crate) leaf_rects: Vec<(ModuleInstanceId, Rect)>,
    pub(crate) splits: Vec<ObservedSplit>,
    pub(crate) always_on_top: bool,
    pub(crate) locked: bool,
    pub(crate) group: Option<u64>,
    pub(crate) opened: bool,
    pub(crate) pending_detach: Option<ModuleInstanceId>,
    /// Logical points.
    pub(crate) min_size: Vec2,
    pub(crate) divider_drag: Option<SplitId>,
    /// Outer frame, physical pixels.
    pub(crate) outer: Option<Rect>,
    /// Lines the last pass drew along the frame.
    pub(crate) edges: Vec<super::window::DrawnEdge>,
    /// Last pass's outlined module and window-local logical rect.
    pub(crate) module_outline: Option<(ModuleInstanceId, Rect)>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WorkspaceObservation {
    pub(crate) windows: Vec<ObservedWindow>,
    pub(crate) modules: Vec<ObservedModule>,
    pub(crate) move_source: Option<ModuleInstanceId>,
    pub(crate) settings_target: Option<(Option<ModuleInstanceId>, Option<WindowId>)>,
    pub(crate) source_status: SourceStatus,
    pub(crate) control_error: Option<String>,
    pub(crate) modules_created: u64,
    pub(crate) preset: Option<crate::persistence::PresetRef>,
    pub(crate) notices: Vec<String>,
    pub(crate) theme: ThemeSettings,
}

impl WorkspaceObservation {
    pub(crate) fn window(&self, id: WindowId) -> Option<&ObservedWindow> {
        self.windows.iter().find(|window| window.id == id)
    }

    pub(crate) fn module(&self, id: ModuleInstanceId) -> Option<&ObservedModule> {
        self.modules.iter().find(|module| module.instance_id == id)
    }
}

impl Workspace {
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
                data: entry.module.data().map(DataController::spec),
                view: entry
                    .module
                    .view_settings()
                    .into_iter()
                    .map(|setting| (setting.key, setting.value))
                    .collect(),
                colors: entry.colors.clone(),
                palette: entry.palette,
                missing: entry
                    .preserved
                    .as_ref()
                    .map(|preserved| preserved.file.type_id.clone()),
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
                        always_on_top: window.always_on_top,
                        locked: window.locked,
                        group: window.group,
                        opened: window.opened,
                        pending_detach: window.pending_detach.map(|p| p.instance_id),
                        min_size: self.window_min_size(window),
                        divider_drag: window.ui.divider_drag.map(|drag| drag.split_id),
                        outer: window.outer,
                        edges: window.ui.edges.clone(),
                        module_outline: window.ui.module_outline,
                    }
                })
                .collect(),
            modules,
            move_source: self.move_source,
            settings_target: self.settings_open().then(|| self.settings_targets_now()),
            source_status: self.services.source.source_status(),
            control_error: self
                .control_error
                .as_ref()
                .map(|(message, _)| message.clone()),
            modules_created: self.modules_created,
            preset: self.preset.clone(),
            notices: self.notices.clone(),
            theme: self.theme.clone(),
        }
    }

    /// Ownership invariant: every live instance sits in exactly one layout leaf and
    /// every leaf refers to a live instance.
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

    pub(crate) fn dataflow_handle_for_automation(&self) -> crate::dataflow::DataflowHandle {
        self.services.dataflow.clone()
    }

    pub(crate) fn source_descriptor_for_automation(&self) -> &crate::audio::AudioDescriptor {
        self.services.source.descriptor().as_ref()
    }
}
