//! Action chips shown in selection mode. See docs/architecture.md §4.2;
//! spec §5.3.
//!
//! The whole point of this module: **an action that would fail is never
//! offered**. Every chip in the returned row is backed either by a TDLib
//! capability flag ([`MessageCaps`], fetched per message via
//! `getMessageProperties` — architecture §7) or by a local fact the client
//! can prove on its own (this message carries a file; that file is already
//! downloaded; this send failed). Nothing here is a static menu, so a chip
//! row is a truthful statement about what Telegram will accept right now.

use crate::model::message::MessageCaps;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chip {
    Reply,    // 'r'
    Forward,  // 'f'
    React,    // 'e'
    Copy,     // 'c'
    Edit,     // 'd'  (only own editable messages)
    Delete,   // 'x'
    Download, // 'l'  (file content, not yet downloaded)
    Open,     // 'o'  (file content, downloaded)
    Resend,   // 's'  (only SendState::Failed)
    /// The `⏎`-on-selected-message reveal the design spec calls for
    /// (architecture §7.5.1, T77): unlike every other chip, it is not
    /// derived by `chips_for` from `MessageCaps` — a local rendering fact
    /// (an unrevealed `Spoiler` entity) gates it instead, appended by
    /// `selection.rs` after `chips_for` runs. Its presence here, not a
    /// hidden key binding, is what keeps the row "the truth about what is
    /// possible" for this action too.
    Reveal, // 'v'  (message has an unrevealed spoiler)
    /// Abandon an upload still in flight (spec §452: uploads "are
    /// cancellable"). Like [`Chip::Reveal`] it is not a TDLib capability —
    /// `MediaState::uploads` holding an entry for the message is the local
    /// fact that gates it — so `selection.rs` appends it after `chips_for`
    /// runs rather than widening that function.
    ///
    /// Deliberately *not* suppressed on a failed send, unlike `Reveal`. The
    /// two are opposites: a message that never reached the server has no
    /// server-confirmed content to reveal, but an upload that is still
    /// tracked is exactly the thing a user wants to abandon, and `Resend` is
    /// the only other chip a failed send offers. Withholding cancel there
    /// would leave a stuck upload with no way out but quitting.
    CancelUpload, // 'k'  (an upload for this message is still in flight)
}

impl Chip {
    pub fn shortcut(self) -> char {
        match self {
            Chip::Reply => 'r',
            Chip::Forward => 'f',
            Chip::React => 'e',
            Chip::Copy => 'c',
            Chip::Edit => 'd',
            Chip::Delete => 'x',
            Chip::Download => 'l',
            Chip::Open => 'o',
            Chip::Resend => 's',
            Chip::Reveal => 'v',
            // Not 'c' (Copy) and not 'x' (Delete); 'k' is free and reads as
            // "kill" without colliding with either.
            Chip::CancelUpload => 'k',
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Chip::Reply => "Reply",
            Chip::Forward => "Forward",
            Chip::React => "React",
            Chip::Copy => "Copy",
            Chip::Edit => "Edit",
            Chip::Delete => "Delete",
            Chip::Download => "Download",
            Chip::Open => "Open",
            Chip::Resend => "Resend",
            Chip::Reveal => "Reveal",
            Chip::CancelUpload => "Cancel upload",
        }
    }
}

