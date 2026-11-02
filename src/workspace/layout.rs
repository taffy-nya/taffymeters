//! Geometry is in window-local logical points; leaves only reference instance IDs.

use std::{collections::HashMap, error::Error, fmt};

use egui::{Pos2, Rect, Vec2, pos2};

use super::ModuleInstanceId;

pub(crate) const GRID_STEP_POINTS: f32 = 16.0;
pub(crate) const DIVIDER_HALF_WIDTH_POINTS: f32 = 3.0;
/// Ctrl-drag snap points, as fractions of the arrangement group.
const SNAP_FRACTIONS: [f32; 5] = [0.25, 1.0 / 3.0, 0.5, 2.0 / 3.0, 0.75];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct SplitId(pub(crate) u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Axis {
    LeftRight,
    TopBottom,
}

impl Axis {
    fn other(self) -> Self {
        match self {
            Self::LeftRight => Self::TopBottom,
            Self::TopBottom => Self::LeftRight,
        }
    }

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

#[derive(Clone, Debug)]
pub(crate) enum Node {
    Leaf(ModuleInstanceId),
    Split(Box<Split>),
}

#[derive(Clone, Debug)]
pub(crate) struct Split {
    pub(crate) id: SplitId,
    pub(crate) axis: Axis,
    /// Desired share of `first` along the axis. Layout clamps it to legal sizes
    /// without rewriting it, so growing the window restores the chosen proportion.
    pub(crate) ratio: f32,
    /// Custom: the user placed this divider. Split and close equalize only between
    /// Custom dividers and never move them.
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

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingLeaf(id) => write!(f, "module {} is not in this layout", id.0),
            Self::MissingSplit(id) => write!(f, "split {} is not in this layout", id.0),
            Self::DuplicateLeaf(id) => write!(f, "module {} is already in a layout", id.0),
            Self::NotEmpty => f.write_str("the window already has a layout"),
        }
    }
}

impl Error for LayoutError {}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DividerGeometry {
    pub(crate) id: SplitId,
    pub(crate) axis: Axis,
    pub(crate) parent: Rect,
    /// Absolute coordinate of the divider along `axis`.
    pub(crate) position: f32,
    /// Absolute extent along `axis` of the arrangement group (same-axis chain).
    group: (f32, f32),
}

impl DividerGeometry {
    pub(crate) fn hit_rect(&self) -> Rect {
        match self.axis {
            Axis::LeftRight => Rect::from_min_max(
                pos2(self.position - DIVIDER_HALF_WIDTH_POINTS, self.parent.min.y),
                pos2(self.position + DIVIDER_HALF_WIDTH_POINTS, self.parent.max.y),
            ),
            Axis::TopBottom => Rect::from_min_max(
                pos2(self.parent.min.x, self.position - DIVIDER_HALF_WIDTH_POINTS),
                pos2(self.parent.max.x, self.position + DIVIDER_HALF_WIDTH_POINTS),
            ),
        }
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

    pub(crate) fn divider(&self, id: SplitId) -> Option<&DividerGeometry> {
        self.dividers.iter().find(|divider| divider.id == id)
    }
}

fn fits(area: Vec2, axis: Axis, first: Vec2, second: Vec2) -> bool {
    let need = axis.combine(first, second);
    area.x + 0.5 >= need.x && area.y + 0.5 >= need.y
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Snap {
    /// The 16-point grid from the group start, plus the legal endpoints.
    Grid,
    /// Group fractions, other parallel dividers and the legal endpoints.
    Fractions,
    Off,
}

#[derive(Clone, Copy, Debug)]
struct Anchor {
    axis: Axis,
    position: f32,
    parent: Rect,
    ratio: f32,
    custom: bool,
}

/// One divider drag, from the layout as it was when the drag started. Every frame is
/// recomputed from this snapshot, so the tree must not change structurally meanwhile.
#[derive(Clone, Debug)]
pub(crate) struct DividerDragPlan {
    hit: SplitId,
    axis: Axis,
    rect: Rect,
    start: f32,
    group: (f32, f32),
    range: (f32, f32),
    aligned: Vec<SplitId>,
    aligned_range: (f32, f32),
    anchors: HashMap<SplitId, Anchor>,
}

impl DividerDragPlan {
    pub(crate) fn hit(&self) -> SplitId {
        self.hit
    }

    pub(crate) fn rect(&self) -> Rect {
        self.rect
    }

    pub(crate) fn start(&self) -> f32 {
        self.start
    }

    #[cfg(test)]
    pub(crate) fn aligned(&self) -> &[SplitId] {
        &self.aligned
    }

    pub(crate) fn moves(&self, id: SplitId, aligned: bool) -> bool {
        id == self.hit || (aligned && self.aligned.contains(&id))
    }

    /// Every split as the drag found it, to restore on cancel.
    pub(crate) fn original(&self) -> Vec<(SplitId, f32, bool)> {
        self.anchors
            .iter()
            .map(|(id, anchor)| (*id, anchor.ratio, anchor.custom))
            .collect()
    }

    /// `pointer` is absolute along the axis. Candidates outside the legal range clamp to
    /// its endpoints; the nearest wins and ties go to the smaller coordinate.
    pub(crate) fn position(&self, pointer: f32, snap: Snap, aligned: bool) -> f32 {
        let (low, high) = if aligned {
            self.aligned_range
        } else {
            self.range
        };
        let free = pointer.clamp(low, high);
        let (start, end) = self.group;
        let mut candidates = vec![low, high];
        match snap {
            Snap::Off => return free,
            Snap::Grid => {
                let first = ((low - start) / GRID_STEP_POINTS).ceil();
                let last = ((high - start) / GRID_STEP_POINTS).floor();
                if first <= last {
                    let step = ((free - start) / GRID_STEP_POINTS)
                        .round()
                        .clamp(first, last);
                    candidates.push(start + step * GRID_STEP_POINTS);
                }
            }
            Snap::Fractions => {
                candidates.extend(
                    SNAP_FRACTIONS
                        .iter()
                        .map(|fraction| start + (end - start) * fraction),
                );
                candidates.extend(
                    self.anchors
                        .iter()
                        .filter(|(id, anchor)| {
                            anchor.axis == self.axis && !self.moves(**id, aligned)
                        })
                        .map(|(_, anchor)| anchor.position),
                );
            }
        }
        candidates
            .into_iter()
            .map(|candidate| candidate.clamp(low, high))
            .min_by(|a, b| {
                (a - free)
                    .abs()
                    .total_cmp(&(b - free).abs())
                    .then(a.total_cmp(b))
            })
            .unwrap_or(free)
    }
}

/// A leaf or divider that sits directly in one arrangement group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Member {
    Leaf(ModuleInstanceId),
    Split(SplitId),
}

/// An arrangement group flattened from a same-axis chain: `dividers[k]` lies between
/// `members[k]` and `members[k + 1]`; shares are fractions of the group's extent.
struct Group {
    axis: Axis,
    members: Vec<Node>,
    shares: Vec<f32>,
    dividers: Vec<(SplitId, bool)>,
}

impl Group {
    fn flatten(split: Split) -> Self {
        let mut group = Self {
            axis: split.axis,
            members: Vec::new(),
            shares: Vec::new(),
            dividers: Vec::new(),
        };
        group.push(Node::Split(Box::new(split)), 1.0);
        group
    }

