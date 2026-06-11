pub enum PanelAction {
    None, 
    SplitRight(usize), 
    SplitDown(usize), 
    Remove(usize) 
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    H, V
}
