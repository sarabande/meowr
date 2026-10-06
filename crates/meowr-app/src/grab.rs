//! The Grab: the secondary selection.
//!
//! The Grab is a *region*, and the Selection is what the commands act on. They
//! are deliberately different things — the whole point of the Grab is to hold
//! one place while the Selection moves somewhere else, so anything that
//! conflated them would take away the only reason it exists.

use super::*;

impl Session {
    pub fn grab(&self) -> Option<Range<usize>> {
        self.current().grab()
    }

    /// `meow-grab`: the selection becomes the Grab, and the selection goes.
    ///
    /// With no selection this *clears* the Grab rather than grabbing nothing,
    /// which is `meow--cancel-second-selection` — so `G` with nothing selected
    /// is how you put the Grab away.
    pub(crate) fn grab_command(&mut self) -> Action {
        let buffer = self.current_mut();
        let current = buffer.selection.current();
        buffer.grab = current.selection_type().map(|_| current.range());
        meowr_core::cancel(&mut buffer.selection);
        Action::Redraw
    }

    /// `meow-pop-grab`: the Grab becomes the selection, and the Grab goes.
    pub(crate) fn pop_grab(&mut self) -> Action {
        let buffer = self.current_mut();
        if buffer.grab.is_none() {
            return self.no_grab();
        }
        let range = buffer.grab.take().expect("just checked");
        // `secondary-selection-to-region` activates the region and no more:
        // no selection type, and no push onto the selection history, because
        // it never goes through `meow--select`.
        buffer
            .selection
            .select_without_history(Selection::spanning(range.start, range.end));
        self.scroll_into_view();
        Action::Redraw
    }

    /// `meow-swap-grab`: each takes the other's text.
    ///
    /// The later range is replaced first, so the earlier one's offsets are
    /// still valid when its turn comes — replacing in buffer order would move
    /// the second range out from under itself.
    pub(crate) fn swap_grab(&mut self) -> Action {
        let buffer = self.current_mut();
        let selection = buffer.selection.current();
        let (Some(grab), Some(_)) = (buffer.grab.clone(), selection.selection_type()) else {
            if buffer.grab.is_none() {
                return self.no_grab();
            }
            return self.refuse("No selection to exchange with the Grab.");
        };
        let range = selection.range();
        if grab.start.max(range.start) < grab.end.min(range.end) {
            // Overlapping ranges cannot be exchanged coherently. This rang the
            // bell and said nothing, which is the failure mode #80 is named
            // for — and it survived the first pass because the discipline
            // table could only see the cases somebody had thought to list.
            return self.refuse_aloud("The selection and the Grab overlap.");
        }
        let Content::Ready(doc) = &buffer.content else {
            return self.refuse("Still reading this file.");
        };
        let text = doc.text();
        let slice = |r: &Range<usize>| {
            text.slice(text.byte_to_char(r.start)..text.byte_to_char(r.end))
                .to_string()
        };
        let (selected, grabbed) = (slice(&range), slice(&grab));
        let (selected_len, grabbed_len) = (selected.len(), grabbed.len());
        let (first, first_text, second, second_text) = if range.start < grab.start {
            (range.clone(), grabbed, grab.clone(), selected)
        } else {
            (grab.clone(), selected, range.clone(), grabbed)
        };
        let Content::Ready(doc) = &mut buffer.content else {
            return self.refuse("Still reading this file.");
        };
        let _ = buffer
            .history
            .delete(doc, &mut buffer.selection, second.clone());
        let _ = buffer
            .history
            .insert(doc, &mut buffer.selection, second.start, &second_text);
        let _ = buffer
            .history
            .delete(doc, &mut buffer.selection, first.clone());
        let _ = buffer
            .history
            .insert(doc, &mut buffer.selection, first.start, &first_text);
        // The Grab stays where it is and now holds what the selection held.
        // If the selection was the *earlier* of the two, replacing it changed
        // the buffer's length before the Grab, so the Grab's offset moves by
        // that difference — the one arithmetic in this function that is easy
        // to get silently wrong.
        let grab_start = if range.start < grab.start {
            let before = (range.end - range.start) as isize;
            // What replaced it: the Grab's old text.
            let after = grabbed_len as isize;
            (grab.start as isize + after - before) as usize
        } else {
            grab.start
        };
        meowr_core::cancel(&mut buffer.selection);
        // Placed *after* `after_command`, which shifts the Grab to account for
        // the edits just made. This range is already in post-edit
        // coordinates, so letting it be shifted again would move it twice —
        // which it did, to `0..0`, until a test said so.
        buffer.after_command(10);
        buffer.grab = Some(grab_start..grab_start + selected_len);
        Action::Redraw
    }

    /// `meow-sync-grab`: the Grab takes the selection's text.
    pub(crate) fn sync_grab(&mut self) -> Action {
        let buffer = self.current_mut();
        let selection = buffer.selection.current();
        let (Some(grab), Some(_)) = (buffer.grab.clone(), selection.selection_type()) else {
            if buffer.grab.is_none() {
                return self.no_grab();
            }
            return self.refuse("No selection to exchange with the Grab.");
        };
        let range = selection.range();
        let Content::Ready(doc) = &buffer.content else {
            return Action::Nothing;
        };
        let text = doc.text();
        let selected = text
            .slice(text.byte_to_char(range.start)..text.byte_to_char(range.end))
            .to_string();
        let Content::Ready(doc) = &mut buffer.content else {
            return self.refuse("Still reading this file.");
        };
        let _ = buffer
            .history
            .delete(doc, &mut buffer.selection, grab.clone());
        let _ = buffer
            .history
            .insert(doc, &mut buffer.selection, grab.start, &selected);
        meowr_core::cancel(&mut buffer.selection);
        buffer.after_command(10);
        // As in `swap_grab`: already in post-edit coordinates.
        buffer.grab = Some(grab.start..grab.start + selected.len());
        Action::Redraw
    }
}
