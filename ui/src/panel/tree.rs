use std::collections::VecDeque;
use super::action::Dir;
use super::node::Node;

static SPLIT_ID_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
fn next_split_id() -> u64 {
    SPLIT_ID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

pub fn do_split(node: Node, target: usize, counter: &mut usize, dir: Dir) -> (Node, bool) {
    match node {
        Node::Leaf(panel) => {
            let id = *counter;
            *counter += 1;
            if id != target { return (Node::Leaf(panel), false); }
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
            let a_count = a.leaf_count();
            if *counter + a_count > target {
                let (new_a, hit) = do_split(*a, target, counter, dir);
                let mut new_node = Node::Split { id, dir: sd, ratio, dragged, a: Box::new(new_a), b };
                if hit && sd == dir && is_chain_not_dragged(&new_node, dir) {
                    new_node = rebalance_chain(new_node, dir);
                }
                (new_node, hit)
            } else {
                *counter += a_count;
                let (new_b, hit) = do_split(*b, target, counter, dir);
                let mut new_node = Node::Split { id, dir: sd, ratio, dragged, a, b: Box::new(new_b) };
                if hit && sd == dir && is_chain_not_dragged(&new_node, dir) {
                    new_node = rebalance_chain(new_node, dir);
                }
                (new_node, hit)
            }
        }
    }
}

pub fn do_remove(node: Node, target: usize, counter: &mut usize) -> (Node, bool) {
    match node {
        Node::Leaf(panel) => {
            let id = *counter;
            *counter += 1;
            (Node::Leaf(panel), id == target)
        }
        Node::Split { id, dir, ratio, dragged, a, b } => {
            let a_count = a.leaf_count();

            if *counter + a_count > target {
                let (new_a, hit) = do_remove(*a, target, counter);
                if hit {
                    let mut b = *b;
                    if is_chain_not_dragged(&b, dir) {
                        b = rebalance_chain(b, dir);
                    } 
                    return (b, false);
                }
                let mut new_node = Node::Split { id, dir, ratio, dragged, a: Box::new(new_a), b };
                if is_chain_not_dragged(&new_node, dir) {
                    new_node = rebalance_chain(new_node, dir);
                }
                (new_node, false)
            } else {
                *counter += a_count;
                let (new_b, hit) = do_remove(*b, target, counter);
                if hit {
                    let mut a = *a;
                    if is_chain_not_dragged(&a, dir) {
                        a = rebalance_chain(a, dir);
                    } 
                    return (a, false);
                }
                let mut new_node = Node::Split { id, dir, ratio, dragged, a, b: Box::new(new_b) };
                if is_chain_not_dragged(&new_node, dir) {
                    new_node = rebalance_chain(new_node, dir);
                }
                (new_node, false)
            }
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