    fn push(&mut self, node: Node, share: f32) {
        match node {
            Node::Split(split) if split.axis == self.axis => {
                let split = *split;
                self.push(split.first, share * split.ratio);
                self.dividers.push((split.id, split.manual));
                self.push(split.second, share * (1.0 - split.ratio));
            }
            member => {
                self.members.push(member);
                self.shares.push(share);
            }
        }
    }

    fn index(&self, leaf: ModuleInstanceId) -> usize {
        self.members
            .iter()
            .position(|member| matches!(member, Node::Leaf(id) if *id == leaf))
            .expect("the group was selected because it holds this leaf")
    }

    /// Equal shares for the members between the Custom dividers around `member`.
    fn equalize_around(&mut self, member: usize) {
        let first = self.dividers[..member]
            .iter()
            .rposition(|(_, custom)| *custom)
            .map_or(0, |divider| divider + 1);
        let last = self.dividers[member..]
            .iter()
            .position(|(_, custom)| *custom)
            .map_or(self.members.len() - 1, |offset| member + offset);
        let total: f32 = self.shares[first..=last].iter().sum();
        let share = total / (last - first + 1) as f32;
        self.shares[first..=last].fill(share);
    }

    fn insert(
        &mut self,
        target: ModuleInstanceId,
        before: bool,
        new: ModuleInstanceId,
        id: SplitId,
    ) {
        let index = self.index(target);
        let at = if before { index } else { index + 1 };
        self.members.insert(at, Node::Leaf(new));
        self.shares.insert(at, 0.0);
        self.dividers.insert(index, (id, false));
        self.equalize_around(index);
    }

    /// Delete the Auto side when the two sides differ, otherwise the trailing one;
    /// the member across the deleted divider takes the space.
    fn remove(&mut self, target: ModuleInstanceId) {
        let index = self.index(target);
        let leading = index.checked_sub(1).map(|divider| self.dividers[divider].1);
        let trailing = self.dividers.get(index).map(|divider| divider.1);
        let take_trailing = match (leading, trailing) {
            (Some(leading), Some(trailing)) => leading || !trailing,
            (_, trailing) => trailing.is_some(),
        };
        self.members.remove(index);
        let share = self.shares.remove(index);
        let receiver = if take_trailing {
            self.dividers.remove(index);
            index
        } else {
            self.dividers.remove(index - 1);
            index - 1
        };
        self.shares[receiver] += share;
        self.equalize_around(receiver);
    }

    fn reset(&mut self) {
        for divider in &mut self.dividers {
            divider.1 = false;
        }
        let share = 1.0 / self.members.len() as f32;
        self.shares.fill(share);
    }

