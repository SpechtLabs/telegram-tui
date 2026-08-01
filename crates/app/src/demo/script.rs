//! Builds the fixed [`FakeTd`] script `tgt --demo` drives: log in already
//! done, the chat list populated, Nova's one conversation opened, then two
//! outgoing messages sent live. See `crate::demo`'s module docs for why
//! `FakeTd` (a scripted fixture) rather than a lenient in-memory runtime is
//! the right call now that the brief is one committed, regenerable
//! recording rather than a freely playable demo, and `content.rs`'s module
//! docs for why Nova's opening page is padded to a full 50 messages and the
//! photo is pre-seeded as already downloaded — both exist to keep the two
//! `GetChatHistory` `Await`s below the *only* requests that fan-out risk
//! touches, so their order is exactly `read_only.rs`'s own
//! `read_only_script` and not a race.
//!
//! Everything else the app requests while driving this flow — `OpenChat`,
//! `ViewMessages`, `CloseChat` — is fire-and-forget and answered by
//! `FakeTd`'s default `Ok` fallback; see `FakeTd`'s module docs ("Driver
//! mechanics").
//!
//! # Showing `Sending` → `Sent` without racing `FakeTd`'s own timing
//!
//! `FakeTd::request` answers an `Await` and drains any `Emit` steps that
//! immediately follow it *synchronously*, before returning (see its module
//! docs, "Driver mechanics") — there is no way to tell it "answer this, then
//! wait two seconds, then push that update". Chaining `SendMessageText`'s
//! `Await` straight to an `Emit(MessageSendSucceeded)` would therefore
//! resolve the send before the runtime loop's next draw, and the `⋯`
//! (`SendState::Sending`, see `view::conversation::receipt_marker`) would
//! never render for long enough to see, let alone record.
//!
//! So the two don't chain. This script sends **two** messages: the first
//! `Await` answers with an optimistic `Sending` message and nothing else;
//! only the *second* `SendMessageText` — which only ever arrives once the
//! recording's driver actually presses Enter again — is followed by the
//! `Emit(MessageSendSucceeded)` that resolves the first. The gap the viewer
//! sees between "⋯" and "✓" is therefore exactly the wall-clock pause the
//! driver script puts between the two sends, fully outside `FakeTd`'s
//! control and fully within the recording's.

use std::path::PathBuf;

use tgt_core::model::chat::ChatListId;
use tgt_core::model::entity::FormattedText;
use tgt_core::model::ids::MessageId;
use tgt_core::model::message::{FileSnapshot, MessageContent, MessageView, SendState, Sender};
use tgt_core::td::fake::{FakeTd, RequestMatcher, RespondWith, ScriptStep};
use tgt_core::td::request::{TdRequest, TdResponse};
use tgt_core::td::update::{AuthPhase, ConnectionPhase, TdUpdate};

use super::content::{self, CHAT_NOVA, PHOTO_FILE_ID};

/// Exact text of the two live-sent messages — `composer::submit` never
/// attaches entities on its own (plain `FormattedText { entities: vec![] }`
/// every time), so these double as the exact match key `RequestMatcher::
/// Exact` needs and the strings `scripts/record-demo.sh` types.
pub const SEND_1_TEXT: &str = "Recording this for the docs — thanks for reviewing! 🎬";
pub const SEND_2_TEXT: &str = "Back in a bit 👋";

/// Builds the fixture and returns a ready-to-drive `FakeTd`. `photo_path` is
/// where the demo's one photo lives on disk (`photo::resolve`'s result) —
/// read here only for its size and pixel dimensions, matching how
/// `main.rs::TdlibRuntime` never inspects a file's *content* to answer a
/// request about it either.
pub fn build(photo_path: PathBuf) -> FakeTd {
    let dims = image::image_dimensions(&photo_path).unwrap_or((320, 320));
    let bytes = std::fs::metadata(&photo_path).map(|m| m.len()).unwrap_or(0);
    let text = jsonl(photo_path, dims, bytes);
    FakeTd::from_jsonl(&text).expect("demo::script's steps always serialize to valid jsonl")
}

