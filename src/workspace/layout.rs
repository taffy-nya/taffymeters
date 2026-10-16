//! Binary space partition of one window. Leaves reference Module instances by ID;
//! the Module objects themselves are owned by the Workspace, so moving a leaf never
//! touches the object or its analysis subscriptions.

use eframe::egui::{Pos2, Rect, Vec2, pos2};

use super::ModuleInstanceId;

/// Grid step for divider positions, in logical points.
pub(crate) const GRID_STEP: f32 = 16.0;
/// Half width of the overlay divider hit zone, in logical points.
pub(crate) const DIVIDER_HALF_WIDTH: f32 = 3.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct SplitId(pub(crate) u64);

/// How the two children of a split are arranged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Axis {
    /// `first` on the left, `second` on the right; the divider is a vertical line.
    LeftRight,
    /// `first` above, `second` below; the divider is a horizontal line.
    TopBottom,
}

impl Axis {
    fn extent(self, size: Vec2) -> f32 {
        match self {
            Self::LeftRight => size.x,
            Self::TopBottom => size.y,
        }
    }

    fn start(self, rect: Rect) -> f32 {
        match self {
            Self::LeftRight => rect.min.x,
            Self::TopBottom => rect.min.y,
        }
    }

    fn end(self, rect: Rect) -> f32 {
        match self {
            Self::LeftRight => rect.max.x,
            Self::TopBottom => rect.max.y,
        }
    }

    /// Coordinate of `point` along this axis.
    pub(crate) fn coordinate(self, point: Pos2) -> f32 {
        match self {
            Self::LeftRight => point.x,
            Self::TopBottom => point.y,
        }
    }

    fn combine(self, first: Vec2, second: Vec2) -> Vec2 {
        match self {
            Self::LeftRight => Vec2::new(first.x + second.x, first.y.max(second.y)),
            Self::TopBottom => Vec2::new(first.x.max(second.x), first.y + second.y),
        }
    }

    fn halves(self, rect: Rect, position: f32) -> (Rect, Rect) {
        match self {
            Self::LeftRight => (
                Rect::from_min_max(rect.min, pos2(position, rect.max.y)),
                Rect::from_min_max(pos2(position, rect.min.y), rect.max),
            ),
            Self::TopBottom => (
                Rect::from_min_max(rect.min, pos2(rect.max.x, position)),
                Rect::from_min_max(pos2(rect.min.x, position), rect.max),
            ),
        }
    }
}

/// Where a leaf is inserted relative to a target leaf.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Side {
    Left,
    Right,
    Above,
    Below,
}

impl Side {
    pub(crate) const ALL: [Self; 4] = [Self::Left, Self::Right, Self::Above, Self::Below];

    pub(crate) fn axis(self) -> Axis {
        match self {
            Self::Left | Self::Right => Axis::LeftRight,
            Self::Above | Self::Below => Axis::TopBottom,
        }
    }

    fn new_is_first(self) -> bool {
        matches!(self, Self::Left | Self::Above)
    }
}

#[derive(Debug)]
pub(crate) enum Node {
    Leaf(ModuleInstanceId),
    Split(Box<Split>),
}

