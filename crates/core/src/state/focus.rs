//! Focus routing: which pane/overlay claims keys, and the modal push/pop
//! stack. See docs/architecture.md §4.5.

use std::path::PathBuf;

use crate::model::ids::{ChatId, MessageId};

#[derive(Debug, Clone, PartialEq)]
pub enum Focus {
    ChatList,
    ChatFilter,
    Composer,
    /// Message selection mode (chips visible).
    Selection,
    ChatSearch,
    Palette,
    Help,
    Modal(ModalKind),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ModalKind {
    ConfirmDelete {
        chat_id: ChatId,
        message_id: MessageId,
        can_revoke: bool,
    },
    ConfirmSendFile {
        path: PathBuf,
    },
}

/// Invariant: never empty. `Esc` pops exactly one level and never pops the base.
#[derive(Debug, Clone, PartialEq)]
pub struct FocusStack {
    stack: Vec<Focus>,
}

impl FocusStack {
    pub fn new(base: Focus) -> Self {
        FocusStack { stack: vec![base] }
    }
    pub fn current(&self) -> &Focus {
        self.stack.last().expect("focus stack never empty")
    }
    pub fn push(&mut self, f: Focus) {
        self.stack.push(f);
    }
    /// Pops one level; returns false (and does nothing) at the base.
    pub fn pop(&mut self) -> bool {
        if self.stack.len() > 1 {
            self.stack.pop();
            true
        } else {
            false
        }
    }
    pub fn replace_base(&mut self, f: Focus) {
        self.stack[0] = f;
    }
    pub fn depth(&self) -> usize {
        self.stack.len()
    }
    /// Whether `f` is anywhere in the stack rather than merely on top of it.
    ///
    /// [`Self::current`] answers "what claims keys"; this answers "what mode
    /// is the user in". The two differ whenever a level is pushed *over*
    /// another without leaving it, which is every modal and both overlays:
    /// a confirm dialog over selection mode leaves `current()` reporting
    /// `Modal(_)` while the user is still, in every sense they would
    /// recognize, selecting a message. Routing wants `current()` — that is
    /// what a stack is for. Anything asking whether a mode is still *active*
    /// wants this, and reaching for `current()` there is a silent bug, not a
    /// compile error (architecture §4.5).
    ///
    /// Equality-based, so `Modal(_)` matches an exact [`ModalKind`]. Every
    /// caller today asks about a payload-free variant; "is any modal open"
    /// would need its own discriminant-based helper.
    pub fn contains(&self, f: &Focus) -> bool {
        self.stack.contains(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esc_pops_exactly_one_level_and_never_the_base() {
        let mut stack = FocusStack::new(Focus::ChatList);
        stack.push(Focus::Composer);
        stack.push(Focus::Palette);
        assert_eq!(stack.depth(), 3);

        assert!(stack.pop());
        assert_eq!(stack.depth(), 2);
        assert_eq!(stack.current(), &Focus::Composer);

        assert!(stack.pop());
        assert_eq!(stack.depth(), 1);
        assert_eq!(stack.current(), &Focus::ChatList);

        // Third pop hits the base: floor holds, pop reports failure.
        assert!(!stack.pop());
        assert_eq!(stack.depth(), 1);
        assert_eq!(stack.current(), &Focus::ChatList);
    }

    #[test]
    fn contains_sees_levels_current_cannot() {
        let mut stack = FocusStack::new(Focus::Composer);
        stack.push(Focus::Selection);
        stack.push(Focus::Modal(ModalKind::ConfirmSendFile {
            path: PathBuf::from("/tmp/x"),
        }));

        // The whole point: the user is still in selection mode, and only
        // one of these two questions can tell.
        assert!(stack.contains(&Focus::Selection));
        assert_ne!(stack.current(), &Focus::Selection);
        // The base counts too, not just the levels above it.
        assert!(stack.contains(&Focus::Composer));
        assert!(!stack.contains(&Focus::ChatList));

        // Popping the modal off does not change the answer for Selection;
        // popping Selection does.
        stack.pop();
        assert!(stack.contains(&Focus::Selection));
        stack.pop();
        assert!(!stack.contains(&Focus::Selection));

        // Payload-carrying variants compare exactly.
        stack.push(Focus::Modal(ModalKind::ConfirmSendFile {
            path: PathBuf::from("/tmp/x"),
        }));
        assert!(stack.contains(&Focus::Modal(ModalKind::ConfirmSendFile {
            path: PathBuf::from("/tmp/x"),
        })));
        assert!(!stack.contains(&Focus::Modal(ModalKind::ConfirmSendFile {
            path: PathBuf::from("/tmp/y"),
        })));
    }
}
