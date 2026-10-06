//! The window tree: how the frame is divided, and what each division shows.
//!
//! Windows **tile** — they partition the frame exactly, never overlap, and
//! there is nothing floating above them. That is ADR-0004, and it is the
//! reason this is a tree of splits rather than a list of rectangles: a list
//! would let two windows claim the same cell, and nothing in the type would
//! stop it.

use crate::buffer::BufferId;

/// Which window, in the tree's flattened order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WindowId(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Children stacked top to bottom — `C-x 2`.
    Vertical,
    /// Children side by side — `C-x 3`.
    Horizontal,
}

/// One pane: a buffer, and where in it we are looking.
#[derive(Debug, Clone)]
pub struct Window {
    pub buffer: BufferId,
    /// Where this window was looking when the focus left it.
    ///
    /// Emacs keeps point per window and lets the buffer hold one for when it
    /// is newly displayed. meowr keeps the live one on the buffer and parks a
    /// copy here on the way out, which is the same arrangement seen from the
    /// other side — and does not put a window's identity in front of the forty
    /// or so places that reach for `buffer.selection`.
    /// The buffer it belongs to, because a window that has been pointed at
    /// something else since holds an offset into a different text.
    pub point: Option<(BufferId, meowr_core::SelectionState)>,
    /// The first visible line. Each window scrolls independently, which is the
    /// whole point of showing one buffer twice.
    pub top: usize,
    /// Filled in by [`Tree::layout`]; meaningless until then.
    pub area: Area,
}

/// A rectangle of the frame, in cells.
///
/// Defined in `meowr-view` and re-exported here, where the tree that computes
/// one is. It went down a layer with `Screen`: a plain-data model of the frame
/// cannot sit below the type that says where a Window is, and this crate is
/// above the view layer rather than beneath it.
pub use meowr_view::Area;

/// A window's area less the row its modeline occupies.
///
/// The modeline is not an overlay — ADR-0004 — so it takes a row from the
/// window it belongs to. Every viewport calculation has to know that, or the
/// last line of the buffer hides underneath it.
pub fn text_area(area: Area) -> Area {
    Area {
        height: area.height.saturating_sub(1),
        ..area
    }
}

#[derive(Debug, Clone)]
enum Node {
    Leaf(WindowId),
    Split {
        direction: Direction,
        children: Vec<Node>,
    },
}

/// The frame's division, and the windows in it.
#[derive(Debug, Clone)]
pub struct Tree {
    root: Node,
    windows: Vec<Window>,
    focus: WindowId,
}

impl Tree {
    /// One window, showing `buffer`, filling the frame.
    pub fn new(buffer: BufferId) -> Self {
        Self {
            root: Node::Leaf(WindowId(0)),
            windows: vec![Window {
                buffer,
                point: None,
                top: 0,
                area: Area::default(),
            }],
            focus: WindowId(0),
        }
    }

    pub fn focus(&self) -> WindowId {
        self.focus
    }