fn jsonl(photo_path: PathBuf, photo_dims: (u32, u32), photo_bytes: u64) -> String {
    steps(photo_path, photo_dims, photo_bytes)
        .iter()
        .map(|s| serde_json::to_string(s).expect("ScriptStep serializes"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn steps(photo_path: PathBuf, photo_dims: (u32, u32), photo_bytes: u64) -> Vec<ScriptStep> {
    let content = content::seed(photo_dims);
    // Captured before `content.nova_history` is moved out below (its last
    // entry's date, so the two live sends land chronologically after it).
    let last_date = content
        .nova_history
        .last()
        .expect("nova_history is never empty")
        .date;

    let mut steps = vec![
        // Logged in already: TDLib restores an authorized session from its
        // database, so `Ready` is the first update and no credentials round
        // trip happens — mirrors `read_only.rs`'s `ready_and_load_chats`.
        ScriptStep::Emit(TdUpdate::Connection(ConnectionPhase::Ready)),
        ScriptStep::Emit(TdUpdate::Auth(AuthPhase::Ready)),
        ScriptStep::Await {
            expect: RequestMatcher::Exact(TdRequest::LoadChats {
                list: ChatListId::Main,
                limit: 200,
            }),
            respond: RespondWith::Ok(TdResponse::Ok),
        },
        ScriptStep::Emit(TdUpdate::ChatFolders(content.folders.clone())),
    ];

    for chat in &content.chats {
        steps.push(ScriptStep::Emit(TdUpdate::NewChat(chat.clone())));
    }

    // The photo, already "downloaded". `media::should_auto_request` sees a
    // completed `FileSnapshot` and never issues `DownloadFile` at all, which
    // is what keeps that request out of this script entirely rather than
    // racing it against the reconcile below (content.rs's module docs).
    steps.push(ScriptStep::Emit(TdUpdate::File(FileSnapshot {
        id: PHOTO_FILE_ID,
        expected_size: photo_bytes,
        downloaded_size: photo_bytes,
        uploaded_size: 0,
        is_downloading: false,
        is_completed: true,
        local_path: Some(photo_path),
    })));

    // Opening Nova (T59: local-first, `only_local: true`). A full
    // `PAGE_SIZE` page, so T67's viewport-fill has nothing to do — the
    // reconcile below is the only other request this flow produces.
    steps.push(ScriptStep::Await {
        expect: RequestMatcher::Exact(TdRequest::GetChatHistory {
            chat_id: CHAT_NOVA,
            from_message_id: MessageId(0),
            limit: 50,
            only_local: true,
        }),
        respond: RespondWith::Ok(TdResponse::Messages {
            messages: content.nova_history.clone(),
        }),
    });
    // T59's automatic remote reconcile: answered with the same page again,
    // a no-op — matching `read_only.rs`'s `read_only_script` exactly.
    steps.push(ScriptStep::Await {
        expect: RequestMatcher::Exact(TdRequest::GetChatHistory {
            chat_id: CHAT_NOVA,
            from_message_id: MessageId(0),
            limit: 50,
            only_local: false,
        }),
        respond: RespondWith::Ok(TdResponse::Messages {
            messages: content.nova_history,
        }),
    });

    // The first live send: answered with an optimistic `Sending` message
    // and nothing else — see the module docs' "Showing Sending -> Sent"
    // section for why its confirmation is deliberately NOT chained here.
    //
    // Temp ids deliberately are NOT the usual small-negative placeholder
    // (dispatch.rs's own tests use `MessageId(-1)`, and that's fine there:
    // an isolated conversation with nothing else loaded). Here Nova's
    // history is real, ascending ids 1..=50, and `state::conversation::
    // append_new_message`'s insert is order-sensitive: `msg.id > last.id`
    // takes the fast "definitely newest" path straight to `push_back`;
    // anything else is treated as a genuinely out-of-order arrival and
    // binary-searched into position by id. A temp id of -1 would sort
    // *before* message 1 — into the very front of the window, invisible
    // until scrolled all the way up, which is exactly the bug this comment
    // exists to keep someone from reintroducing. Temp ids here are chosen
    // comfortably above 50 so they always take the `push_back` path.
    const SEND_1_TEMP_ID: MessageId = MessageId(1000);
    const SEND_1_FINAL_ID: MessageId = MessageId(51);
    steps.push(ScriptStep::Await {
        expect: RequestMatcher::Exact(send_1_request()),
        respond: RespondWith::Ok(TdResponse::Message(outgoing_message(
            SEND_1_TEMP_ID,
            last_date + 60,
            SendState::Sending,
            SEND_1_TEXT,
        ))),
    });

    // The second live send: answered the same way, but immediately followed
    // by the `Emit` that resolves the *first* send — see the module docs.
    // The driver script controls the gap between the two by controlling
    // when this request actually arrives. Also above `SEND_1_TEMP_ID`, for
    // the same ordering reason (message #1 is still the newest loaded
    // message, temp id and all, at the moment #2 is optimistically appended).
    const SEND_2_TEMP_ID: MessageId = MessageId(1001);
    steps.push(ScriptStep::Await {
        expect: RequestMatcher::Exact(send_2_request()),
        respond: RespondWith::Ok(TdResponse::Message(outgoing_message(
            SEND_2_TEMP_ID,
            last_date + 120,
            SendState::Sending,
            SEND_2_TEXT,
        ))),
    });
    steps.push(ScriptStep::Emit(TdUpdate::MessageSendSucceeded {
        chat_id: CHAT_NOVA,
        old_message_id: SEND_1_TEMP_ID,
        message: outgoing_message(
            SEND_1_FINAL_ID,
            last_date + 60,
            SendState::Sent,
            SEND_1_TEXT,
        ),
    }));

    steps
}

fn send_1_request() -> TdRequest {
    TdRequest::SendMessageText {
        chat_id: CHAT_NOVA,
        reply_to: None,
        text: plain(SEND_1_TEXT),
    }
}

fn send_2_request() -> TdRequest {
    TdRequest::SendMessageText {
        chat_id: CHAT_NOVA,
        reply_to: None,
        text: plain(SEND_2_TEXT),
    }
}

fn plain(text: &str) -> FormattedText {
    FormattedText {
        text: text.to_string(),
        entities: Vec::new(),
    }
}

fn outgoing_message(id: MessageId, date: i64, send_state: SendState, text: &str) -> MessageView {
    MessageView {
        id,
        chat_id: CHAT_NOVA,
        sender: Sender::User(content::YOU),
        sender_name: "You".to_string(),
        is_outgoing: true,
        date,
        content: MessageContent::Text(plain(text)),
        reply_to: None,
        send_state,
        reactions: Vec::new(),
        caps: content::outgoing_caps(),
        is_edited: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tgt_core::td::runtime::TdRuntime;

    fn photo() -> PathBuf {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cat.png");
        std::mem::forget(dir); // outlive this function; short-lived test process
        super::super::photo::write_placeholder(&path).expect("placeholder should render");
        path
    }

    #[test]
    fn builds_without_panicking() {
        // Constructing it already proves every step round-trips through
        // `from_jsonl`; driving it end to end needs the real `Core`/`App`,
        // which this crate-internal module can't build standalone without
        // `#[path]`-including half of `main.rs` — covered instead by
        // actually running `tgt --demo` (see `crate::demo`'s module docs).
        let _fake = build(photo());
    }

    #[tokio::test]
    async fn drives_load_chats_then_both_history_requests_in_order() {
        let fake = build(photo());
        let mut updates = fake.updates();

        assert_eq!(
            updates.recv().await.unwrap(),
            TdUpdate::Connection(ConnectionPhase::Ready)
        );
        assert_eq!(
            updates.recv().await.unwrap(),
            TdUpdate::Auth(AuthPhase::Ready)
        );
        // The `Await` on `LoadChats` blocks the rest of the leading `Emit`
        // burst until it's answered (see `FakeTd`'s "Driver mechanics").
        assert!(updates.try_recv().is_err());

        let resp = fake
            .request(TdRequest::LoadChats {
                list: ChatListId::Main,
                limit: 200,
            })
            .await
            .unwrap();
        assert_eq!(resp, TdResponse::Ok);

        assert!(matches!(
            updates.recv().await.unwrap(),
            TdUpdate::ChatFolders(folders) if folders.len() == 2
        ));
        for _ in 0..5 {
            assert!(matches!(
                updates.recv().await.unwrap(),
                TdUpdate::NewChat(_)
            ));
        }
        assert!(matches!(updates.recv().await.unwrap(), TdUpdate::File(_)));

        let local = fake
            .request(TdRequest::GetChatHistory {
                chat_id: CHAT_NOVA,
                from_message_id: MessageId(0),
                limit: 50,
                only_local: true,
            })
            .await
            .unwrap();

        let reconcile = fake
            .request(TdRequest::GetChatHistory {
                chat_id: CHAT_NOVA,
                from_message_id: MessageId(0),
                limit: 50,
                only_local: false,
            })
            .await
            .unwrap();
        // Same full page both times — a no-op reconcile.
        assert_eq!(reconcile, local);

        let TdResponse::Messages { messages } = local else {
            panic!("expected Messages");
        };
        assert_eq!(messages.len(), 50);
    }

    /// The mechanism the module docs' "Showing Sending -> Sent" section
    /// describes: the first send's confirmation must not arrive until the
    /// *second* send is actually requested, however long that takes — never
    /// bundled into the first response.
    #[tokio::test]
    async fn send_flow_holds_sending_until_the_second_message_goes_out() {
        let fake = build(photo());
        let mut updates = fake.updates();
        // Connection, Auth: emitted immediately, before LoadChats is answered.
        updates.recv().await.unwrap();
        updates.recv().await.unwrap();
        fake.request(TdRequest::LoadChats {
            list: ChatListId::Main,
            limit: 200,
        })
        .await
        .unwrap();
        // ChatFolders, 5x NewChat, File: released once LoadChats is answered.
        for _ in 0..7 {
            updates.recv().await.unwrap();
        }
        fake.request(TdRequest::GetChatHistory {
            chat_id: CHAT_NOVA,
            from_message_id: MessageId(0),
            limit: 50,
            only_local: true,
        })
        .await
        .unwrap();
        fake.request(TdRequest::GetChatHistory {
            chat_id: CHAT_NOVA,
            from_message_id: MessageId(0),
            limit: 50,
            only_local: false,
        })
        .await
        .unwrap();

        let first = fake.request(send_1_request()).await.unwrap();
        let TdResponse::Message(first_msg) = first else {
            panic!("expected Message");
        };
        assert_eq!(first_msg.send_state, SendState::Sending);
        assert!(
            first_msg.id > MessageId(50),
            "see send_temp_ids_sort_after_every_seeded_history_message"
        );

        // Nothing resolves the first send until the second is requested —
        // proven here by there being nothing to receive yet.
        assert!(updates.try_recv().is_err());

        let second = fake.request(send_2_request()).await.unwrap();
        let TdResponse::Message(second_msg) = second else {
            panic!("expected Message");
        };
        assert_eq!(second_msg.send_state, SendState::Sending);

        let resolved = updates.recv().await.unwrap();
        let TdUpdate::MessageSendSucceeded {
            old_message_id,
            message,
            ..
        } = resolved
        else {
            panic!("expected MessageSendSucceeded, got {resolved:?}");
        };
        assert_eq!(old_message_id, first_msg.id);
        assert_eq!(message.send_state, SendState::Sent);
        assert_eq!(message.content, first_msg.content);
    }

    /// Pins the bug the `SEND_1_TEMP_ID`/`SEND_2_TEMP_ID` doc comments warn
    /// about: `state::conversation::append_new_message` only takes the
    /// definitely-newest `push_back` path when `msg.id > last.id`; anything
    /// else is treated as an out-of-order arrival and binary-searched into
    /// position. A temp id that doesn't sort above every id in
    /// `nova_history` (50 of them, ascending) lands invisibly at the front
    /// of the window instead of the visible bottom — verified live against
    /// the real running app before this test existed (a small negative id,
    /// the usual placeholder convention elsewhere in this codebase, put the
    /// "just sent" message at the top of a freshly opened chat, off-screen).
    #[tokio::test]
    async fn send_temp_ids_sort_after_every_seeded_history_message() {
        let fake = build(photo());
        let mut updates = fake.updates();
        updates.recv().await.unwrap(); // Connection
        updates.recv().await.unwrap(); // Auth
        fake.request(TdRequest::LoadChats {
            list: ChatListId::Main,
            limit: 200,
        })
        .await
        .unwrap();
        for _ in 0..7 {
            updates.recv().await.unwrap(); // ChatFolders, 5x NewChat, File
        }
        fake.request(TdRequest::GetChatHistory {
            chat_id: CHAT_NOVA,
            from_message_id: MessageId(0),
            limit: 50,
            only_local: true,
        })
        .await
        .unwrap();
        fake.request(TdRequest::GetChatHistory {
            chat_id: CHAT_NOVA,
            from_message_id: MessageId(0),
            limit: 50,
            only_local: false,
        })
        .await
        .unwrap();

        let TdResponse::Message(first) = fake.request(send_1_request()).await.unwrap() else {
            panic!("expected Message");
        };
        assert!(
            first.id > MessageId(50),
            "send #1's temp id {:?} must sort after nova_history's newest \
             (id 50), or it lands at the front of the window instead of the \
             visible bottom",
            first.id
        );

        let TdResponse::Message(second) = fake.request(send_2_request()).await.unwrap() else {
            panic!("expected Message");
        };
        assert!(
            second.id > first.id,
            "send #2's temp id {:?} must sort after send #1's ({:?}), which \
             is still the newest loaded message at the moment #2 is \
             optimistically appended",
            second.id,
            first.id
        );
    }

    #[test]
    fn unscripted_requests_get_the_safe_default_ok() {
        let fake = build(photo());
        let _updates = fake.updates();
        // Not `#[tokio::test]`-async here: `FakeTd::request` needs no real
        // await for a request it isn't scripted to match, but `Runtime`
        // requires an executor to call it from.
        let rt = tokio::runtime::Runtime::new().unwrap();
        let resp = rt
            .block_on(fake.request(TdRequest::OpenChat { chat_id: CHAT_NOVA }))
            .unwrap();
        assert_eq!(resp, TdResponse::Ok);
    }
}
