//! Mock chats, messages and folders for the `tgt --demo` recording (see the
//! parent module docs). Everything here is invented: no real names, no
//! plausible phone numbers, nothing that could be mistaken for an actual
//! person's conversation.
//!
//! Chat 1 ("Nova") is the one the recording opens: a reply, a reaction, an
//! edited message, a spoiler and the demo's one photo all live there, as the
//! *last* nine messages — so they're the ones on screen the instant the chat
//! opens (the conversation pane starts scrolled to the bottom, like a real
//! one). The other four chats exist to show unread badges, folders and the
//! different chat kinds (group/channel/supergroup) in the sidebar; the
//! recording never opens them, so they carry only a `last_message` preview,
//! not a full history.
//!
//! # Why Nova's history is padded to a full 50-message page
//!
//! `state::conversation::apply_history_page`'s "cold open" case: a chat
//! opened for the first time answers `getChatHistory(only_local: true)` with
//! whatever few messages TDLib's *local* database already has (often just
//! the one chat-list preview), which is short. A short opening page fires
//! *two* more requests **in the same effect batch**, per that function's own
//! docs ("A cold open does put both in flight at once"): T59's remote
//! reconcile (`from_message_id: 0`) and T67's viewport-fill (`from` the
//! oldest loaded message, walking further back) — plus, separately, media's
//! auto-download `DownloadFile` for any photo now in view. All three are
//! spawned via `tokio::spawn` from one `effects.drain()` loop
//! (`runtime_loop::Core::step_until`), and tokio's scheduler gives no
//! ordering guarantee across independently spawned tasks — so which one a
//! strict, single-cursor `FakeTd` script would see *first* is not something
//! this fixture can predict or pin down.
//!
//! `crates/app/tests/read_only.rs`'s own `read_only_script` sidesteps
//! exactly this by always seeding a full `PAGE_SIZE` (50) opening page —
//! see its doc comment ("A full page, so T67's viewport fill has nothing to
//! do"). This module does the same: [`nova_history`] returns exactly 50
//! messages, so `apply_history_page` never calls `fill_viewport` at all, and
//! the only request left to script beyond the opening page is T59's
//! reconcile — one predictable `Await`, matching `read_only_script` step for
//! step. The photo's `DownloadFile` race is avoided the same way `runtime.rs`
//! (this module's now-removed sibling, from before the brief narrowed to a
//! single scripted recording — see `crate::demo`'s module docs) used to
//! avoid it structurally: [`demo::script`](super::script) emits the photo's
//! `FileSnapshot` as already complete *before* the chat is ever opened, so
//! `media::should_auto_request` sees it already downloaded and never issues
//! `DownloadFile` in the first place.

use tgt_core::model::chat::{
    ChatKind, ChatListId, ChatPositionEntry, ChatView, FolderInfo, MessagePreview,
};
use tgt_core::model::entity::{EntityKind, FormattedText, TextEntity};
use tgt_core::model::ids::{ChatId, FileId, MessageId, UserId};
use tgt_core::model::message::{
    MessageCaps, MessageContent, MessageView, ReactionView, ReplyPreview, SendState, Sender,
};

/// The "me" persona every outgoing demo message is sent as.
pub const YOU: UserId = UserId(1);
/// The demo's one media file: the placeholder (or `TGT_DEMO_PHOTO`-supplied)
/// cat photo in chat 1.
pub const PHOTO_FILE_ID: FileId = FileId(1);
/// The chat the recording opens. `Await` steps in `demo::script` are scripted
/// against this id specifically (not just `GetChatHistory`'s request kind),
/// so a stray request for a different chat can't be mistaken for it.
pub const CHAT_NOVA: ChatId = ChatId(1);

const NOVA: UserId = UserId(2);
// Ada/Sam/Priya/Lin/Kai never appear as a live `Sender` — the other four
// chats are never opened by the recording, so they carry only a cosmetic
// `MessagePreview` (a plain display string) rather than a full message
// history with a `UserId`-backed sender.

