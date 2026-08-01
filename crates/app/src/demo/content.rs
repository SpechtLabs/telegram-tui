//! Mock chats, messages and folders for `tgt --demo` (see the parent module
//! docs). Everything here is invented: no real names, no plausible phone
//! numbers, nothing that could be mistaken for an actual person's
//! conversation.
//!
//! Chat 1 ("Nova") is the one built to show the product off in a single
//! recording pass: a reply, a reaction, an edited message, a spoiler and the
//! demo's one photo all live there. The rest exist to show unread badges,
//! folders and the different chat kinds (group/channel/supergroup).

use std::collections::HashMap;

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

const NOVA: UserId = UserId(2);
const ADA: UserId = UserId(3);
const SAM: UserId = UserId(4);
const PRIYA: UserId = UserId(5);
const LIN: UserId = UserId(6);
const KAI: UserId = UserId(7);

const CHAT_NOVA: ChatId = ChatId(1);
const CHAT_ADA: ChatId = ChatId(2);
const CHAT_HIKING: ChatId = ChatId(3);
const CHAT_RELEASE_NOTES: ChatId = ChatId(4);
const CHAT_DESIGN_SYNC: ChatId = ChatId(5);

const FOLDER_WORK: i32 = 1;
const FOLDER_FRIENDS: i32 = 2;

/// A plausible-looking base timestamp (2024-11-19, arbitrary); messages
/// within a chat are spaced a few minutes apart from there.
const BASE_DATE: i64 = 1_732_000_000;

pub struct DemoContent {
    pub folders: Vec<FolderInfo>,
    pub chats: Vec<ChatView>,
    pub messages: HashMap<ChatId, Vec<MessageView>>,
}

