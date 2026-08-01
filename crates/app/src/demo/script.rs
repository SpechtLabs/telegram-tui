//! Builds the fixed [`FakeTd`] script `tgt --demo` drives: log in already
//! done, the chat list populated, then Nova's one conversation opened. See
//! `crate::demo`'s module docs for why `FakeTd` (a scripted fixture) rather
//! than a lenient in-memory runtime is the right call now that the brief is
//! one committed, regenerable recording rather than a freely playable demo,
//! and `content.rs`'s module docs for why Nova's opening page is padded to a
//! full 50 messages and the photo is pre-seeded as already downloaded — both
//! exist to keep this script's two `Await` steps the *only* two `TdRequest`s
//! that matter, so their order is exactly `read_only.rs`'s own
//! `read_only_script` and not a race.
//!
//! Everything else the app requests while driving this flow — `OpenChat`,
//! `ViewMessages`, `CloseChat` — is fire-and-forget and answered by
//! `FakeTd`'s default `Ok` fallback; see `FakeTd`'s module docs ("Driver
//! mechanics").

use std::path::PathBuf;

use tgt_core::model::chat::ChatListId;
use tgt_core::model::ids::MessageId;
use tgt_core::model::message::FileSnapshot;
use tgt_core::td::fake::{FakeTd, RequestMatcher, RespondWith, ScriptStep};
use tgt_core::td::request::{TdRequest, TdResponse};
use tgt_core::td::update::{AuthPhase, ConnectionPhase, TdUpdate};

use super::content::{self, CHAT_NOVA, PHOTO_FILE_ID};

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

    steps
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
