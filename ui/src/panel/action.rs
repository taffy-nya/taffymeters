pub enum PanelAction { None, SplitRight, SplitDown, Remove }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    H, V
}