    /// A single remaining member replaces the group and so joins any same-axis outer group.
    fn build(self) -> Node {
        let Self {
            axis,
            members,
            shares,
            dividers,
        } = self;
        let mut rest = members.into_iter().zip(shares).rev();
        let (mut node, mut tail) = rest.next().expect("a group keeps at least one member");
        for ((member, share), (id, custom)) in rest.zip(dividers.into_iter().rev()) {
            let total = share + tail;
            let ratio = if total > 0.0 { share / total } else { 0.5 };
            node = Node::Split(Box::new(Split {
                id,
                axis,
                ratio,
                manual: custom,
                first: member,
                second: node,
            }));
            tail = total;
        }
        node
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Layout {
    root: Option<Node>,
}

impl Layout {
    #[cfg(any(test, feature = "automation"))]
    pub(crate) fn with_leaf(id: ModuleInstanceId) -> Self {
        Self {
            root: Some(Node::Leaf(id)),
        }
    }

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

    /// `split_id` must be unused. In the target's group the new Auto divider joins the
    /// interval between Custom dividers, which is equalized; across a perpendicular
    /// group only the target is halved.
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
        let root = self.root.take().ok_or(LayoutError::MissingLeaf(target))?;
        if !find_leaf(&root, target) {
            self.root = Some(root);
            return Err(LayoutError::MissingLeaf(target));
        }
        let axis = side.axis();
        let before = side.new_is_first();
        let in_group =
            group_top(&root, None, Member::Leaf(target)).is_some_and(|group| group.axis == axis);
        self.root = Some(if in_group {
            edit_group(root, None, Member::Leaf(target), &mut |group| {
                group.insert(target, before, new, split_id);
            })
        } else {
            let mut root = root;
            let leaf = find_leaf_mut(&mut root, target).expect("the leaf was found above");
            let (first, second) = if before {
                (Node::Leaf(new), Node::Leaf(target))
            } else {
                (Node::Leaf(target), Node::Leaf(new))
            };
            *leaf = Node::Split(Box::new(Split {
                id: split_id,
                axis,
                ratio: 0.5,
                manual: false,
                first,
                second,
            }));
            root
        });
        Ok(())
    }

    /// Surviving dividers keep their IDs and states; the result depends only on the
    /// visible layout, not on the tree shape.
    pub(crate) fn remove_leaf(&mut self, target: ModuleInstanceId) -> Result<(), LayoutError> {
        let root = self.root.take().ok_or(LayoutError::MissingLeaf(target))?;
        if !find_leaf(&root, target) {
            self.root = Some(root);
            return Err(LayoutError::MissingLeaf(target));
        }
        self.root = match root {
            Node::Leaf(_) => None,
            root => Some(edit_group(root, None, Member::Leaf(target), &mut |group| {
                group.remove(target);
            })),
        };
        Ok(())
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

    /// Assigns each split's desired ratio and Custom state; changes nothing when any ID
    /// is missing.
    pub(crate) fn set_splits(
        &mut self,
        splits: &[(SplitId, f32, bool)],
    ) -> Result<(), LayoutError> {
        if let Some((missing, ..)) = splits.iter().find(|(id, ..)| self.split(*id).is_none()) {
            return Err(LayoutError::MissingSplit(*missing));
        }
        for &(id, ratio, custom) in splits {
            let split = self.split_mut(id).expect("every ID was checked above");
            split.ratio = if ratio.is_finite() {
                ratio.clamp(0.0, 1.0)
            } else {
                0.5
            };
            split.manual = custom;
        }
        Ok(())
    }

    /// Equalizes the whole group holding `id` and makes all its dividers Auto.
    pub(crate) fn reset_group(&mut self, id: SplitId) -> Result<(), LayoutError> {
        let root = self.root.take().ok_or(LayoutError::MissingSplit(id))?;
        if find_split(&root, id).is_none() {
            self.root = Some(root);
            return Err(LayoutError::MissingSplit(id));
        }
        self.root = Some(edit_group(root, None, Member::Split(id), &mut Group::reset));
        Ok(())
    }

    /// Whether `incoming` fits beside `target` once the interval it joins is equalized.
    pub(crate) fn insertion_fits(
        &self,
        geometry: &LayoutGeometry,
        target: ModuleInstanceId,
        side: Side,
        incoming: Vec2,
        min_leaf: &dyn Fn(ModuleInstanceId) -> Vec2,
    ) -> bool {
        let (Some(root), Some(rect)) = (&self.root, geometry.leaf_rect(target)) else {
            return false;
        };
        let axis = side.axis();
        let Some(group) =
            group_top(root, None, Member::Leaf(target)).filter(|group| group.axis == axis)
        else {
            return fits(rect.size(), axis, min_leaf(target), incoming);
        };
        let mut members = Vec::new();
        let mut dividers = Vec::new();
        chain_view(group, &mut members, &mut dividers);
        let index = members
            .iter()
            .position(|member| matches!(member, Node::Leaf(id) if *id == target))
            .expect("the group was selected because it holds this leaf");
        let Some(&DividerGeometry {
            group: (group_start, group_end),
            ..
        }) = geometry.divider(group.id)
        else {
            return false;
        };
        let bound = |divider: Option<usize>, fallback: f32| {
            divider
                .and_then(|divider| geometry.divider(dividers[divider].0))
                .map_or(fallback, |divider| divider.position)
        };
        let first = dividers[..index].iter().rposition(|(_, custom)| *custom);
        let last = dividers[index..]
            .iter()
            .position(|(_, custom)| *custom)
            .map(|offset| index + offset);
        let start = bound(first, group_start);
        let end = bound(last, group_end);
        let members =
            &members[first.map_or(0, |divider| divider + 1)..=last.unwrap_or(members.len() - 1)];
        let need: f32 = members
            .iter()
            .map(|member| axis.extent(node_min_size(member, min_leaf)))
            .sum::<f32>()
            + axis.extent(incoming);
        let cross = axis.other();
        end - start + 0.5 >= need && cross.extent(rect.size()) + 0.5 >= cross.extent(incoming)
    }

    /// `tolerance` is the alignment distance, in points, for Shift-dragging.
    pub(crate) fn drag_plan(
        &self,
        rect: Rect,
        min_leaf: &dyn Fn(ModuleInstanceId) -> Vec2,
        hit: SplitId,
        tolerance: f32,
    ) -> Option<DividerDragPlan> {
        let root = self.root.as_ref()?;
        let geometry = self.geometry(rect, min_leaf);
        let divider = *geometry.divider(hit)?;
        let mut anchors = HashMap::new();
        collect_anchors(root, &geometry, &mut anchors);
        let range = |id| free_range(root, id, &geometry, min_leaf);
        let own = range(hit)?;
        let aligned: Vec<SplitId> = geometry
            .dividers
            .iter()
            .filter(|other| {
                other.id != hit
                    && other.axis == divider.axis
                    && (other.position - divider.position).abs() <= tolerance
            })
            .map(|other| other.id)
            .collect();
        let (low, high) = aligned
            .iter()
            .filter_map(|id| range(*id))
            .fold(own, |(low, high), (other_low, other_high)| {
                (low.max(other_low), high.min(other_high))
            });
        let aligned_range = if low <= high {
            (low, high)
        } else {
            (divider.position, divider.position)
        };
        Some(DividerDragPlan {
            hit,
            axis: divider.axis,
            rect,
            start: divider.position,
            group: divider.group,
            range: own,
            aligned,
            aligned_range,
            anchors,
        })
    }

    /// Every split's ratio and state with the moved dividers at `position` and all
    /// others at their start coordinates. Splits whose area is unchanged keep their
    /// desired ratio, even when the layout currently clamps it.
    pub(crate) fn drag_updates(
        &self,
        plan: &DividerDragPlan,
        position: f32,
        aligned: bool,
    ) -> Vec<(SplitId, f32, bool)> {
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            place(root, plan.rect, plan, position, aligned, &mut out);
        }
        out
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
            layout_node(root, rect, None, min_leaf, &mut geometry);
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

fn chain_holds(split: &Split, member: Member) -> bool {
    member == Member::Split(split.id)
        || [&split.first, &split.second]
            .into_iter()
            .any(|child| match child {
                Node::Split(child) if child.axis == split.axis => chain_holds(child, member),
                Node::Leaf(id) => member == Member::Leaf(*id),
                Node::Split(_) => false,
            })
}

/// The top split of the group that directly holds `member`.
fn group_top(node: &Node, parent_axis: Option<Axis>, member: Member) -> Option<&Split> {
    let Node::Split(split) = node else {
        return None;
    };
    if parent_axis != Some(split.axis) && chain_holds(split, member) {
        return Some(split);
    }
    group_top(&split.first, Some(split.axis), member)
        .or_else(|| group_top(&split.second, Some(split.axis), member))
}

/// Rebuilds the group that directly holds `member` after `edit`; other groups keep
/// their shape.
fn edit_group(
    node: Node,
    parent_axis: Option<Axis>,
    member: Member,
    edit: &mut dyn FnMut(&mut Group),
) -> Node {
    let Node::Split(mut split) = node else {
        return node;
    };
    let axis = split.axis;
    if parent_axis != Some(axis) && chain_holds(&split, member) {
        let mut group = Group::flatten(*split);
        edit(&mut group);
        return group.build();
    }
    split.first = edit_group(split.first, Some(axis), member, edit);
    split.second = edit_group(split.second, Some(axis), member, edit);
    Node::Split(split)
}

fn chain_view<'a>(
    split: &'a Split,
    members: &mut Vec<&'a Node>,
    dividers: &mut Vec<(SplitId, bool)>,
) {
    for (index, child) in [&split.first, &split.second].into_iter().enumerate() {
        if index == 1 {
            dividers.push((split.id, split.manual));
        }
        match child {
            Node::Split(child) if child.axis == split.axis => chain_view(child, members, dividers),
            member => members.push(member),
        }
    }
}

/// Leaves touching the far end (`trailing`) or the near end of `node` along `axis`.
fn edge_leaves(node: &Node, axis: Axis, trailing: bool, out: &mut Vec<ModuleInstanceId>) {
    match node {
        Node::Leaf(id) => out.push(*id),
        Node::Split(split) if split.axis == axis => {
            let edge = if trailing {
                &split.second
            } else {
                &split.first
            };
            edge_leaves(edge, axis, trailing, out);
        }
        Node::Split(split) => {
            edge_leaves(&split.first, axis, trailing, out);
            edge_leaves(&split.second, axis, trailing, out);
        }
    }
}

/// With every other divider fixed, how far a divider can move before a leaf touching
/// it reaches its minimum. Always contains the current position, so a squeezed layout
/// never makes the divider jump.
fn free_range(
    root: &Node,
    id: SplitId,
    geometry: &LayoutGeometry,
    min_leaf: &dyn Fn(ModuleInstanceId) -> Vec2,
) -> Option<(f32, f32)> {
    let split = find_split(root, id)?;
    let divider = geometry.divider(id)?;
    let axis = split.axis;
    let mut leaves = Vec::new();
    edge_leaves(&split.first, axis, true, &mut leaves);
    let low = leaves
        .iter()
        .filter_map(|leaf| {
            geometry
                .leaf_rect(*leaf)
                .map(|rect| axis.start(rect) + axis.extent(min_leaf(*leaf)))
        })
        .fold(axis.start(divider.parent), f32::max);
    leaves.clear();
    edge_leaves(&split.second, axis, false, &mut leaves);
    let high = leaves
        .iter()
        .filter_map(|leaf| {
            geometry
                .leaf_rect(*leaf)
                .map(|rect| axis.end(rect) - axis.extent(min_leaf(*leaf)))
        })
        .fold(axis.end(divider.parent), f32::min);
    Some((low.min(divider.position), high.max(divider.position)))
}

fn collect_anchors(node: &Node, geometry: &LayoutGeometry, out: &mut HashMap<SplitId, Anchor>) {
    let Node::Split(split) = node else {
        return;
    };
    if let Some(divider) = geometry.divider(split.id) {
        out.insert(
            split.id,
            Anchor {
                axis: split.axis,
                position: divider.position,
                parent: divider.parent,
                ratio: split.ratio,
                custom: split.manual,
            },
        );
    }
    collect_anchors(&split.first, geometry, out);
    collect_anchors(&split.second, geometry, out);
}

fn place(
    node: &Node,
    rect: Rect,
    plan: &DividerDragPlan,
    position: f32,
    aligned: bool,
    out: &mut Vec<(SplitId, f32, bool)>,
) {
    let Node::Split(split) = node else {
        return;
    };
    let Some(anchor) = plan.anchors.get(&split.id) else {
        return;
    };
    let moved = plan.moves(split.id, aligned);
    let at = if moved { position } else { anchor.position };
    let axis = split.axis;
    let span = axis.extent(rect.size());
    let ratio = if (!moved && rect == anchor.parent) || span <= 0.0 {
        anchor.ratio
    } else {
        ((at - axis.start(rect)) / span).clamp(0.0, 1.0)
    };
    out.push((split.id, ratio, anchor.custom));
    let (first, second) = axis.halves(rect, at);
    place(&split.first, first, plan, position, aligned, out);
    place(&split.second, second, plan, position, aligned, out);
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

/// `chain` carries the enclosing same-axis group's extent down the chain.
fn layout_node(
    node: &Node,
    rect: Rect,
    chain: Option<(Axis, (f32, f32))>,
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
            let position = if low <= high {
                (start + split.ratio * span).clamp(low, high)
            } else {
                let total = first_min + second_min;
                let share = if total > 0.0 { first_min / total } else { 0.5 };
                start + span * share
            };
            let group = match chain {
                Some((chain_axis, group)) if chain_axis == axis => group,
                _ => (start, end),
            };
            out.dividers.push(DividerGeometry {
                id: split.id,
                axis,
                parent: rect,
                position,
                group,
            });
            let (first, second) = axis.halves(rect, position);
            let chain = Some((axis, group));
            layout_node(&split.first, first, chain, min_leaf, out);
            layout_node(&split.second, second, chain, min_leaf, out);
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
        Vec2::splat(16.0)
    }

    const WINDOW: Rect = Rect::from_min_max(Pos2::ZERO, pos2(600.0, 200.0));

    /// A canonical same-axis group of leaves 1..; divider `k` has `SplitId(k + 1)`.
    fn group(axis: Axis, shares: &[f32], custom: &[bool]) -> Node {
        Group {
            axis,
            members: (1..=shares.len() as u64)
                .map(|leaf| Node::Leaf(id(leaf)))
                .collect(),
            shares: shares.iter().map(|share| share / 100.0).collect(),
            dividers: custom
                .iter()
                .enumerate()
                .map(|(index, custom)| (SplitId(index as u64 + 1), *custom))
                .collect(),
        }
        .build()
    }

    fn row(shares: &[f32], custom: &[bool]) -> Layout {
        Layout::from_root(Some(group(Axis::LeftRight, shares, custom)))
    }

    fn split(id: u64, axis: Axis, ratio: f32, manual: bool, first: Node, second: Node) -> Node {
        Node::Split(Box::new(Split {
            id: SplitId(id),
            axis,
            ratio,
            manual,
            first,
            second,
        }))
    }

    fn widths(layout: &Layout) -> Vec<(u64, f32)> {
        layout
            .geometry(WINDOW, &min)
            .leaves
            .iter()
            .map(|(leaf, rect)| (leaf.0, (rect.width() * 100.0).round() / 100.0))
            .collect()
    }

    fn position(layout: &Layout, split: u64) -> f32 {
        layout
            .geometry(WINDOW, &min)
            .divider(SplitId(split))
            .map(|divider| (divider.position * 100.0).round() / 100.0)
            .unwrap()
    }

    fn states(layout: &Layout) -> Vec<(u64, bool)> {
        layout
            .split_ids()
            .into_iter()
            .map(|split| (split.0, layout.split(split).unwrap().manual))
            .collect()
    }

    fn drag(layout: &mut Layout, split: u64, pointer: f32, snap: Snap, aligned: bool) -> f32 {
        let plan = layout.drag_plan(WINDOW, &min, SplitId(split), 1.0).unwrap();
        let at = plan.position(pointer, snap, aligned);
        let updates = layout.drag_updates(&plan, at, aligned);
        layout.set_splits(&updates).unwrap();
        at
    }

    #[test]
    fn split_equalizes_only_the_interval_between_custom_dividers() {
        // S1: A 50% ‖ B 50%, two Split Right on B.
        let mut layout = row(&[50.0, 50.0], &[true]);
        layout
            .insert_beside(id(2), Side::Right, id(3), SplitId(2))
            .unwrap();
        layout
            .insert_beside(id(3), Side::Right, id(4), SplitId(3))
            .unwrap();
        assert_eq!(
            widths(&layout),
            [(1, 300.0), (2, 100.0), (3, 100.0), (4, 100.0)]
        );
        assert_eq!(states(&layout), [(1, true), (2, false), (3, false)]);

        // S2: an all-Auto group equalizes entirely.
        let mut layout = row(&[50.0, 50.0], &[false]);
        layout
            .insert_beside(id(2), Side::Right, id(3), SplitId(2))
            .unwrap();
        assert_eq!(widths(&layout), [(1, 200.0), (2, 200.0), (3, 200.0)]);

        // S3: A 40% ‖ B 30% | C 30%, Split Right on A leaves B and C alone.
        let mut layout = row(&[40.0, 30.0, 30.0], &[true, false]);
        layout
            .insert_beside(id(1), Side::Right, id(4), SplitId(3))
            .unwrap();
        assert_eq!(
            widths(&layout),
            [(1, 120.0), (4, 120.0), (2, 180.0), (3, 180.0)]
        );
        assert_eq!(position(&layout, 1), 240.0);
    }

    #[test]
    fn perpendicular_split_halves_only_the_target() {
        let mut layout = row(&[25.0, 25.0, 50.0], &[false, false]);
        layout
            .insert_beside(id(2), Side::Below, id(4), SplitId(3))
            .unwrap();
        let geometry = layout.geometry(WINDOW, &min);
        assert_eq!(geometry.leaf_rect(id(2)).unwrap().height(), 100.0);
        assert_eq!(
            widths(&layout),
            [(1, 150.0), (2, 150.0), (4, 150.0), (3, 300.0)]
        );

        // [A | (B / C)]: B's group is the vertical one, so Split Right halves only B.
        let mut layout = Layout::from_root(Some(split(
            1,
            Axis::LeftRight,
            0.5,
            false,
            Node::Leaf(id(1)),
            split(
                2,
                Axis::TopBottom,
                0.5,
                false,
                Node::Leaf(id(2)),
                Node::Leaf(id(3)),
            ),
        )));
        layout
            .insert_beside(id(2), Side::Right, id(4), SplitId(3))
            .unwrap();
        assert_eq!(position(&layout, 1), 300.0);
        assert_eq!(position(&layout, 3), 450.0);
    }

    #[test]
    fn close_deletes_the_auto_or_trailing_divider_and_equalizes_the_receiving_interval() {
        // C1 and C2: A 50% ‖ B 20% ‖ C 15% | D 15%.
        let mut layout = row(&[50.0, 20.0, 15.0, 15.0], &[true, true, false]);
        layout.remove_leaf(id(3)).unwrap();
        assert_eq!(widths(&layout), [(1, 300.0), (2, 120.0), (4, 180.0)]);
        assert_eq!(states(&layout), [(1, true), (2, true)]);
        let mut layout = row(&[50.0, 20.0, 15.0, 15.0], &[true, true, false]);
        layout.remove_leaf(id(2)).unwrap();
        assert_eq!(widths(&layout), [(1, 300.0), (3, 150.0), (4, 150.0)]);
        assert_eq!(states(&layout), [(1, true), (3, false)]);

        // C3: the last member gives its space to the left.
        let mut layout = row(&[40.0, 20.0, 20.0, 20.0], &[true, true, true]);
        layout.remove_leaf(id(4)).unwrap();
        assert_eq!(widths(&layout), [(1, 240.0), (2, 120.0), (3, 240.0)]);

        // C4: the first member gives its space to the right.
        let mut layout = row(&[25.0, 25.0, 50.0], &[true, false]);
        layout.remove_leaf(id(1)).unwrap();
        assert_eq!(widths(&layout), [(2, 300.0), (3, 300.0)]);
        assert_eq!(states(&layout), [(2, false)]);
    }

    #[test]
    fn close_result_does_not_depend_on_the_tree_shape() {
        // (A (B C)) and ((A B) C) with A|B = split 1 and B|C = split 2 (C5–C8).
        let nested_first = |shares: [f32; 3], custom: [bool; 2]| {
            let [a, b, _] = shares.map(|share| share / 100.0);
            Layout::from_root(Some(split(
                2,
                Axis::LeftRight,
                a + b,
                custom[1],
                split(
                    1,
                    Axis::LeftRight,
                    a / (a + b),
                    custom[0],
                    Node::Leaf(id(1)),
                    Node::Leaf(id(2)),
                ),
                Node::Leaf(id(3)),
            )))
        };
        // Shares, Custom states, widths after closing B, surviving divider.
        type Case = ([f32; 3], [bool; 2], [(u64, f32); 2], (u64, bool));
        let cases: [Case; 4] = [
            (
                [100.0 / 3.0; 3],
                [false, false],
                [(1, 300.0), (3, 300.0)],
                (1, false),
            ),
            (
                [30.0, 30.0, 40.0],
                [true, false],
                [(1, 180.0), (3, 420.0)],
                (1, true),
            ),
            (
                [30.0, 30.0, 40.0],
                [false, true],
                [(1, 360.0), (3, 240.0)],
                (2, true),
            ),
            (
                [30.0, 30.0, 40.0],
                [true, true],
                [(1, 180.0), (3, 420.0)],
                (1, true),
            ),
        ];
        for (shares, custom, expected, survivor) in cases {
            for mut layout in [row(&shares, &custom), nested_first(shares, custom)] {
                layout.remove_leaf(id(2)).unwrap();
                assert_eq!(widths(&layout), expected, "{shares:?} {custom:?}");
                assert_eq!(states(&layout), [survivor], "{shares:?} {custom:?}");
            }
        }
    }

    #[test]
    fn a_collapsed_group_joins_the_outer_group_unchanged() {
        // A ‖ [P ‖ Q / Y]: closing Y leaves P and Q in A's row with their states.
        let mut layout = Layout::from_root(Some(split(
            1,
            Axis::LeftRight,
            0.25,
            true,
            Node::Leaf(id(1)),
            split(
                2,
                Axis::TopBottom,
                0.5,
                false,
                split(
                    3,
                    Axis::LeftRight,
                    0.5,
                    true,
                    Node::Leaf(id(2)),
                    Node::Leaf(id(3)),
                ),
                Node::Leaf(id(4)),
            ),
        )));
        layout.remove_leaf(id(4)).unwrap();
        assert_eq!(widths(&layout), [(1, 150.0), (2, 225.0), (3, 225.0)]);
        assert_eq!(states(&layout), [(1, true), (3, true)]);
        let geometry = layout.geometry(WINDOW, &min);
        assert_eq!(geometry.leaf_rect(id(3)).unwrap().height(), 200.0);
    }

    #[test]
    fn reset_equalizes_the_group_and_scales_nested_layouts() {
        // R1: A 50% ‖ B 30% | C 20%, where C holds a Custom [C1 ‖ C2 / C3] at its middle.
        let nested = split(
            3,
            Axis::TopBottom,
            0.5,
            false,
            split(
                4,
                Axis::LeftRight,
                0.5,
                true,
                Node::Leaf(id(3)),
                Node::Leaf(id(5)),
            ),
            Node::Leaf(id(6)),
        );
        let mut layout = Layout::from_root(Some(split(
            1,
            Axis::LeftRight,
            0.5,
            true,
            Node::Leaf(id(1)),
            split(2, Axis::LeftRight, 0.6, false, Node::Leaf(id(2)), nested),
        )));
        layout.reset_group(SplitId(2)).unwrap();
        assert_eq!(position(&layout, 1), 200.0);
        assert_eq!(position(&layout, 2), 400.0);
        assert_eq!(position(&layout, 4), 500.0);
        assert_eq!(
            states(&layout),
            [(1, false), (2, false), (3, false), (4, true)]
        );
        assert_eq!(
            layout.reset_group(SplitId(9)),
            Err(LayoutError::MissingSplit(SplitId(9)))
        );
    }

    #[test]
    fn reassigned_members_scale_their_inner_layout() {
        // P1: A | B, B holds [B1 ‖ B2 / B3] at its middle; Split Right on A.
        let mut layout = Layout::from_root(Some(split(
            1,
            Axis::LeftRight,
            0.5,
            false,
            Node::Leaf(id(1)),
            split(
                2,
                Axis::TopBottom,
                0.5,
                false,
                split(
                    3,
                    Axis::LeftRight,
                    0.5,
                    true,
                    Node::Leaf(id(2)),
                    Node::Leaf(id(3)),
                ),
                Node::Leaf(id(4)),
            ),
        )));
        assert_eq!(position(&layout, 3), 450.0);
        layout
            .insert_beside(id(1), Side::Right, id(5), SplitId(4))
            .unwrap();
        assert_eq!(position(&layout, 4), 200.0);
        assert_eq!(position(&layout, 1), 400.0);
        assert_eq!(position(&layout, 3), 500.0);
        assert!(layout.split(SplitId(3)).unwrap().manual);
    }

    #[test]
    fn drag_keeps_every_other_divider_and_stops_at_touching_minimums() {
        // D1: A | [B | C / D] with A| at 300 and B|C at 450.
        let nested = || {
            Layout::from_root(Some(split(
                1,
                Axis::LeftRight,
                0.5,
                false,
                Node::Leaf(id(1)),
                split(
                    2,
                    Axis::TopBottom,
                    0.5,
                    false,
                    split(
                        3,
                        Axis::LeftRight,
                        0.5,
                        false,
                        Node::Leaf(id(2)),
                        Node::Leaf(id(3)),
                    ),
                    Node::Leaf(id(4)),
                ),
            )))
        };
        let mut layout = nested();
        assert_eq!(drag(&mut layout, 1, 200.0, Snap::Off, false), 200.0);
        assert_eq!(position(&layout, 3), 450.0);
        assert_eq!(states(&layout), [(1, false), (2, false), (3, false)]);
        let mut layout = nested();
        assert_eq!(drag(&mut layout, 1, 590.0, Snap::Off, false), 434.0);

        // D2 and D3: A | B | C at 200 and 400.
        let mut layout = row(&[100.0 / 3.0; 3], &[false, false]);
        assert_eq!(drag(&mut layout, 1, 500.0, Snap::Off, false), 384.0);
        assert_eq!(position(&layout, 2), 400.0);
        let mut layout = row(&[100.0 / 3.0; 3], &[false, false]);
        assert_eq!(drag(&mut layout, 2, 100.0, Snap::Off, false), 216.0);
        assert_eq!(position(&layout, 1), 200.0);
    }

    #[test]
    fn drag_starts_from_the_rendered_divider_and_keeps_clamped_ratios_elsewhere() {
        // A wants 12 points but renders at its 16-point minimum.
        let mut layout = row(&[2.0, 49.0, 49.0], &[true, false]);
        let plan = layout.drag_plan(WINDOW, &min, SplitId(1), 1.0).unwrap();
        assert_eq!(plan.start(), 16.0);
        assert_eq!(plan.position(5.0, Snap::Off, false), 16.0);
        assert_eq!(drag(&mut layout, 2, 400.0, Snap::Off, false), 400.0);
        assert!((layout.split(SplitId(1)).unwrap().ratio - 0.02).abs() < 1e-6);
        assert_eq!(position(&layout, 1), 16.0);
    }

    #[test]
    fn shift_drag_moves_aligned_dividers_within_their_common_range() {
        // D4/D5: [A | B] over [C | (D | E)], both rows split at 300, D|E at 350.
        let mut layout = Layout::from_root(Some(split(
            1,
            Axis::TopBottom,
            0.5,
            false,
            split(
                2,
                Axis::LeftRight,
                0.5,
                false,
                Node::Leaf(id(1)),
                Node::Leaf(id(2)),
            ),
            split(
                3,
                Axis::LeftRight,
                0.5,
                false,
                Node::Leaf(id(3)),
                split(
                    4,
                    Axis::LeftRight,
                    1.0 / 6.0,
                    false,
                    Node::Leaf(id(4)),
                    Node::Leaf(id(5)),
                ),
            ),
        )));
        let plan = layout.drag_plan(WINDOW, &min, SplitId(2), 1.0).unwrap();
        assert_eq!(plan.aligned(), [SplitId(3)]);
        assert_eq!(plan.position(1000.0, Snap::Off, false), 584.0);
        assert_eq!(plan.position(1000.0, Snap::Off, true), 334.0);

        assert_eq!(drag(&mut layout, 2, 200.0, Snap::Off, false), 200.0);
        assert_eq!(position(&layout, 3), 300.0);
        let plan = layout.drag_plan(WINDOW, &min, SplitId(2), 1.0).unwrap();
        assert!(plan.aligned().is_empty());

        let mut layout_shift = layout.clone();
        layout_shift
            .set_splits(&[(SplitId(2), 0.5, false)])
            .unwrap();
        let plan = layout_shift
            .drag_plan(WINDOW, &min, SplitId(2), 1.0)
            .unwrap();
        layout_shift
            .set_splits(&layout_shift.drag_updates(&plan, 200.0, true))
            .unwrap();
        assert_eq!(position(&layout_shift, 2), 200.0);
        assert_eq!(position(&layout_shift, 3), 200.0);
        assert_eq!(position(&layout_shift, 4), 350.0);
        // Releasing Shift mid-drag returns the aligned divider to its start.
        layout_shift
            .set_splits(&layout_shift.drag_updates(&plan, 250.0, false))
            .unwrap();
        assert_eq!(position(&layout_shift, 3), 300.0);
        assert_eq!(position(&layout_shift, 4), 350.0);
        layout_shift.set_splits(&plan.original()).unwrap();
        assert_eq!(position(&layout_shift, 2), 300.0);
    }

    #[test]
    fn snapping_uses_the_group_start_fractions_and_parallel_dividers() {
        // A | B | C at 200 and 400: B|C's grid starts at the group, not at A|B.
        let layout = row(&[100.0 / 3.0; 3], &[false, false]);
        let plan = layout.drag_plan(WINDOW, &min, SplitId(2), 1.0).unwrap();
        assert_eq!(plan.position(470.0, Snap::Grid, false), 464.0);
        assert_eq!(plan.position(1000.0, Snap::Grid, false), 584.0);
        assert_eq!(plan.position(470.0, Snap::Off, false), 470.0);
        assert_eq!(plan.position(430.0, Snap::Fractions, false), 450.0);
        // 400 (2/3) and 450 (3/4) are equally near: the smaller wins.
        assert_eq!(plan.position(425.0, Snap::Fractions, false), 400.0);

        // Ctrl also offers the other row's divider at 330.
        let layout = Layout::from_root(Some(split(
            1,
            Axis::TopBottom,
            0.5,
            false,
            split(
                2,
                Axis::LeftRight,
                0.5,
                false,
                Node::Leaf(id(1)),
                Node::Leaf(id(2)),
            ),
            split(
                3,
                Axis::LeftRight,
                0.55,
                false,
                Node::Leaf(id(3)),
                Node::Leaf(id(4)),
            ),
        )));
        let plan = layout.drag_plan(WINDOW, &min, SplitId(2), 1.0).unwrap();
        assert_eq!(plan.position(335.0, Snap::Fractions, false), 330.0);
    }

    #[test]
    fn insertion_needs_room_in_the_receiving_interval() {
        // A 560 ‖ B 20 | C 20: B's interval is 40 wide, A's is 560.
        let layout = row(&[560.0 / 6.0, 20.0 / 6.0, 20.0 / 6.0], &[true, false]);
        let geometry = layout.geometry(WINDOW, &min);
        let fits =
            |target, side| layout.insertion_fits(&geometry, id(target), side, min(id(9)), &min);
        assert!(!fits(2, Side::Right));
        assert!(fits(1, Side::Right));
        assert!(fits(2, Side::Below));
    }

    #[test]
    fn set_splits_changes_nothing_when_an_id_is_missing() {
        let mut layout = row(&[50.0, 50.0], &[false]);
        assert_eq!(
            layout.set_splits(&[(SplitId(1), 0.2, true), (SplitId(7), 0.5, false)]),
            Err(LayoutError::MissingSplit(SplitId(7)))
        );
        assert_eq!(states(&layout), [(1, false)]);
        assert_eq!(position(&layout, 1), 300.0);
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
        assert_eq!(
            layout.remove_leaf(id(9)),
            Err(LayoutError::MissingLeaf(id(9)))
        );
        layout.remove_leaf(id(2)).unwrap();
        layout.remove_leaf(id(3)).unwrap();
        layout.remove_leaf(id(1)).unwrap();
        assert!(layout.is_empty());
    }
}