    pub fn len(&self) -> usize {
        self.windows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    pub fn window(&self, id: WindowId) -> &Window {
        &self.windows[id.0]
    }

    pub fn window_mut(&mut self, id: WindowId) -> &mut Window {
        &mut self.windows[id.0]
    }

    pub fn current(&self) -> &Window {
        self.window(self.focus)
    }

    pub fn current_mut(&mut self) -> &mut Window {
        let focus = self.focus;
        self.window_mut(focus)
    }

    /// Every window, in the order the tree lays them out — which is the order
    /// `other-window` walks.
    pub fn order(&self) -> Vec<WindowId> {
        let mut out = Vec::new();
        collect(&self.root, &mut out);
        out
    }

    pub fn windows(&self) -> impl Iterator<Item = (WindowId, &Window)> {
        self.order().into_iter().map(|id| (id, self.window(id)))
    }

    /// Split the focused window, and focus the new half.
    ///
    /// The new window shows the same buffer at the same scroll position, which
    /// is what makes `C-x 2` useful for looking at two parts of one file.
    pub fn split(&mut self, direction: Direction) -> WindowId {
        let existing = self.current().clone();
        let id = WindowId(self.windows.len());
        self.windows.push(existing);
        split_at(&mut self.root, self.focus, direction, id);
        self.focus = id;
        id
    }

    /// Move the focus on by one, wrapping — Emacs's `other-window`.
    pub fn next_window(&mut self) {
        let order = self.order();
        if let Some(at) = order.iter().position(|id| *id == self.focus) {
            self.focus = order[(at + 1) % order.len()];
        }
    }

    pub fn set_focus(&mut self, id: WindowId) {
        if id.0 < self.windows.len() {
            self.focus = id;
        }
    }

    /// Close the focused window. The last one cannot be closed.
    ///
    /// The window's slot is kept rather than removed, so that every other
    /// `WindowId` still means what it did. Ids that mean different things at
    /// different times are the kind of bug that only shows up with four
    /// windows open and one of them closed.
    pub fn close(&mut self) -> bool {
        if self.order().len() <= 1 {
            return false;
        }
        let closing = self.focus;
        remove(&mut self.root, closing);
        let order = self.order();
        self.focus = order[0];
        true
    }

    /// Close every window but the focused one — `C-x 1`.
    pub fn close_others(&mut self) {
        self.root = Node::Leaf(self.focus);
    }

    /// Assign a rectangle to every window.
    ///
    /// Splits divide their area evenly, and the remainder goes to the last
    /// child rather than being lost: three windows in eighty columns are
    /// 26, 26 and 28, not 26, 26, 26 with two columns nobody draws.
    pub fn layout(&mut self, area: Area) {
        let mut assignments = Vec::new();
        assign(&self.root, area, &mut assignments);
        for (id, area) in assignments {
            self.windows[id.0].area = area;
        }
    }
}

fn collect(node: &Node, out: &mut Vec<WindowId>) {
    match node {
        Node::Leaf(id) => out.push(*id),
        Node::Split { children, .. } => children.iter().for_each(|c| collect(c, out)),
    }
}

fn split_at(node: &mut Node, target: WindowId, direction: Direction, new: WindowId) -> bool {
    match node {
        Node::Leaf(id) if *id == target => {
            *node = Node::Split {
                direction,
                children: vec![Node::Leaf(target), Node::Leaf(new)],
            };
            true
        }
        Node::Leaf(_) => false,
        Node::Split {
            direction: existing,
            children,
        } => {
            // A split in the same direction joins the existing one rather than
            // nesting, so three horizontal splits give three equal columns and
            // not a half and two quarters — which is what Emacs does and what
            // anyone expects.
            let same = *existing == direction;
            for index in 0..children.len() {
                if same && matches!(children[index], Node::Leaf(id) if id == target) {
                    children.insert(index + 1, Node::Leaf(new));
                    return true;
                }
                if split_at(&mut children[index], target, direction, new) {
                    return true;
                }
            }
            false
        }
    }
}

fn remove(node: &mut Node, target: WindowId) {
    if let Node::Split { children, .. } = node {
        children.retain(|child| !matches!(child, Node::Leaf(id) if *id == target));
        for child in children.iter_mut() {
            remove(child, target);
        }
        // A split with one child left is not a split any more.
        if children.len() == 1 {
            *node = children.remove(0);
        }
    }
}

fn assign(node: &Node, area: Area, out: &mut Vec<(WindowId, Area)>) {
    match node {
        Node::Leaf(id) => out.push((*id, area)),
        Node::Split {
            direction,
            children,
        } => {
            let count = children.len().max(1) as u16;
            for (index, child) in children.iter().enumerate() {
                let last = index + 1 == children.len();
                let child_area = match direction {
                    Direction::Vertical => {
                        let each = area.height / count;
                        Area {
                            y: area.y + each * index as u16,
                            height: if last {
                                area.height - each * (count - 1)
                            } else {
                                each
                            },
                            ..area
                        }
                    }
                    Direction::Horizontal => {
                        let each = area.width / count;
                        Area {
                            x: area.x + each * index as u16,
                            width: if last {
                                area.width - each * (count - 1)
                            } else {
                                each
                            },
                            ..area
                        }
                    }
                };
                assign(child, child_area, out);
            }
        }
    }
}

#[cfg(test)]
mod tests;
