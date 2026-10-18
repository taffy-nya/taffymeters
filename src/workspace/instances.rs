//! Module instance lifecycle and per-instance analysis and view parameters.

use super::{
    ActionOutcome, ModuleEntry, ModuleInstanceId, Preserved, WindowId, Workspace, WorkspaceError,
    layout::LayoutError,
};
use crate::{
    dataflow::{ChannelSelection, Keep, NodeRef, NodeSpec, OutputId},
    extensions::RegistryError,
    module::Module,
    settings::SettingValue,
    theme::RoleOverrides,
};

impl Workspace {
    /// Create a default instance through the factory.
    pub(super) fn create_module(
        &mut self,
        type_id: &str,
    ) -> Result<ModuleInstanceId, WorkspaceError> {
        self.create_instance(type_id, None)
    }

    /// Create through the factory, with the saved analysis `data` when given; the
    /// ID is consumed only on success.
    pub(super) fn create_instance(
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

    pub(super) fn insert_entry(
        &mut self,
        id: ModuleInstanceId,
        type_id: &'static str,
        label: &'static str,
        module: Box<dyn Module>,
        preserved: Option<Preserved>,
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

    pub(super) fn add_module(
        &mut self,
        window_id: WindowId,
        type_id: &'static str,
    ) -> Result<ActionOutcome, WorkspaceError> {
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

    pub(super) fn replace_module(
        &mut self,
        instance_id: ModuleInstanceId,
        type_id: &'static str,
    ) -> Result<ActionOutcome, WorkspaceError> {
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

    pub(super) fn close_module(
        &mut self,
        instance_id: ModuleInstanceId,
    ) -> Result<ActionOutcome, WorkspaceError> {
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

    pub(super) fn set_channel(
        &mut self,
        instance_id: ModuleInstanceId,
        channel: ChannelSelection,
    ) -> Result<ActionOutcome, WorkspaceError> {
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

    pub(super) fn set_style(
        &mut self,
        instance_id: ModuleInstanceId,
        style_id: &'static str,
    ) -> Result<ActionOutcome, WorkspaceError> {
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

    pub(super) fn clear_visual_state(
        &mut self,
        instance_id: ModuleInstanceId,
    ) -> Result<ActionOutcome, WorkspaceError> {
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

    pub(super) fn edit_node(
        &mut self,
        instance_id: ModuleInstanceId,
        node: NodeRef,
        spec: NodeSpec,
    ) -> Result<ActionOutcome, WorkspaceError> {
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

    pub(super) fn set_keep(
        &mut self,
        instance_id: ModuleInstanceId,
        output: OutputId,
        seconds: f32,
    ) -> Result<ActionOutcome, WorkspaceError> {
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

    pub(super) fn set_view_setting(
        &mut self,
        instance_id: ModuleInstanceId,
        key: &'static str,
        value: SettingValue,
    ) -> Result<ActionOutcome, WorkspaceError> {
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
}