const CHAT_ADA: ChatId = ChatId(2);
const CHAT_HIKING: ChatId = ChatId(3);
const CHAT_RELEASE_NOTES: ChatId = ChatId(4);
const CHAT_DESIGN_SYNC: ChatId = ChatId(5);

const FOLDER_WORK: i32 = 1;
const FOLDER_FRIENDS: i32 = 2;

/// A plausible-looking base timestamp (2024-11-19, arbitrary); messages
/// within a chat are spaced roughly a minute apart from there.
const BASE_DATE: i64 = 1_732_000_000;

/// `getChatHistory`'s opening page size (`state::history::PAGE_SIZE`), and
/// the length [`nova_history`] pads to. See the module docs for why.
const OPENING_PAGE_SIZE: usize = 50;
/// How many of Nova's 50 messages are the ones actually built to demonstrate
/// something (reply/reaction/edit/spoiler/photo) — always the newest, so
/// they're what's on screen when the chat opens scrolled to the bottom.
const FEATURED_MESSAGES: usize = 9;

pub struct DemoContent {
    pub folders: Vec<FolderInfo>,
    pub chats: Vec<ChatView>,
    /// The one chat the recording opens. Always [`OPENING_PAGE_SIZE`] long —
    /// see the module docs.
    pub nova_history: Vec<MessageView>,
}

/// Builds the complete mock dataset. `photo_dims` comes from
/// `photo::resolve` — the message content only needs the file id
/// ([`PHOTO_FILE_ID`]) and declared dimensions; `demo::script` is what joins
/// the id to an actual path on disk, via a `FileSnapshot`.
pub fn seed(photo_dims: (u32, u32)) -> DemoContent {
    let (photo_width, photo_height) = photo_dims;
    let nova_history = nova_history(photo_width, photo_height);
    let nova_preview = preview_of(nova_history.last().expect("nova_history is never empty"));

    let chats = vec![
        chat(
            CHAT_NOVA,
            ChatKind::Private,
            "Nova",
            500,
            &[],
            0,
            Some(nova_preview),
        ),
        chat(
            CHAT_ADA,
            ChatKind::Private,
            "Ada Lovelace (Demo)",
            480,
            &[],
            3,
            Some(MessagePreview {
                sender_name: "Ada Lovelace (Demo)".to_string(),
                text: "Ping me when you're ready.".to_string(),
                date: BASE_DATE + 10_180,
                is_outgoing: false,
            }),
        ),
        chat(
            CHAT_HIKING,
            ChatKind::Group,
            "Weekend Hiking Crew",
            460,
            &[FOLDER_FRIENDS],
            0,
            Some(MessagePreview {
                sender_name: "Sam".to_string(),
                text: "Perfect.".to_string(),
                date: BASE_DATE + 20_090,
                is_outgoing: false,
            }),
        ),
        chat(
            CHAT_RELEASE_NOTES,
            ChatKind::Channel,
            "tgt Release Notes",
            440,
            &[FOLDER_WORK],
            1,
            Some(MessagePreview {
                sender_name: "tgt Release Notes".to_string(),
                text: "v0.2.0-demo: inline images, folders and reactions are here 🎉".to_string(),
                date: BASE_DATE + 30_000,
                is_outgoing: false,
            }),
        ),
        chat(
            CHAT_DESIGN_SYNC,
            ChatKind::Supergroup,
            "Design Sync",
            420,
            &[FOLDER_WORK],
            0,
            Some(MessagePreview {
                sender_name: "Kai".to_string(),
                text: "Looks great!".to_string(),
                date: BASE_DATE + 40_060,
                is_outgoing: false,
            }),
        ),
    ];

    DemoContent {
        folders: vec![
            FolderInfo {
                id: FOLDER_WORK,
                title: "Work".to_string(),
            },
            FolderInfo {
                id: FOLDER_FRIENDS,
                title: "Friends".to_string(),
            },
        ],
        chats,
        nova_history,
    }
}

