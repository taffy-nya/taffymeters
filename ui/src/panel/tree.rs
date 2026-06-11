use std::collections::VecDeque;
use super::action::Dir;
use super::node::Node;

static SPLIT_ID_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
fn next_split_id() -> usize {
    SPLIT_ID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

pub fn do_split(node: Node, target_id: usize, dir: Dir) -> (Node, bool) {
    match node {
        Node::Leaf(panel) => {
            if panel.id != target_id { return (Node::Leaf(panel), false); }
            let vt = panel.view_type;
            let new_node = Node::Split {
                id: next_split_id(),
                dir,
                ratio: 0.5,
                dragged: false,
                a: Box::new(Node::Leaf(panel)),
                b: Box::new(Node::leaf(vt)),
            };
            (new_node, true)
        }
        Node::Split { id, dir: sd, ratio, dragged, a, b } => {
            let (new_a, hit_a) = do_split(*a, target_id, dir);
            if hit_a {
                let mut new_node = Node::Split { id, dir: sd, ratio, dragged, a: Box::new(new_a), b };
                if sd == dir && is_chain_not_dragged(&new_node, dir) {
                    new_node = rebalance_chain(new_node, dir);
                }
                return (new_node, true);
            }
            let (new_b, hit_b) = do_split(*b, target_id, dir);
            if hit_b {
                let mut new_node = Node::Split { id, dir: sd, ratio, dragged, a: Box::new(new_a), b: Box::new(new_b) };
                if sd == dir && is_chain_not_dragged(&new_node, dir) {
                    new_node = rebalance_chain(new_node, dir);
                }
                return (new_node, true);
            }
            let new_node = Node::Split { id, dir: sd, ratio, dragged, a: Box::new(new_a), b: Box::new(new_b) };
            (new_node, false)
        }
    }
}

pub fn do_remove(node: Node, target_id: usize) -> (Node, bool) {
    match node {
        Node::Leaf(panel) => {
            let hit = panel.id == target_id;
            (Node::Leaf(panel), hit)
        }
        Node::Split { id, dir, ratio, dragged, a, b } => {
            let (new_a, hit_a) = do_remove(*a, target_id);
            if hit_a {
                let mut b = *b;
                if is_chain_not_dragged(&b, dir) {
                    b = rebalance_chain(b, dir);
                }
                return (b, false);
            }
            let (new_b, hit_b) = do_remove(*b, target_id);
            if hit_b {
                let mut a = new_a;
                if is_chain_not_dragged(&a, dir) {
                    a = rebalance_chain(a, dir);
                }
                return (a, false);
            }
            let mut new_node = Node::Split { id, dir, ratio, dragged, a: Box::new(new_a), b: Box::new(new_b) };
            if is_chain_not_dragged(&new_node, dir) {
                new_node = rebalance_chain(new_node, dir);
            }
            (new_node, false)
        }
    }
}

fn is_chain_not_dragged(node: &Node, dir: Dir) -> bool {
    match node {
        Node::Split { dir: sd, dragged, a, b, .. } if *sd == dir => {
            !*dragged && is_chain_not_dragged(a, dir) && is_chain_not_dragged(b, dir)
        }
        _ => true,
    }
}

fn rebalance_chain(node: Node, dir: Dir) -> Node {
    let mut nodes = VecDeque::new();
    collect_chain_nodes(node, dir, &mut nodes);
    rebuild_chain(nodes, dir)
}

fn collect_chain_nodes(node: Node, dir: Dir, nodes: &mut VecDeque<Node>) {
    match node {
        Node::Split { dir: sd, a, b, .. } if sd == dir => {
            collect_chain_nodes(*a, dir, nodes);
            collect_chain_nodes(*b, dir, nodes);
        }
        other => nodes.push_back(other),
    }
}

fn rebuild_chain(mut nodes: VecDeque<Node>, dir: Dir) -> Node {
    if nodes.len() == 1 { return nodes.pop_front().unwrap(); }
    let a = nodes.pop_front().unwrap();
    let remaining = nodes.len();
    let ratio = 1.0 / (remaining + 1) as f32;
    let b = rebuild_chain(nodes, dir);
    Node::Split { id: next_split_id(), dir, ratio, dragged: false, a: Box::new(a), b: Box::new(b) }
}
