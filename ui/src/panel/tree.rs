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
                a: Box::new(Node::Leaf(panel)),
                b: Box::new(Node::leaf(vt)),
            };
            (new_node, true)
        }
        Node::Split { id, dir: sd, ratio, a, b } => {
            let a_count = a.leaf_count();
            if *counter + a_count > target {
                let (new_a, hit) = do_split(*a, target, counter, dir);
                let mut node = Node::Split { id, dir: sd, ratio, a: Box::new(new_a), b };
                if hit && sd == dir {
                    rebalance_chain(&mut node, dir);
                }
                (node, hit)
            } else {
                *counter += a_count;
                let (new_b, hit) = do_split(*b, target, counter, dir);
                let mut node = Node::Split { id, dir: sd, ratio, a, b: Box::new(new_b) };
                if hit && sd == dir {
                    rebalance_chain(&mut node, dir);
                }
                (node, hit)
            }
        }
    }
}

fn rebalance_chain(node: &mut Node, dir: Dir) {
    let total = count_chain_leaves(node, dir);
    apply_chain_ratios(node, dir, total);
}

fn count_chain_leaves(node: &Node, dir: Dir) -> usize {
    match node {
        Node::Leaf(_) => 1,
        Node::Split { dir: sd, a, b, .. } if *sd == dir => {
            count_chain_leaves(a, dir) + count_chain_leaves(b, dir)
        }
        Node::Split { .. } => 1,
    }
}

fn apply_chain_ratios(node: &mut Node, dir: Dir, remaining: usize) {
    if let Node::Split { dir: sd, ratio, b, .. } = node
        && *sd == dir
        && remaining > 1
    {
        *ratio = 1.0 / remaining as f32;
        apply_chain_ratios(b, dir, remaining - 1);
    }
}

pub fn do_remove(node: Node, target: usize, counter: &mut usize) -> (Node, bool) {
    match node {
        Node::Leaf(panel) => {
            let id = *counter;
            *counter += 1;
            (Node::Leaf(panel), id == target)
        }
        Node::Split { id, dir, ratio, a, b } => {
            let a_count = a.leaf_count();

            if *counter + a_count > target {
                let (new_a, hit) = do_remove(*a, target, counter);
                if hit {
                    return (*b, false);
                }
                (Node::Split { id, dir, ratio, a: Box::new(new_a), b }, false)
            } else {
                *counter += a_count;
                let (new_b, hit) = do_remove(*b, target, counter);
                if hit {
                    return (*a, false);
                }
                (Node::Split { id, dir, ratio, a, b: Box::new(new_b) }, false)
            }
        }
    }
}