#[allow(clippy::too_many_arguments)]
fn chat(
    id: ChatId,
    kind: ChatKind,
    title: &str,
    order: i64,
    folders: &[i32],
    unread_count: u32,
    last_message: Option<MessagePreview>,
) -> ChatView {
    let mut positions = vec![ChatPositionEntry {
        list: ChatListId::Main,
        order,
        is_pinned: false,
    }];
    for &folder_id in folders {
        positions.push(ChatPositionEntry {
            list: ChatListId::Folder(folder_id),
            order,
            is_pinned: false,
        });
    }

    ChatView {
        id,
        kind,
        title: title.to_string(),
        positions,
        unread_count,
        unread_mention_count: 0,
        last_message,
        is_muted: false,
    }
}

fn text(text: &str) -> FormattedText {
    FormattedText {
        text: text.to_string(),
        entities: Vec::new(),
    }
}

/// A UTF-16-correct spoiler entity over the first occurrence of `needle` in
/// `body` (architecture.md's UTF-16-offsets gotcha: entity offsets are UTF-16
/// code units, computed here rather than assumed to equal byte counts).
fn spoiler(body: &str, needle: &str) -> FormattedText {
    let byte_idx = body
        .find(needle)
        .expect("demo content: spoiler needle must be present in its own text");
    let offset_utf16 = body[..byte_idx].encode_utf16().count() as u32;
    let length_utf16 = needle.encode_utf16().count() as u32;
    FormattedText {
        text: body.to_string(),
        entities: vec![TextEntity {
            offset_utf16,
            length_utf16,
            kind: EntityKind::Spoiler,
        }],
    }
}

fn plain_text_of(content: &MessageContent) -> &str {
    match content {
        MessageContent::Text(t) => t.text.as_str(),
        MessageContent::Photo { caption, .. }
        | MessageContent::Video { caption, .. }
        | MessageContent::Document { caption, .. } => caption.text.as_str(),
        MessageContent::Audio { file_name, .. } => file_name.as_str(),
        MessageContent::Sticker { emoji } => emoji.as_str(),
        MessageContent::Unsupported { description } => description.as_str(),
    }
}

fn preview_of(m: &MessageView) -> MessagePreview {
    const MAX: usize = 80;
    let text = plain_text_of(&m.content);
    let text = if text.chars().count() <= MAX {
        text.to_string()
    } else {
        text.chars().take(MAX).collect::<String>() + "…"
    };
    MessagePreview {
        sender_name: m.sender_name.clone(),
        text,
        date: m.date,
        is_outgoing: m.is_outgoing,
    }
}

fn incoming_caps() -> MessageCaps {
    MessageCaps {
        can_be_edited: false,
        can_be_deleted_for_all_users: false,
        can_be_deleted_only_for_self: true,
        can_be_forwarded: true,
        can_be_saved: true,
    }
}

pub fn outgoing_caps() -> MessageCaps {
    MessageCaps {
        can_be_edited: true,
        can_be_deleted_for_all_users: true,
        can_be_deleted_only_for_self: true,
        can_be_forwarded: true,
        can_be_saved: true,
    }
}

fn incoming(
    id: i64,
    chat_id: ChatId,
    sender: UserId,
    sender_name: &str,
    date: i64,
    content: MessageContent,
) -> MessageView {
    MessageView {
        id: MessageId(id),
        chat_id,
        sender: Sender::User(sender),
        sender_name: sender_name.to_string(),
        is_outgoing: false,
        date,
        content,
        reply_to: None,
        send_state: SendState::Sent,
        reactions: Vec::new(),
        caps: incoming_caps(),
        is_edited: false,
    }
}

