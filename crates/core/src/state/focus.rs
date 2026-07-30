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
}