#[derive(Debug)]
pub(crate) struct Split {
    pub(crate) id: SplitId,
    pub(crate) axis: Axis,
    /// Desired share of `first` along the axis. Layout clamps it to legal sizes
    /// without rewriting it, so growing the window restores the chosen proportion.
    pub(crate) ratio: f32,
    /// Set once the user dragged this divider; such chains are not re-equalized.
    pub(crate) manual: bool,
    pub(crate) first: Node,
    pub(crate) second: Node,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LayoutError {
    MissingLeaf(ModuleInstanceId),
    MissingSplit(SplitId),
    DuplicateLeaf(ModuleInstanceId),
    NotEmpty,
}

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingLeaf(id) => write!(f, "module {} is not in this layout", id.0),
            Self::MissingSplit(id) => write!(f, "split {} is not in this layout", id.0),
            Self::DuplicateLeaf(id) => write!(f, "module {} is already in a layout", id.0),
            Self::NotEmpty => f.write_str("the window already has a layout"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DividerGeometry {
    pub(crate) id: SplitId,
    pub(crate) axis: Axis,
    /// Area divided by the split.
    pub(crate) parent: Rect,
    /// Absolute coordinate of the divider along `axis`.
    pub(crate) position: f32,
    /// Legal absolute interval for the divider, or `None` when the area is smaller
    /// than both minimum sizes and the children are proportionally squeezed.
    pub(crate) legal: Option<(f32, f32)>,
}

impl DividerGeometry {
    pub(crate) fn hit_rect(&self) -> Rect {
        match self.axis {
            Axis::LeftRight => Rect::from_min_max(
                pos2(self.position - DIVIDER_HALF_WIDTH, self.parent.min.y),
                pos2(self.position + DIVIDER_HALF_WIDTH, self.parent.max.y),
            ),
            Axis::TopBottom => Rect::from_min_max(
                pos2(self.parent.min.x, self.position - DIVIDER_HALF_WIDTH),
                pos2(self.parent.max.x, self.position + DIVIDER_HALF_WIDTH),
            ),
        }
    }

    /// Ratio for a divider dragged to `pointer` (absolute coordinate along the axis).
    /// The legal interval always wins; the grid only chooses inside it, and the
    /// interval's ends are snap points too so either side can reach its minimum.
    pub(crate) fn ratio_for_pointer(&self, pointer: f32, grid: bool) -> f32 {
        let start = self.axis.start(self.parent);
        let span = self.axis.extent(self.parent.size());
        if span <= 0.0 {
            return 0.5;
        }
        let position = match self.legal {
            None => self.position,
            Some((low, high)) => {
                let free = pointer.clamp(low, high);
                if grid {
                    let mut snapped = if free - low <= high - free { low } else { high };
                    let first = ((low - start) / GRID_STEP).ceil();
                    let last = ((high - start) / GRID_STEP).floor();
                    if first <= last {
                        let nearest = ((free - start) / GRID_STEP).round().clamp(first, last);
                        let point = start + nearest * GRID_STEP;
                        if (point - free).abs() <= (snapped - free).abs() {
                            snapped = point;
                        }
                    }
                    snapped
                } else {
                    free
                }
            }
        };
        ((position - start) / span).clamp(0.0, 1.0)
    }
}

#[derive(Debug, Default)]
pub(crate) struct LayoutGeometry {
    pub(crate) leaves: Vec<(ModuleInstanceId, Rect)>,
    pub(crate) dividers: Vec<DividerGeometry>,
}

impl LayoutGeometry {
    pub(crate) fn leaf_rect(&self, id: ModuleInstanceId) -> Option<Rect> {
        self.leaves
            .iter()
            .find(|(leaf, _)| *leaf == id)
            .map(|(_, rect)| *rect)
    }

    #[cfg(test)]
    pub(crate) fn divider(&self, id: SplitId) -> Option<&DividerGeometry> {
        self.dividers.iter().find(|divider| divider.id == id)
    }
}

/// Whether an area can host two subtrees side by side along `axis`.
pub(crate) fn fits(area: Vec2, axis: Axis, first: Vec2, second: Vec2) -> bool {
    let need = axis.combine(first, second);
    area.x + 0.5 >= need.x && area.y + 0.5 >= need.y
}

#[derive(Debug, Default)]
pub(crate) struct Layout {
    root: Option<Node>,
}

impl Layout {
    pub(crate) fn with_leaf(id: ModuleInstanceId) -> Self {
        Self {
            root: Some(Node::Leaf(id)),
        }
    }

    /// A layout with the given tree (restored from a saved workspace).
    pub(crate) fn from_root(root: Option<Node>) -> Self {
        Self { root }
    }

    pub(crate) fn root(&self) -> Option<&Node> {
        self.root.as_ref()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    pub(crate) fn leaves(&self) -> Vec<ModuleInstanceId> {
        let mut leaves = Vec::new();
        if let Some(root) = &self.root {
            collect_leaves(root, &mut leaves);
        }
        leaves
    }

    pub(crate) fn contains(&self, id: ModuleInstanceId) -> bool {
        self.root.as_ref().is_some_and(|root| find_leaf(root, id))
    }

    #[cfg(any(test, feature = "automation"))]
    pub(crate) fn split_ids(&self) -> Vec<SplitId> {
        let mut ids = Vec::new();
        if let Some(root) = &self.root {
            collect_splits(root, &mut ids);
        }
        ids
    }

    pub(crate) fn split(&self, id: SplitId) -> Option<&Split> {
        self.root.as_ref().and_then(|root| find_split(root, id))
    }

    fn split_mut(&mut self, id: SplitId) -> Option<&mut Split> {
        self.root.as_mut().and_then(|root| find_split_mut(root, id))
    }

    pub(crate) fn place_root(&mut self, id: ModuleInstanceId) -> Result<(), LayoutError> {
        if self.root.is_some() {
            return Err(LayoutError::NotEmpty);
        }
        self.root = Some(Node::Leaf(id));
        Ok(())
    }

    /// Put `new` next to `target`. The surrounding chain is re-equalized unless the
    /// user adjusted it. `split_id` must be unused.
    pub(crate) fn insert_beside(
        &mut self,
        target: ModuleInstanceId,
        side: Side,
        new: ModuleInstanceId,
        split_id: SplitId,
    ) -> Result<(), LayoutError> {
        if self.contains(new) {
            return Err(LayoutError::DuplicateLeaf(new));
        }
        let root = self.root.as_mut().ok_or(LayoutError::MissingLeaf(target))?;
        let leaf = find_leaf_mut(root, target).ok_or(LayoutError::MissingLeaf(target))?;
        let (first, second) = if side.new_is_first() {
            (Node::Leaf(new), Node::Leaf(target))
        } else {
            (Node::Leaf(target), Node::Leaf(new))
        };
        *leaf = Node::Split(Box::new(Split {
            id: split_id,
            axis: side.axis(),
            ratio: 0.5,
            manual: false,
            first,
            second,
        }));
        self.equalize();
        Ok(())
    }

    /// Remove a leaf; its sibling subtree takes the parent's place.
    pub(crate) fn remove_leaf(&mut self, target: ModuleInstanceId) -> Result<(), LayoutError> {
        let root = self.root.take().ok_or(LayoutError::MissingLeaf(target))?;
        match remove_from(root, target) {
            Removal::NotFound(root) => {
                self.root = Some(root);
                Err(LayoutError::MissingLeaf(target))
            }
            Removal::Removed(rest) => {
                self.root = rest;
                self.equalize();
                Ok(())
            }
        }
    }

    pub(crate) fn replace_leaf(
        &mut self,
        old: ModuleInstanceId,
        new: ModuleInstanceId,
    ) -> Result<(), LayoutError> {
        if self.contains(new) {
            return Err(LayoutError::DuplicateLeaf(new));
        }
        let root = self.root.as_mut().ok_or(LayoutError::MissingLeaf(old))?;
        let leaf = find_leaf_mut(root, old).ok_or(LayoutError::MissingLeaf(old))?;
        *leaf = Node::Leaf(new);
        Ok(())
    }

    pub(crate) fn set_ratio(
        &mut self,
        id: SplitId,
        ratio: f32,
        manual: bool,
    ) -> Result<(), LayoutError> {
        let split = self.split_mut(id).ok_or(LayoutError::MissingSplit(id))?;
        split.ratio = if ratio.is_finite() {
            ratio.clamp(0.0, 1.0)
        } else {
            0.5
        };
        if manual {
            split.manual = true;
        }
        Ok(())
    }

    /// Double-click reset: clear the manual mark of the whole same-axis chain that
    /// contains `id` and distribute it evenly again.
    pub(crate) fn reset_chain(&mut self, id: SplitId) -> Result<(), LayoutError> {
        let top = self
            .root
            .as_ref()
            .and_then(|root| chain_top_id(root, id, None, None))
            .ok_or(LayoutError::MissingSplit(id))?;
        let split = self.split_mut(top).ok_or(LayoutError::MissingSplit(top))?;
        let axis = split.axis;
        clear_manual(split, axis);
        self.equalize();
        Ok(())
    }

    /// Re-distribute every same-axis chain no user has adjusted.
    fn equalize(&mut self) {
        if let Some(root) = self.root.take() {
            self.root = Some(equalize_node(root));
        }
    }

    pub(crate) fn min_size(&self, min_leaf: &dyn Fn(ModuleInstanceId) -> Vec2) -> Option<Vec2> {
        self.root.as_ref().map(|root| node_min_size(root, min_leaf))
    }

    pub(crate) fn geometry(
        &self,
        rect: Rect,
        min_leaf: &dyn Fn(ModuleInstanceId) -> Vec2,
    ) -> LayoutGeometry {
        let mut geometry = LayoutGeometry::default();
        if let Some(root) = &self.root {
            layout_node(root, rect, min_leaf, &mut geometry);
        }
        geometry
    }
}

fn collect_leaves(node: &Node, out: &mut Vec<ModuleInstanceId>) {
    match node {
        Node::Leaf(id) => out.push(*id),
        Node::Split(split) => {
            collect_leaves(&split.first, out);
            collect_leaves(&split.second, out);
        }
    }
}

#[cfg(any(test, feature = "automation"))]
fn collect_splits(node: &Node, out: &mut Vec<SplitId>) {
    if let Node::Split(split) = node {
        out.push(split.id);
        collect_splits(&split.first, out);
        collect_splits(&split.second, out);
    }
}

fn find_leaf(node: &Node, id: ModuleInstanceId) -> bool {
    match node {
        Node::Leaf(leaf) => *leaf == id,
        Node::Split(split) => find_leaf(&split.first, id) || find_leaf(&split.second, id),
    }
}

fn find_leaf_mut(node: &mut Node, id: ModuleInstanceId) -> Option<&mut Node> {
    match node {
        Node::Leaf(leaf) if *leaf == id => Some(node),
        Node::Leaf(_) => None,
        Node::Split(split) => {
            if find_leaf(&split.first, id) {
                find_leaf_mut(&mut split.first, id)
            } else {
                find_leaf_mut(&mut split.second, id)
            }
        }
    }
}

fn find_split(node: &Node, id: SplitId) -> Option<&Split> {
    match node {
        Node::Leaf(_) => None,
        Node::Split(split) if split.id == id => Some(split),
        Node::Split(split) => {
            find_split(&split.first, id).or_else(|| find_split(&split.second, id))
        }
    }
}

fn find_split_mut(node: &mut Node, id: SplitId) -> Option<&mut Split> {
    let Node::Split(split) = node else {
        return None;
    };
    if split.id == id {
        return Some(split);
    }
    if find_split(&split.first, id).is_some() {
        find_split_mut(&mut split.first, id)
    } else {
        find_split_mut(&mut split.second, id)
    }
}

/// ID of the topmost split of the same-axis chain containing split `id`.
fn chain_top_id(
    node: &Node,
    id: SplitId,
    current_top: Option<SplitId>,
    parent_axis: Option<Axis>,
) -> Option<SplitId> {
    let Node::Split(split) = node else {
        return None;
    };
    let top = if parent_axis == Some(split.axis) {
        current_top
    } else {
        Some(split.id)
    };
    if split.id == id {
        return top;
    }
    chain_top_id(&split.first, id, top, Some(split.axis))
        .or_else(|| chain_top_id(&split.second, id, top, Some(split.axis)))
}

fn clear_manual(split: &mut Split, axis: Axis) {
    split.manual = false;
    for child in [&mut split.first, &mut split.second] {
        if let Node::Split(child) = child
            && child.axis == axis
        {
            clear_manual(child, axis);
        }
    }
}

enum Removal {
    NotFound(Node),
    /// The target is gone; `None` when the target was the whole subtree.
    Removed(Option<Node>),
}

fn remove_from(node: Node, target: ModuleInstanceId) -> Removal {
    match node {
        Node::Leaf(id) if id == target => Removal::Removed(None),
        leaf @ Node::Leaf(_) => Removal::NotFound(leaf),
        Node::Split(mut split) => {
            match remove_from(
                std::mem::replace(&mut split.first, Node::Leaf(target)),
                target,
            ) {
                Removal::Removed(None) => return Removal::Removed(Some(split.second)),
                Removal::Removed(Some(rest)) => {
                    split.first = rest;
                    return Removal::Removed(Some(Node::Split(split)));
                }
                Removal::NotFound(first) => split.first = first,
            }
            match remove_from(
                std::mem::replace(&mut split.second, Node::Leaf(target)),
                target,
            ) {
                Removal::Removed(None) => Removal::Removed(Some(split.first)),
                Removal::Removed(Some(rest)) => {
                    split.second = rest;
                    Removal::Removed(Some(Node::Split(split)))
                }
                Removal::NotFound(second) => {
                    split.second = second;
                    Removal::NotFound(Node::Split(split))
                }
            }
        }
    }
}

fn chain_is_manual(node: &Node, axis: Axis) -> bool {
    match node {
        Node::Split(split) if split.axis == axis => {
            split.manual
                || chain_is_manual(&split.first, axis)
                || chain_is_manual(&split.second, axis)
        }
        _ => false,
    }
}

fn flatten_chain(node: Node, axis: Axis, members: &mut Vec<Node>, ids: &mut Vec<SplitId>) {
    match node {
        Node::Split(split) if split.axis == axis => {
            let split = *split;
            ids.push(split.id);
            flatten_chain(split.first, axis, members, ids);
            flatten_chain(split.second, axis, members, ids);
        }
        other => members.push(other),
    }
}

fn equalize_node(node: Node) -> Node {
    let axis = match &node {
        Node::Leaf(_) => return node,
        Node::Split(split) => split.axis,
    };
    if chain_is_manual(&node, axis) {
        // Keep the user's proportions; still normalize nested chains of the other axis.
        let Node::Split(split) = node else {
            unreachable!("checked above")
        };
        let split = *split;
        return Node::Split(Box::new(Split {
            first: equalize_children(split.first, axis),
            second: equalize_children(split.second, axis),
            ..split
        }));
    }
    let mut members = Vec::new();
    let mut ids = Vec::new();
    flatten_chain(node, axis, &mut members, &mut ids);
    let members: Vec<Node> = members.into_iter().map(equalize_node).collect();
    rebuild_chain(members, axis, &mut ids.into_iter())
}

/// Within a manual chain, same-axis children belong to that chain; only other-axis
/// subtrees start chains of their own.
fn equalize_children(node: Node, axis: Axis) -> Node {
    match node {
        Node::Split(split) if split.axis == axis => {
            let split = *split;
            Node::Split(Box::new(Split {
                first: equalize_children(split.first, axis),
                second: equalize_children(split.second, axis),
                ..split
            }))
        }
        other => equalize_node(other),
    }
}

fn rebuild_chain(
    mut members: Vec<Node>,
    axis: Axis,
    ids: &mut impl Iterator<Item = SplitId>,
) -> Node {
    if members.len() == 1 {
        return members.pop().expect("chain has one member");
    }
    let count = members.len();
    let first = members.remove(0);
    let id = ids.next().expect("a chain of n members has n - 1 splits");
    let second = rebuild_chain(members, axis, ids);
    Node::Split(Box::new(Split {
        id,
        axis,
        ratio: 1.0 / count as f32,
        manual: false,
        first,
        second,
    }))
}

fn node_min_size(node: &Node, min_leaf: &dyn Fn(ModuleInstanceId) -> Vec2) -> Vec2 {
    match node {
        Node::Leaf(id) => min_leaf(*id),
        Node::Split(split) => split.axis.combine(
            node_min_size(&split.first, min_leaf),
            node_min_size(&split.second, min_leaf),
        ),
    }
}

fn layout_node(
    node: &Node,
    rect: Rect,
    min_leaf: &dyn Fn(ModuleInstanceId) -> Vec2,
    out: &mut LayoutGeometry,
) {
    match node {
        Node::Leaf(id) => out.leaves.push((*id, rect)),
        Node::Split(split) => {
            let axis = split.axis;
            let first_min = axis.extent(node_min_size(&split.first, min_leaf));
            let second_min = axis.extent(node_min_size(&split.second, min_leaf));
            let start = axis.start(rect);
            let end = axis.end(rect);
            let span = end - start;
            let low = start + first_min;
            let high = end - second_min;
            let (position, legal) = if low <= high {
                (
                    (start + split.ratio * span).clamp(low, high),
                    Some((low, high)),
                )
            } else {
                let total = first_min + second_min;
                let share = if total > 0.0 { first_min / total } else { 0.5 };
                (start + span * share, None)
            };
            out.dividers.push(DividerGeometry {
                id: split.id,
                axis,
                parent: rect,
                position,
                legal,
            });
            let (first, second) = axis.halves(rect, position);
            layout_node(&split.first, first, min_leaf, out);
            layout_node(&split.second, second, min_leaf, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn id(value: u64) -> ModuleInstanceId {
        ModuleInstanceId(value)
    }

    fn min(_: ModuleInstanceId) -> Vec2 {
        Vec2::new(120.0, 80.0)
    }

    fn ratios(layout: &Layout) -> Vec<(SplitId, f32)> {
        layout
            .split_ids()
            .into_iter()
            .map(|split| (split, layout.split(split).unwrap().ratio))
            .collect()
    }

    #[test]
    fn untouched_chain_is_equalized_and_manual_chain_keeps_its_ratio() {
        let mut layout = Layout::with_leaf(id(1));
        layout
            .insert_beside(id(1), Side::Right, id(2), SplitId(1))
            .unwrap();
        layout
            .insert_beside(id(2), Side::Right, id(3), SplitId(2))
            .unwrap();
        assert_eq!(layout.leaves(), vec![id(1), id(2), id(3)]);
        let geometry = layout.geometry(
            Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 400.0)),
            &min,
        );
        let widths: Vec<f32> = geometry.leaves.iter().map(|(_, r)| r.width()).collect();
        assert!(
            widths.iter().all(|w| (w - 300.0).abs() < 0.01),
            "{widths:?}"
        );

        layout.set_ratio(SplitId(1), 0.2, true).unwrap();
        let before = ratios(&layout);
        layout
            .insert_beside(id(3), Side::Right, id(4), SplitId(3))
            .unwrap();
        let after = ratios(&layout);
        assert_eq!(after[0], before[0], "manual ratio was rewritten");

        layout.remove_leaf(id(4)).unwrap();
        layout.reset_chain(SplitId(1)).unwrap();
        let geometry = layout.geometry(
            Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 400.0)),
            &min,
        );
        assert!(
            geometry
                .leaves
                .iter()
                .all(|(_, r)| (r.width() - 300.0).abs() < 0.01)
        );
    }

    #[test]
    fn removing_a_leaf_lets_the_sibling_fill_and_keeps_split_ids_stable() {
        let mut layout = Layout::with_leaf(id(1));
        layout
            .insert_beside(id(1), Side::Right, id(2), SplitId(1))
            .unwrap();
        layout
            .insert_beside(id(2), Side::Below, id(3), SplitId(2))
            .unwrap();
        layout.remove_leaf(id(1)).unwrap();
        assert_eq!(layout.leaves(), vec![id(2), id(3)]);
        assert_eq!(layout.split_ids(), vec![SplitId(2)]);
        assert_eq!(
            layout.remove_leaf(id(9)),
            Err(LayoutError::MissingLeaf(id(9)))
        );
        assert_eq!(layout.leaves(), vec![id(2), id(3)]);
        layout.remove_leaf(id(2)).unwrap();
        layout.remove_leaf(id(3)).unwrap();
        assert!(layout.is_empty());
    }

    #[test]
    fn divider_respects_minimum_sizes_before_the_grid() {
        let mut layout = Layout::with_leaf(id(1));
        layout
            .insert_beside(id(1), Side::Right, id(2), SplitId(1))
            .unwrap();
        let rect = Rect::from_min_size(pos2(10.0, 0.0), Vec2::new(300.0, 200.0));
        let geometry = layout.geometry(rect, &min);
        let divider = *geometry.divider(SplitId(1)).unwrap();
        assert_eq!(divider.legal, Some((130.0, 190.0)));
        // Far left: clamped to the first child's minimum; the interval's end is a snap
        // point, nearer than the first grid point inside (anchored at x=10: 138).
        let ratio = divider.ratio_for_pointer(-500.0, true);
        assert!((ratio * 300.0 + 10.0 - 130.0).abs() < 0.01, "{ratio}");
        // Far right: the end 190 lies off the grid (last grid point 186) and still wins.
        let ratio = divider.ratio_for_pointer(1000.0, true);
        assert!((ratio * 300.0 + 10.0 - 190.0).abs() < 0.01, "{ratio}");
        // Near an end but nearer a grid point: the grid point.
        let ratio = divider.ratio_for_pointer(135.0, true);
        assert!((ratio * 300.0 + 10.0 - 138.0).abs() < 0.01, "{ratio}");
        // Alt: free but still legal.
        let ratio = divider.ratio_for_pointer(-500.0, false);
        assert!((ratio * 300.0 + 10.0 - 130.0).abs() < 0.01);
        let ratio = divider.ratio_for_pointer(150.0, true);
        assert!((ratio * 300.0 + 10.0 - 154.0).abs() < 0.01);

        // Too small for both minimums: proportional squeeze, no legal interval.
        let tiny = Rect::from_min_size(Pos2::ZERO, Vec2::new(100.0, 50.0));
        let geometry = layout.geometry(tiny, &min);
        assert_eq!(geometry.dividers[0].legal, None);
        assert_eq!(geometry.leaves[0].1.width(), 50.0);
        assert_eq!(layout.min_size(&min), Some(Vec2::new(240.0, 80.0)));
    }

    #[test]
    fn insertion_sides_and_duplicates() {
        let mut layout = Layout::with_leaf(id(1));
        layout
            .insert_beside(id(1), Side::Left, id(2), SplitId(1))
            .unwrap();
        layout
            .insert_beside(id(1), Side::Above, id(3), SplitId(2))
            .unwrap();
        assert_eq!(layout.leaves(), vec![id(2), id(3), id(1)]);
        assert_eq!(
            layout.insert_beside(id(1), Side::Right, id(2), SplitId(3)),
            Err(LayoutError::DuplicateLeaf(id(2)))
        );
        assert_eq!(layout.leaves(), vec![id(2), id(3), id(1)]);
        assert!(fits(
            Vec2::new(240.0, 80.0),
            Axis::LeftRight,
            min(id(1)),
            min(id(2))
        ));
        assert!(!fits(
            Vec2::new(239.0, 80.0),
            Axis::LeftRight,
            min(id(1)),
            min(id(2))
        ));
    }
}