fn outgoing(id: i64, chat_id: ChatId, date: i64, content: MessageContent) -> MessageView {
    MessageView {
        id: MessageId(id),
        chat_id,
        sender: Sender::User(YOU),
        sender_name: "You".to_string(),
        is_outgoing: true,
        date,
        content,
        reply_to: None,
        send_state: SendState::Sent,
        reactions: Vec::new(),
        caps: outgoing_caps(),
        is_edited: false,
    }
}

/// Short, generic exchanges that pad Nova's history out to a full opening
/// page (module docs). Cycled rather than hand-written 40-odd times; nothing
/// here is meant to be read — the recording opens Nova already scrolled to
/// the bottom, where the featured messages are, and never scrolls up into
/// this filler.
const FILLER_LINES: &[&str] = &[
    "Sounds good.",
    "On it.",
    "👍",
    "Let me check.",
    "Almost done.",
    "One sec.",
    "Ready when you are.",
    "Just saw this, thanks!",
    "Will do.",
    "Makes sense.",
    "👌",
    "Sent!",
    "Good catch.",
    "Yep, agreed.",
];

/// Chat 1's full opening page: [`OPENING_PAGE_SIZE`] messages, the newest
/// [`FEATURED_MESSAGES`] of which demonstrate a reply, a reaction, an edited
/// message, a spoiler and the demo's photo. See the module docs for why the
/// page is padded to a full 50 rather than just the featured nine.
fn nova_history(photo_width: u32, photo_height: u32) -> Vec<MessageView> {
    let filler_count = OPENING_PAGE_SIZE - FEATURED_MESSAGES;
    let mut messages: Vec<MessageView> = (1..=filler_count as i64)
        .map(|id| {
            let line = FILLER_LINES[(id as usize - 1) % FILLER_LINES.len()];
            let date = BASE_DATE + id * 45;
            if id % 2 == 1 {
                incoming(
                    id,
                    CHAT_NOVA,
                    NOVA,
                    "Nova",
                    date,
                    MessageContent::Text(text(line)),
                )
            } else {
                outgoing(id, CHAT_NOVA, date, MessageContent::Text(text(line)))
            }
        })
        .collect();

    let base = filler_count as i64;
    let t = BASE_DATE + base * 45;

    let reply_source_text = "Yes! I'll bring the laptop.";
    let reply_source_id = base + 2;
    let reply_source = outgoing(
        reply_source_id,
        CHAT_NOVA,
        t + 60,
        MessageContent::Text(text(reply_source_text)),
    );

    let mut reply = incoming(
        base + 3,
        CHAT_NOVA,
        NOVA,
        "Nova",
        t + 130,
        MessageContent::Text(text("Perfect, see you at 10.")),
    );
    reply.reply_to = Some(ReplyPreview {
        message_id: MessageId(reply_source_id),
        sender_name: "You".to_string(),
        excerpt: reply_source_text.to_string(),
    });

    let mut photo = incoming(
        base + 5,
        CHAT_NOVA,
        NOVA,
        "Nova",
        t + 300,
        MessageContent::Photo {
            file_id: PHOTO_FILE_ID,
            width: photo_width,
            height: photo_height,
            caption: text("This is Ferris, my code reviewer 🐱"),
        },
    );
    photo.reactions = vec![ReactionView {
        emoji: "❤️".to_string(),
        count: 2,
        chosen_by_me: true,
    }];

    let mut edited = outgoing(
        base + 6,
        CHAT_NOVA,
        t + 360,
        MessageContent::Text(text("Let's meet at 10:30 instead, running a bit late.")),
    );
    edited.is_edited = true;

    messages.extend([
        incoming(
            base + 1,
            CHAT_NOVA,
            NOVA,
            "Nova",
            t,
            MessageContent::Text(text("Hey! Are we still on for the walkthrough tomorrow?")),
        ),
        reply_source,
        reply,
        outgoing(
            base + 4,
            CHAT_NOVA,
            t + 250,
            MessageContent::Text(text("Also, meet the newest team member.")),
        ),
        photo,
        edited,
        incoming(
            base + 7,
            CHAT_NOVA,
            NOVA,
            "Nova",
            t + 420,
            MessageContent::Text(spoiler(
                "The wifi password is hunter2, don't tell anyone.",
                "hunter2",
            )),
        ),
        incoming(
            base + 8,
            CHAT_NOVA,
            NOVA,
            "Nova",
            t + 480,
            MessageContent::Text(text("🎉 Sounds good, see you then!")),
        ),
        outgoing(
            base + 9,
            CHAT_NOVA,
            t + 540,
            MessageContent::Text(text("See you soon 👋")),
        ),
    ]);

    debug_assert_eq!(messages.len(), OPENING_PAGE_SIZE);
    messages
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nova_history_is_a_full_opening_page() {
        let content = seed((320, 320));
        assert_eq!(
            content.nova_history.len(),
            OPENING_PAGE_SIZE,
            "a short opening page would race T67's viewport-fill against T59's \
             reconcile — see the module docs"
        );
        // Ascending, contiguous ids: every other seed builder and `FakeTd`
        // fixture in this codebase assumes this window invariant.
        for (idx, m) in content.nova_history.iter().enumerate() {
            assert_eq!(m.id, MessageId(idx as i64 + 1));
        }
    }

    #[test]
    fn nova_chat_demonstrates_every_feature() {
        let content = seed((320, 320));
        let nova = &content.nova_history;

        assert!(
            nova.iter().any(|m| m.reply_to.is_some()),
            "expected a reply"
        );
        assert!(
            nova.iter().any(|m| !m.reactions.is_empty()),
            "expected a reaction"
        );
        assert!(
            nova.iter().any(|m| m.is_edited),
            "expected an edited message"
        );
        assert!(
            nova.iter()
                .any(|m| matches!(&m.content, MessageContent::Text(t)
                if t.entities.iter().any(|e| e.kind == EntityKind::Spoiler))),
            "expected a spoiler"
        );
        assert!(
            nova.iter()
                .any(|m| matches!(&m.content, MessageContent::Photo { .. })),
            "expected a photo"
        );
    }

    #[test]
    fn spoiler_offsets_are_utf16_correct() {
        let ft = spoiler("café hunter2 café", "hunter2");
        let entity = &ft.entities[0];
        // "café " is 5 chars, all but "é" single UTF-16 units — "é" is one
        // UTF-16 unit too (it's in the BMP), so the offset equals the char
        // count here; this test exists so a future non-BMP needle would
        // catch a byte-offset regression via encode_utf16().
        assert_eq!(entity.offset_utf16, 5);
        assert_eq!(entity.length_utf16, 7);
    }

    #[test]
    fn folder_positions_include_main_and_the_named_folder() {
        let content = seed((320, 320));
        let hiking = content
            .chats
            .iter()
            .find(|c| c.id == CHAT_HIKING)
            .expect("hiking chat");
        assert!(hiking.positions.iter().any(|p| p.list == ChatListId::Main));
        assert!(
            hiking
                .positions
                .iter()
                .any(|p| p.list == ChatListId::Folder(FOLDER_FRIENDS))
        );
    }

    #[test]
    fn unread_chat_carries_a_nonzero_badge() {
        let content = seed((320, 320));
        let ada = content
            .chats
            .iter()
            .find(|c| c.id == CHAT_ADA)
            .expect("ada chat");
        assert_eq!(ada.unread_count, 3);
    }

    #[test]
    fn nova_preview_matches_its_newest_message() {
        let content = seed((320, 320));
        let nova = content
            .chats
            .iter()
            .find(|c| c.id == CHAT_NOVA)
            .expect("nova chat");
        let preview = nova.last_message.as_ref().expect("nova has a preview");
        assert_eq!(preview.text, "See you soon 👋");
        assert!(preview.is_outgoing);
    }
}