/// Pure derivation from TDLib capability flags plus local message facts.
/// An action that would fail is never offered.
pub fn chips_for(
    caps: &MessageCaps,
    is_outgoing: bool,
    has_file: bool,
    file_downloaded: bool,
    send_failed: bool,
) -> Vec<Chip> {
    let mut chips = Vec::new();
    if send_failed {
        chips.push(Chip::Resend);
        chips.push(Chip::Delete);
        return chips;
    }
    chips.push(Chip::Reply);
    if caps.can_be_forwarded {
        chips.push(Chip::Forward);
    }
    chips.push(Chip::React);
    if caps.can_be_saved {
        chips.push(Chip::Copy);
    }
    if is_outgoing && caps.can_be_edited {
        chips.push(Chip::Edit);
    }
    if has_file && !file_downloaded {
        chips.push(Chip::Download);
    }
    if has_file && file_downloaded {
        chips.push(Chip::Open);
    }
    if caps.can_be_deleted_for_all_users || caps.can_be_deleted_only_for_self {
        chips.push(Chip::Delete);
    }
    chips
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    const ALL: &[Chip] = &[
        Chip::Reply,
        Chip::Forward,
        Chip::React,
        Chip::Copy,
        Chip::Edit,
        Chip::Delete,
        Chip::Download,
        Chip::Open,
        Chip::Resend,
        Chip::Reveal,
    ];

    fn caps(
        can_be_edited: bool,
        can_be_deleted_for_all_users: bool,
        can_be_deleted_only_for_self: bool,
        can_be_forwarded: bool,
        can_be_saved: bool,
    ) -> MessageCaps {
        MessageCaps {
            can_be_edited,
            can_be_deleted_for_all_users,
            can_be_deleted_only_for_self,
            can_be_forwarded,
            can_be_saved,
        }
    }

    struct Case {
        caps: MessageCaps,
        is_outgoing: bool,
        has_file: bool,
        file_downloaded: bool,
        send_failed: bool,
        expected: Vec<Chip>,
    }

    fn case(
        caps: MessageCaps,
        is_outgoing: bool,
        has_file: bool,
        file_downloaded: bool,
        send_failed: bool,
        expected: Vec<Chip>,
    ) -> Case {
        Case {
            caps,
            is_outgoing,
            has_file,
            file_downloaded,
            send_failed,
            expected,
        }
    }

    /// The table this task exists for: every chip traces back to a capability
    /// flag or a local fact, and nothing is offered "just because".
    #[test]
    fn chips_derive_from_caps_never_hardcoded() {
        let table = [
            // Nothing permitted at all: only the two actions that need no
            // capability (replying and reacting are chat-level rights).
            case(
                caps(false, false, false, false, false),
                false,
                false,
                false,
                false,
                vec![Chip::Reply, Chip::React],
            ),
            // A plain incoming message in a normal chat.
            case(
                caps(false, false, true, true, true),
                false,
                false,
                false,
                false,
                vec![
                    Chip::Reply,
                    Chip::Forward,
                    Chip::React,
                    Chip::Copy,
                    Chip::Delete,
                ],
            ),
            // Own message, still editable, deletable for everyone.
            case(
                caps(true, true, true, true, true),
                true,
                false,
                false,
                false,
                vec![
                    Chip::Reply,
                    Chip::Forward,
                    Chip::React,
                    Chip::Copy,
                    Chip::Edit,
                    Chip::Delete,
                ],
            ),
            // `can_be_edited` on someone ELSE's message (channel admin case
            // reported by TDLib) must not offer Edit: is_outgoing gates it.
            case(
                caps(true, false, false, false, false),
                false,
                false,
                false,
                false,
                vec![Chip::Reply, Chip::React],
            ),
            // Protected content (`can_be_saved == false`): no Copy, no Forward.
            case(
                caps(false, false, true, false, false),
                false,
                false,
                false,
                false,
                vec![Chip::Reply, Chip::React, Chip::Delete],
            ),
            // File not yet on disk → Download, never Open.
            case(
                caps(false, false, false, true, true),
                false,
                true,
                false,
                false,
                vec![
                    Chip::Reply,
                    Chip::Forward,
                    Chip::React,
                    Chip::Copy,
                    Chip::Download,
                ],
            ),
            // Same file, downloaded → Open, never Download.
            case(
                caps(false, false, false, true, true),
                false,
                true,
                true,
                false,
                vec![
                    Chip::Reply,
                    Chip::Forward,
                    Chip::React,
                    Chip::Copy,
                    Chip::Open,
                ],
            ),
            // Failed send short-circuits everything, however permissive the
            // caps look: the message does not exist server-side yet.
            case(
                caps(true, true, true, true, true),
                true,
                true,
                true,
                true,
                vec![Chip::Resend, Chip::Delete],
            ),
        ];

        for c in table {
            assert_eq!(
                chips_for(
                    &c.caps,
                    c.is_outgoing,
                    c.has_file,
                    c.file_downloaded,
                    c.send_failed
                ),
                c.expected,
                "caps={:?} outgoing={} file={} downloaded={} failed={}",
                c.caps,
                c.is_outgoing,
                c.has_file,
                c.file_downloaded,
                c.send_failed
            );
        }
    }

    #[test]
    fn delete_offered_for_either_delete_capability() {
        let for_all = chips_for(
            &caps(false, true, false, false, false),
            false,
            false,
            false,
            false,
        );
        let self_only = chips_for(
            &caps(false, false, true, false, false),
            false,
            false,
            false,
            false,
        );
        assert!(for_all.contains(&Chip::Delete));
        assert!(self_only.contains(&Chip::Delete));
    }

    #[test]
    fn chip_shortcut_letters_unique_per_row() {
        // Globally unique, which is the strongest form of "unique per row":
        // no row can ever contain two chips answering to the same letter.
        let letters: HashSet<char> = ALL.iter().map(|c| c.shortcut()).collect();
        assert_eq!(letters.len(), ALL.len());

        // And prove it on the rows the derivation actually produces, over
        // every combination of the five inputs.
        for bits in 0u8..32 {
            let caps = caps(
                bits & 1 != 0,
                bits & 2 != 0,
                bits & 4 != 0,
                bits & 8 != 0,
                bits & 16 != 0,
            );
            for flags in 0u8..16 {
                let row = chips_for(
                    &caps,
                    flags & 1 != 0,
                    flags & 2 != 0,
                    flags & 4 != 0,
                    flags & 8 != 0,
                );
                let row_letters: HashSet<char> = row.iter().map(|c| c.shortcut()).collect();
                assert_eq!(
                    row_letters.len(),
                    row.len(),
                    "duplicate shortcut in {row:?}"
                );
            }
        }
    }

    #[test]
    fn download_and_open_are_mutually_exclusive() {
        for downloaded in [false, true] {
            let row = chips_for(
                &caps(false, false, false, false, false),
                false,
                true,
                downloaded,
                false,
            );
            assert!(!(row.contains(&Chip::Download) && row.contains(&Chip::Open)));
        }
    }

    #[test]
    fn labels_are_distinct() {
        let labels: HashSet<&str> = ALL.iter().map(|c| c.label()).collect();
        assert_eq!(labels.len(), ALL.len());
    }
}
