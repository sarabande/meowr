use super::*;

fn tree() -> Tree {
    let mut tree = Tree::new(BufferId(0));
    tree.layout(Area {
        x: 0,
        y: 0,
        width: 80,
        height: 24,
    });
    tree
}

/// Every cell of `area` is claimed by exactly one window.
///
/// This is the property that matters, and the one a list of rectangles would
/// let you break: tiling means partitioning, not merely "not obviously wrong".
fn tiles_exactly(tree: &Tree, area: Area) -> bool {
    let mut covered = vec![0u32; (area.width as usize) * (area.height as usize)];
    for (_, window) in tree.windows() {
        for y in window.area.y..window.area.y + window.area.height {
            for x in window.area.x..window.area.x + window.area.width {
                covered[y as usize * area.width as usize + x as usize] += 1;
            }
        }
    }
    covered.iter().all(|n| *n == 1)
}

const FRAME: Area = Area {
    x: 0,
    y: 0,
    width: 80,
    height: 24,
};

#[test]
fn one_window_fills_the_frame() {
    let tree = tree();
    assert_eq!(tree.len(), 1);
    assert_eq!(tree.current().area, FRAME);
}

#[test]
fn splitting_both_ways_tiles_without_overlapping() {
    let mut tree = tree();
    tree.split(Direction::Vertical);
    tree.layout(FRAME);
    assert!(tiles_exactly(&tree, FRAME), "after a vertical split");

    tree.split(Direction::Horizontal);
    tree.layout(FRAME);
    assert!(
        tiles_exactly(&tree, FRAME),
        "and a horizontal one inside it"
    );

    tree.split(Direction::Vertical);
    tree.layout(FRAME);
    assert!(tiles_exactly(&tree, FRAME), "and another");
    assert_eq!(tree.order().len(), 4);
}

#[test]
fn an_odd_frame_gives_the_remainder_to_the_last_window() {
    // 24 rows over five windows is 4,4,4,4,8 — not 4,4,4,4,4 with four rows
    // nobody draws.
    let mut tree = tree();
    for _ in 0..4 {
        tree.set_focus(tree.order()[0]);
        tree.split(Direction::Vertical);
    }
    tree.layout(FRAME);
    assert!(tiles_exactly(&tree, FRAME));
    let total: u16 = tree.windows().map(|(_, w)| w.area.height).sum();
    assert_eq!(total, FRAME.height);
}

#[test]
fn a_split_in_the_same_direction_joins_rather_than_nests() {
    // Three horizontal splits give three equal columns, not a half and two
    // quarters. Emacs does this, and anyone who has used it expects it.
    let mut tree = tree();
    tree.split(Direction::Horizontal);
    tree.split(Direction::Horizontal);
    tree.layout(FRAME);

    let widths: Vec<u16> = tree.windows().map(|(_, w)| w.area.width).collect();
    assert_eq!(widths.len(), 3);
    assert!(
        widths.iter().all(|w| (*w as i32 - 26).abs() <= 2),
        "uneven columns: {widths:?}"
    );
}

#[test]
fn a_new_window_shows_the_same_buffer_at_the_same_place() {
    // Which is what makes splitting useful for looking at two parts of one
    // file.
    let mut tree = tree();
    tree.current_mut().top = 42;
    let new = tree.split(Direction::Vertical);

    assert_eq!(tree.window(new).buffer, BufferId(0));
    assert_eq!(tree.window(new).top, 42);
    assert_eq!(tree.focus(), new, "and the focus follows the split");
}

#[test]
fn each_window_scrolls_independently() {
    let mut tree = tree();
    let first = tree.focus();
    let second = tree.split(Direction::Vertical);

    tree.window_mut(first).top = 10;
    tree.window_mut(second).top = 90;

    assert_eq!(tree.window(first).top, 10);
    assert_eq!(tree.window(second).top, 90);
}

#[test]
fn moving_between_windows_wraps() {
    let mut tree = tree();
    tree.split(Direction::Vertical);
    tree.split(Direction::Horizontal);
    let order = tree.order();

    tree.set_focus(order[0]);
    for expected in order.iter().skip(1).chain(order.first()) {
        tree.next_window();
        assert_eq!(tree.focus(), *expected);
    }
}

#[test]
fn closing_a_window_keeps_the_rest_tiling() {
    let mut tree = tree();
    tree.split(Direction::Vertical);
    tree.split(Direction::Horizontal);
    assert_eq!(tree.order().len(), 3);

    assert!(tree.close());
    tree.layout(FRAME);

    assert_eq!(tree.order().len(), 2);
    assert!(tiles_exactly(&tree, FRAME), "no gap where it used to be");
}

#[test]
fn the_last_window_cannot_be_closed() {
    let mut tree = tree();
    assert!(!tree.close());
    assert_eq!(tree.order().len(), 1);
}

#[test]
fn closing_all_but_one_leaves_the_focused_window_filling_the_frame() {
    let mut tree = tree();
    tree.split(Direction::Vertical);
    tree.split(Direction::Horizontal);
    let kept = tree.focus();

    tree.close_others();
    tree.layout(FRAME);

    assert_eq!(tree.order(), vec![kept]);
    assert_eq!(tree.current().area, FRAME);
}

#[test]
fn window_ids_keep_meaning_the_same_thing_after_a_close() {
    // The slot is kept rather than removed. Ids that mean different things at
    // different times are the bug that only appears with four windows open and
    // one of them closed.
    let mut tree = tree();
    let first = tree.focus();
    let second = tree.split(Direction::Vertical);
    let third = tree.split(Direction::Vertical);
    tree.window_mut(third).top = 7;

    tree.set_focus(second);
    tree.close();

    assert_eq!(tree.window(third).top, 7, "still the same window");
    assert!(tree.order().contains(&first) && tree.order().contains(&third));
}

#[test]
fn resizing_redistributes_every_window() {
    let mut tree = tree();
    tree.split(Direction::Horizontal);
    tree.layout(FRAME);

    let narrow = Area { width: 40, ..FRAME };
    tree.layout(narrow);

    assert!(tiles_exactly(&tree, narrow), "columns narrow, not clip");
    let widths: Vec<u16> = tree.windows().map(|(_, w)| w.area.width).collect();
    assert_eq!(widths, vec![20, 20]);
}
