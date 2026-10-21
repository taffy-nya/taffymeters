use eframe::egui::{Rect, Vec2};

use super::{
    ActionOutcome, INITIAL_WINDOW_SIZE, ModuleInstanceId, MoveTarget, PendingDetach, Side, SplitId,
    WindowId, WindowState, Workspace, WorkspaceError,
    layout::{Layout, fits},
    persist,
};

impl Workspace {
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
        self.window_of(instance)
            .and_then(|window_id| self.leaf_rect(window_id, instance))
            .is_some_and(|rect| self.can_split_in(instance, side, rect.size()))
    }

    pub(crate) fn can_split_in(&self, instance: ModuleInstanceId, side: Side, area: Vec2) -> bool {
        let min = self.module_min_size(instance);
        self.modules
            .get(&instance)
            .is_some_and(|entry| entry.preserved.is_none())
            && fits(area, side.axis(), min, min)
    }

    pub(crate) fn move_target_valid(&self, source: ModuleInstanceId, target: MoveTarget) -> bool {
        if !self.modules.contains_key(&source) || self.window_of(source).is_none() {
            return false;
        }
        match target {
            MoveTarget::Beside { instance_id, side } => {
                instance_id != source
                    && self
                        .window_of(instance_id)
                        .is_some_and(|window_id| !self.windows[&window_id].locked)
                    && self.can_place_beside(instance_id, side, self.module_min_size(source))
            }
            MoveTarget::EmptyWindow(window_id) => {
                self.windows.get(&window_id).is_some_and(|window| {
                    !window.locked && window.layout.is_empty() && window.pending_detach.is_none()
                })
            }
        }
    }

    /// Preview the post-move layout: remove the source if already here, then re-equalize.
    pub(crate) fn move_landing(
        &self,
        source: ModuleInstanceId,
        target: ModuleInstanceId,
        side: Side,
        window_id: WindowId,
        rect: Rect,
    ) -> Option<Rect> {
        let mut layout = self.windows.get(&window_id)?.layout.clone();
        if layout.contains(source) {
            layout.remove_leaf(source).ok()?;
        }
        layout
            .insert_beside(target, side, source, SplitId(self.next_split_id))
            .ok()?;
        layout
            .geometry(rect, &|id| self.module_min_size(id))
            .leaves
            .into_iter()
            .find_map(|(leaf, leaf_rect)| (leaf == source).then_some(leaf_rect))
    }

    /// Copies get a fresh subscription and private state, but retain explicit data bindings.
    pub(super) fn split_module(
        &mut self,
        instance_id: ModuleInstanceId,
        side: Side,
    ) -> Result<ActionOutcome, WorkspaceError> {
        let window_id = self.located(instance_id)?;
        if self.modules[&instance_id].preserved.is_some() {
            return Err(WorkspaceError::Unsupported(
                "copying a module that is not installed",
            ));
        }
        if !self.can_split(instance_id, side) {
            return Err(WorkspaceError::InsufficientSpace("split this module"));
        }
        let config = persist::ModuleConfig::of(&self.modules[&instance_id]);
        let split_id = SplitId(self.next_split_id);
        let next_split = self
            .next_split_id
            .checked_add(1)
            .ok_or(WorkspaceError::IdExhausted)?;
        let new = self.create_instance(config.type_id, config.data.as_ref())?;
        // An unavailable style can be preserved; lost settings must abort the copy.
        if let Err(reason) = self.apply_config(new, &config) {
            self.modules.remove(&new);
            return Err(WorkspaceError::CopyIncomplete(reason));
        }
        if let Err(error) =
            self.window_mut(window_id)?
                .layout
                .insert_beside(instance_id, side, new, split_id)
        {
            self.modules.remove(&new);
            return Err(error.into());
        }
        self.next_split_id = next_split;
        Ok(ActionOutcome::Created(new))
    }

    pub(super) fn set_split_ratio(
        &mut self,
        window_id: WindowId,
        split_id: SplitId,
        ratio: f32,
        manual: bool,
    ) -> Result<ActionOutcome, WorkspaceError> {
        self.window_mut(window_id)?
            .layout
            .set_ratio(split_id, ratio, manual)?;
        Ok(ActionOutcome::Applied)
    }

    pub(super) fn reset_split(
        &mut self,
        window_id: WindowId,
        split_id: SplitId,
    ) -> Result<ActionOutcome, WorkspaceError> {
        self.window_mut(window_id)?.layout.reset_chain(split_id)?;
        Ok(ActionOutcome::Applied)
    }

    pub(super) fn cancel_detach(
        &mut self,
        window_id: WindowId,
    ) -> Result<ActionOutcome, WorkspaceError> {
        let window = self.window(window_id)?;
        if window.pending_detach.is_none() || window.opened {
            return Err(WorkspaceError::Unsupported(
                "cancel a detach that is not pending",
            ));
        }
        self.windows.remove(&window_id);
        Ok(ActionOutcome::Applied)
    }

    pub(super) fn detach(
        &mut self,
        instance_id: ModuleInstanceId,
    ) -> Result<ActionOutcome, WorkspaceError> {
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
        let position = source.offset_position();
        // The app-wide clock: the source window's own pass time stops while it is
        // minimized (detach can be requested from the settings window then).
        let requested_at = self.display_time;
        let always_on_top = source.always_on_top;
        let min_size = self.module_min_size(instance_id);
        let window_id = WindowId(Self::allocate(&mut self.next_window_id)?);
        let mut window = WindowState::new(Layout::default(), size, always_on_top);
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

    /// Call on the destination's first frame; the source layout retains the instance until then.
    pub(super) fn commit_detach(&mut self, window_id: WindowId) {
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
            self.set_control_error("detach canceled: the module is no longer there".into());
            return;
        }
        let source = self
            .windows
            .get_mut(&pending.source)
            .expect("the source window was located before detaching");
        source
            .layout
            .remove_leaf(pending.instance_id)
            .expect("the instance was located in the source window");
        let source_empty = source.layout.is_empty();
        let window = self
            .windows
            .get_mut(&window_id)
            .expect("pending window exists");
        window
            .layout
            .place_root(pending.instance_id)
            .expect("pending window starts empty");
        window.pending_detach = None;
        if source_empty {
            let _ = self.close_window(pending.source);
        }
        self.sync_visibility();
    }

    pub(super) fn move_module(
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
        let source = self
            .windows
            .get_mut(&source_id)
            .expect("the source window was located before moving");
        source
            .layout
            .remove_leaf(instance_id)
            .expect("the instance was located in the source window");
        let source_empty = source.layout.is_empty();
        let window = self
            .windows
            .get_mut(&target_window)
            .expect("the target window was located before moving");
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
                .expect("the target was validated before moving the module"),
            _ => window
                .layout
                .place_root(instance_id)
                .expect("the target window was validated as empty"),
        }
        if self.move_source == Some(instance_id) {
            self.move_source = None;
        }
        if source_empty && source_id != target_window {
            self.close_window(source_id)?;
        }
        Ok(ActionOutcome::Applied)
    }
}