/// Builds the complete mock dataset. `photo_dims` comes from
/// `photo::resolve` — the message content only needs the file id
/// ([`PHOTO_FILE_ID`]) and declared dimensions; `DemoTd` (in `runtime.rs`) is
/// what joins the id to an actual path on disk, via a `FileSnapshot`.
pub fn seed(photo_dims: (u32, u32)) -> DemoContent {
    let (photo_width, photo_height) = photo_dims;

    let nova_messages = nova_messages(photo_width, photo_height);
    let ada_messages = ada_messages();
    let hiking_messages = hiking_messages();
    let release_notes_messages = release_notes_messages();
    let design_sync_messages = design_sync_messages();

    let chats = vec![
        chat(
            CHAT_NOVA,
            ChatKind::Private,
            "Nova",
            500,
            &[],
            0,
            nova_messages.last(),
        ),
        chat(
            CHAT_ADA,
            ChatKind::Private,
            "Ada Lovelace (Demo)",
            480,
            &[],
            3,
            ada_messages.last(),
        ),
        chat(
            CHAT_HIKING,
            ChatKind::Group,
            "Weekend Hiking Crew",
            460,
            &[FOLDER_FRIENDS],
            0,
            hiking_messages.last(),
        ),
        chat(
            CHAT_RELEASE_NOTES,
            ChatKind::Channel,
            "tgt Release Notes",
            440,
            &[FOLDER_WORK],
            1,
            release_notes_messages.last(),
        ),
        chat(
            CHAT_DESIGN_SYNC,
            ChatKind::Supergroup,
            "Design Sync",
            420,
            &[FOLDER_WORK],
            0,
            design_sync_messages.last(),
        ),
    ];

    let messages = HashMap::from([
        (CHAT_NOVA, nova_messages),
        (CHAT_ADA, ada_messages),
        (CHAT_HIKING, hiking_messages),
        (CHAT_RELEASE_NOTES, release_notes_messages),
        (CHAT_DESIGN_SYNC, design_sync_messages),
    ]);

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
        messages,
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
    last_message: Option<&MessageView>,
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
        last_message: last_message.map(|m| MessagePreview {
            sender_name: m.sender_name.clone(),
            text: excerpt(&m.content),
            date: m.date,
            is_outgoing: m.is_outgoing,
        }),
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

fn excerpt(content: &MessageContent) -> String {
    let text = match content {
        MessageContent::Text(t) => t.text.as_str(),
        MessageContent::Photo { caption, .. }
        | MessageContent::Video { caption, .. }
        | MessageContent::Document { caption, .. } => caption.text.as_str(),
        MessageContent::Audio { file_name, .. } => file_name.as_str(),
        MessageContent::Sticker { emoji } => emoji.as_str(),
        MessageContent::Unsupported { description } => description.as_str(),
    };
    const MAX: usize = 80;
    if text.chars().count() <= MAX {
        text.to_string()
    } else {
        text.chars().take(MAX).collect::<String>() + "…"
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

/// Chat 1: the one built to show off a reply, a reaction, an edited message,
/// a spoiler and the demo's photo in a single open-chat pass.
fn nova_messages(photo_width: u32, photo_height: u32) -> Vec<MessageView> {
    let t = BASE_DATE;

    let reply_source_text = "Yes! I'll bring the laptop.";
    let reply_source = outgoing(
        2,
        CHAT_NOVA,
        t + 60,
        MessageContent::Text(text(reply_source_text)),
    );

    let mut photo = incoming(
        5,
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
        6,
        CHAT_NOVA,
        t + 360,
        MessageContent::Text(text("Let's meet at 10:30 instead, running a bit late.")),
    );
    edited.is_edited = true;

    let mut reply = incoming(
        3,
        CHAT_NOVA,
        NOVA,
        "Nova",
        t + 130,
        MessageContent::Text(text("Perfect, see you at 10.")),
    );
    reply.reply_to = Some(ReplyPreview {
        message_id: MessageId(2),
        sender_name: "You".to_string(),
        excerpt: reply_source_text.to_string(),
    });

    vec![
        incoming(
            1,
            CHAT_NOVA,
            NOVA,
            "Nova",
            t,
            MessageContent::Text(text("Hey! Are we still on for the walkthrough tomorrow?")),
        ),
        reply_source,
        reply,
        outgoing(
            4,
            CHAT_NOVA,
            t + 250,
            MessageContent::Text(text("Also, meet the newest team member.")),
        ),
        photo,
        edited,
        incoming(
            7,
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
            8,
            CHAT_NOVA,
            NOVA,
            "Nova",
            t + 480,
            MessageContent::Text(text("🎉 Sounds good, see you then!")),
        ),
        outgoing(
            9,
            CHAT_NOVA,
            t + 540,
            MessageContent::Text(text("See you soon 👋")),
        ),
    ]
}

/// Chat 2: three unread messages, to show the sidebar badge.
fn ada_messages() -> Vec<MessageView> {
    let t = BASE_DATE + 10_000;
    vec![
        incoming(
            1,
            CHAT_ADA,
            ADA,
            "Ada Lovelace (Demo)",
            t,
            MessageContent::Text(text("Don't forget the demo starts at 3pm.")),
        ),
        incoming(
            2,
            CHAT_ADA,
            ADA,
            "Ada Lovelace (Demo)",
            t + 90,
            MessageContent::Text(text("Also I pushed the slides.")),
        ),
        incoming(
            3,
            CHAT_ADA,
            ADA,
            "Ada Lovelace (Demo)",
            t + 180,
            MessageContent::Text(text("Ping me when you're ready.")),
        ),
    ]
}

/// Chat 3: a group, two different senders — shows per-sender header colors.
fn hiking_messages() -> Vec<MessageView> {
    let t = BASE_DATE + 20_000;
    vec![
        incoming(
            1,
            CHAT_HIKING,
            SAM,
            "Sam",
            t,
            MessageContent::Text(text("Trailhead at 8am, don't be late!")),
        ),
        incoming(
            2,
            CHAT_HIKING,
            PRIYA,
            "Priya",
            t + 45,
            MessageContent::Text(text("I'll bring snacks 🥨")),
        ),
        incoming(
            3,
            CHAT_HIKING,
            SAM,
            "Sam",
            t + 90,
            MessageContent::Text(text("Perfect.")),
        ),
    ]
}

/// Chat 4: a channel post, sent as the channel itself rather than a user.
fn release_notes_messages() -> Vec<MessageView> {
    let t = BASE_DATE + 30_000;
    vec![MessageView {
        id: MessageId(1),
        chat_id: CHAT_RELEASE_NOTES,
        sender: Sender::Chat(CHAT_RELEASE_NOTES),
        sender_name: "tgt Release Notes".to_string(),
        is_outgoing: false,
        date: t,
        content: MessageContent::Text(text(
            "v0.2.0-demo: inline images, folders and reactions are here 🎉",
        )),
        reply_to: None,
        send_state: SendState::Sent,
        reactions: Vec::new(),
        caps: incoming_caps(),
        is_edited: false,
    }]
}

/// Chat 5: a supergroup, two senders.
fn design_sync_messages() -> Vec<MessageView> {
    let t = BASE_DATE + 40_000;
    vec![
        incoming(
            1,
            CHAT_DESIGN_SYNC,
            LIN,
            "Lin",
            t,
            MessageContent::Text(text("Updated the mockups, check the shared link.")),
        ),
        incoming(
            2,
            CHAT_DESIGN_SYNC,
            KAI,
            "Kai",
            t + 60,
            MessageContent::Text(text("Looks great!")),
        ),
    ]
}

/// The plain text a message's content carries, for search matching and reply
/// excerpts built outside `seed()` (e.g. against a message the user just
/// sent in the live session — see `runtime::DemoTd`).
pub fn plain_text(content: &MessageContent) -> String {
    excerpt(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_chat_has_at_least_one_message() {
        let content = seed((320, 320));
        for chat in &content.chats {
            let messages = content
                .messages
                .get(&chat.id)
                .unwrap_or_else(|| panic!("chat {:?} has no seeded messages", chat.id));
            assert!(
                !messages.is_empty(),
                "chat {:?} ({}) must not open empty",
                chat.id,
                chat.title
            );
        }
    }

    #[test]
    fn nova_chat_demonstrates_every_feature() {
        let content = seed((320, 320));
        let nova = &content.messages[&CHAT_NOVA];

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
}
