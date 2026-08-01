//! `DemoTd`: an in-memory, offline [`TdRuntime`] for `tgt --demo`. See the
//! parent module (`crate::demo`) docs for why this exists and what keeps it
//! from ever touching a real account.
//!
//! # Why a lenient runtime rather than a `FakeTd` fixture
//!
//! `crate::td::fake::FakeTd` (reused unmodified by the integration test
//! suite) is script-strict: it expects requests in a fixed order and answers
//! the scripted one, defaulting everything else to a bare `Ok`. That is
//! right for a test that is asserting on a specific sequence of effects, and
//! wrong for a demo: a person driving this live (or a recording script that
//! opens chats out of order, scrolls further than planned, or types
//! something unscripted) would diverge from any fixed script and the
//! session would stall or silently no-op the moment it did. This module
//! instead holds the mock chats and messages as live, mutable state and
//! answers *any* request that names something the mock data has — open any
//! chat, page any history, send anything, react, delete, edit, search. A
//! demo built this way is re-recordable and interactively drivable rather
//! than a single fragile take.
//!
//! # Send flow
//!
//! `SendMessageText`/`SendMessageFile` mirror TDLib's real two-phase shape
//! (architecture §5.2) so the composer's optimistic-append/confirm path
//! actually exercises: the response is an optimistic [`MessageView`] with a
//! temporary (negative) id and [`SendState::Sending`], and — after a short
//! delay, so "Sending" is visible for a beat rather than flashing past —
//! [`TdUpdate::MessageSendSucceeded`] arrives with the final id and
//! [`SendState::Sent`]. The delay is a plain `tokio::spawn` + `sleep` holding
//! only a cloned update-channel sender and already-owned data — never `self`
//! — so it needs no lifetime tricks to outlive the `request()` call that
//! started it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use tokio::sync::mpsc;

use tgt_core::model::entity::FormattedText;
use tgt_core::model::ids::{ChatId, FileId, MessageId};
use tgt_core::model::message::{
    FileSnapshot, MessageCaps, MessageContent, MessageView, ReactionView, ReplyPreview, SendState,
    Sender,
};
use tgt_core::td::error::TdError;
use tgt_core::td::request::{OutgoingFileKind, TdRequest, TdResponse};
use tgt_core::td::runtime::TdRuntime;
use tgt_core::td::update::{AuthPhase, ConnectionPhase, TdUpdate};

use super::content::{self, YOU};

/// Same rationale as `FakeTd`'s: generous enough that the leading burst of
/// construction-time updates (folders, chats) never has to block.
const CHANNEL_CAPACITY: usize = 1024;
/// How long an outgoing message sits in `SendState::Sending` before the
/// confirming `MessageSendSucceeded` arrives — long enough to see on
/// screen, short enough not to make a demo feel laggy.
const SEND_CONFIRM_DELAY: Duration = Duration::from_millis(350);

struct State {
    /// Ascending by id within each chat, matching every other window
    /// invariant in this codebase (`FakeTd`'s fixtures, `state::history`).
    messages: HashMap<ChatId, Vec<MessageView>>,
    files: HashMap<FileId, FileSnapshot>,
    next_temp_id: i64,
    next_message_id: HashMap<ChatId, i64>,
    next_file_id: i32,
}

pub struct DemoTd {
    tx: mpsc::Sender<TdUpdate>,
    rx: Mutex<Option<mpsc::Receiver<TdUpdate>>>,
    state: Mutex<State>,
}

impl DemoTd {
    /// `photo_path` is where the demo's one photo (chat 1) lives on disk —
    /// either the placeholder `photo::resolve` drew or a real file supplied
    /// via `TGT_DEMO_PHOTO`. Never read here beyond its size; rendering
    /// resolves the path itself once it reaches `AppState.media.files` in a
    /// `FileSnapshot`.
    pub fn new(photo_path: PathBuf) -> Self {
        let (photo_width, photo_height) =
            image::image_dimensions(&photo_path).unwrap_or((320, 320));
        let content = content::seed((photo_width, photo_height));
        let photo_bytes = std::fs::metadata(&photo_path).map(|m| m.len()).unwrap_or(0);

        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);

        // Pushed immediately at construction, exactly like `FakeTd`'s
        // leading `Emit` steps (see that module's docs, "Driver mechanics")
        // — the runtime loop starts reading `updates()` right after this
        // returns. `Ready` skips the whole auth wizard: `main.rs`/`demo/
        // mod.rs` boots straight to `Screen::Auth` with `AuthPhase::
        // WaitTdlibParameters` (`App::new`'s ordinary default), and this is
        // what carries it the rest of the way to `Screen::Main` — the same
        // transition a session restored from TDLib's own on-disk database
        // takes on a real login (see `state::auth::handle_td`'s `Ready` arm,
        // and the `read_only.rs` integration test's `ready_and_load_chats`
        // fixture, which this mirrors).
        let _ = tx.try_send(TdUpdate::Connection(ConnectionPhase::Ready));
        let _ = tx.try_send(TdUpdate::Auth(AuthPhase::Ready));
        let _ = tx.try_send(TdUpdate::ChatFolders(content.folders.clone()));
        for chat in &content.chats {
            let _ = tx.try_send(TdUpdate::NewChat(chat.clone()));
        }

        let mut next_message_id = HashMap::new();
        for (chat_id, messages) in &content.messages {
            let next = messages.iter().map(|m| m.id.0).max().unwrap_or(0) + 1;
            next_message_id.insert(*chat_id, next);
        }

        let mut files = HashMap::new();
        files.insert(
            content::PHOTO_FILE_ID,
            FileSnapshot {
                id: content::PHOTO_FILE_ID,
                expected_size: photo_bytes,
                downloaded_size: 0,
                uploaded_size: 0,
                is_downloading: false,
                is_completed: false,
                local_path: Some(photo_path),
            },
        );

        DemoTd {
            tx,
            rx: Mutex::new(Some(rx)),
            state: Mutex::new(State {
                messages: content.messages,
                files,
                next_temp_id: -1,
                next_message_id,
                next_file_id: content::PHOTO_FILE_ID.0 + 1,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn next_temp_id(&self) -> MessageId {
        let mut state = self.lock();
        state.next_temp_id -= 1;
        MessageId(state.next_temp_id)
    }

    fn next_message_id(&self, chat_id: ChatId) -> MessageId {
        let mut state = self.lock();
        let counter = state.next_message_id.entry(chat_id).or_insert(1);
        let id = *counter;
        *counter += 1;
        MessageId(id)
    }

    fn next_file_id(&self) -> FileId {
        let mut state = self.lock();
        let id = state.next_file_id;
        state.next_file_id += 1;
        FileId(id)
    }

    /// Messages older than `from_message_id` (TDLib's sentinel `0` means
    /// "newest"), newest-`limit`-first the way `getChatHistory` answers —
    /// see `crates/app/tests/fixtures/read_only.jsonl` for the shape this
    /// mirrors. Requests for a chat the mock data doesn't know about, or a
    /// page past the oldest loaded message, come back empty; the paging
    /// state machine (`state::history`) treats that as ordinary
    /// end-of-history, not an error.
    fn history_page(
        &self,
        chat_id: ChatId,
        from_message_id: MessageId,
        limit: u8,
    ) -> Vec<MessageView> {
        let state = self.lock();
        let Some(all) = state.messages.get(&chat_id) else {
            return Vec::new();
        };
        let limit = limit as usize;
        let end = if from_message_id == MessageId(0) {
            all.len()
        } else {
            all.partition_point(|m| m.id.0 < from_message_id.0)
        };
        let start = end.saturating_sub(limit);
        all[start..end].to_vec()
    }

    fn caps_for(&self, chat_id: ChatId, message_id: MessageId) -> MessageCaps {
        let state = self.lock();
        let is_outgoing = state
            .messages
            .get(&chat_id)
            .and_then(|msgs| msgs.iter().find(|m| m.id == message_id))
            .map(|m| m.is_outgoing)
            .unwrap_or(false);
        if is_outgoing {
            content::outgoing_caps()
        } else {
            MessageCaps {
                can_be_edited: false,
                can_be_deleted_for_all_users: false,
                can_be_deleted_only_for_self: true,
                can_be_forwarded: true,
                can_be_saved: true,
            }
        }
    }

    fn reply_preview(&self, chat_id: ChatId, message_id: MessageId) -> Option<ReplyPreview> {
        let state = self.lock();
        let m = state
            .messages
            .get(&chat_id)?
            .iter()
            .find(|m| m.id == message_id)?;
        Some(ReplyPreview {
            message_id,
            sender_name: m.sender_name.clone(),
            excerpt: content::plain_text(&m.content),
        })
    }

    /// Builds the optimistic reply, records the eventual "sent" message into
    /// the mock store immediately (so a follow-up `GetChatHistory` or search
    /// sees it right away) and schedules the confirming push. Shared by both
    /// `SendMessageText` and `SendMessageFile`.
    fn send(
        &self,
        chat_id: ChatId,
        reply_to: Option<ReplyPreview>,
        content: MessageContent,
    ) -> TdResponse {
        let temp_id = self.next_temp_id();
        let date = now_unix();
        let optimistic = MessageView {
            id: temp_id,
            chat_id,
            sender: Sender::User(YOU),
            sender_name: "You".to_string(),
            is_outgoing: true,
            date,
            content: content.clone(),
            reply_to: reply_to.clone(),
            send_state: SendState::Sending,
            reactions: Vec::new(),
            caps: content::outgoing_caps(),
            is_edited: false,
        };

        let final_id = self.next_message_id(chat_id);
        let final_view = MessageView {
            id: final_id,
            send_state: SendState::Sent,
            ..optimistic.clone()
        };
        self.lock()
            .messages
            .entry(chat_id)
            .or_default()
            .push(final_view.clone());

        let tx = self.tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(SEND_CONFIRM_DELAY).await;
            let _ = tx
                .send(TdUpdate::MessageSendSucceeded {
                    chat_id,
                    old_message_id: temp_id,
                    message: final_view,
                })
                .await;
        });

        TdResponse::Message(optimistic)
    }

    fn send_file(
        &self,
        chat_id: ChatId,
        path: PathBuf,
        kind: OutgoingFileKind,
        caption: Option<FormattedText>,
    ) -> TdResponse {
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".to_string());
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let file_id = self.next_file_id();
        let caption = caption.unwrap_or(FormattedText {
            text: String::new(),
            entities: Vec::new(),
        });

        let content = match kind {
            OutgoingFileKind::Photo => {
                let (width, height) = image::image_dimensions(&path).unwrap_or((0, 0));
                MessageContent::Photo {
                    file_id,
                    width,
                    height,
                    caption,
                }
            }
            OutgoingFileKind::Video => MessageContent::Video {
                file_id,
                file_name,
                size,
                duration_secs: 0,
                caption,
            },
            OutgoingFileKind::Audio => MessageContent::Audio {
                file_id,
                file_name,
                size,
                duration_secs: 0,
            },
            OutgoingFileKind::Document => MessageContent::Document {
                file_id,
                file_name,
                size,
                caption,
            },
        };

        // The sender already has this file on disk — it is what got
        // "uploaded" — so it renders immediately rather than sitting on a
        // Download affordance for a file that never needs fetching. Mirrors
        // what TDLib's own upload flow reports back as the send goes out.
        self.lock().files.insert(
            file_id,
            FileSnapshot {
                id: file_id,
                expected_size: size,
                downloaded_size: size,
                uploaded_size: size,
                is_downloading: false,
                is_completed: true,
                local_path: Some(path),
            },
        );

        self.send(chat_id, None, content)
    }

    fn edit_text(&self, chat_id: ChatId, message_id: MessageId, text: FormattedText) {
        let mut state = self.lock();
        if let Some(m) = state
            .messages
            .get_mut(&chat_id)
            .and_then(|msgs| msgs.iter_mut().find(|m| m.id == message_id))
        {
            m.content = MessageContent::Text(text);
        }
    }

    fn delete(&self, chat_id: ChatId, message_ids: &[MessageId]) {
        let mut state = self.lock();
        if let Some(msgs) = state.messages.get_mut(&chat_id) {
            msgs.retain(|m| !message_ids.contains(&m.id));
        }
    }

    /// Toggles `emoji` on `message_id` for "me" — adds a fresh reaction,
    /// increments/flips an existing one's `chosen_by_me`, or removes it once
    /// its count reaches zero. Returns the message's full reaction list
    /// afterward, or `None` if the message doesn't exist.
    fn toggle_reaction(
        &self,
        chat_id: ChatId,
        message_id: MessageId,
        emoji: &str,
    ) -> Option<Vec<ReactionView>> {
        let mut state = self.lock();
        let m = state
            .messages
            .get_mut(&chat_id)?
            .iter_mut()
            .find(|m| m.id == message_id)?;

        match m.reactions.iter_mut().find(|r| r.emoji == emoji) {
            Some(existing) if existing.chosen_by_me => {
                existing.chosen_by_me = false;
                existing.count = existing.count.saturating_sub(1);
                if existing.count == 0 {
                    m.reactions.retain(|r| r.emoji != emoji);
                }
            }
            Some(existing) => existing.chosen_by_me = true,
            None => m.reactions.push(ReactionView {
                emoji: emoji.to_string(),
                count: 1,
                chosen_by_me: true,
            }),
        }
        Some(m.reactions.clone())
    }

    fn search(
        &self,
        chat_id: ChatId,
        query: &str,
        from_message_id: MessageId,
        limit: u8,
    ) -> Vec<MessageId> {
        let state = self.lock();
        let Some(msgs) = state.messages.get(&chat_id) else {
            return Vec::new();
        };
        let needle = query.to_lowercase();
        msgs.iter()
            .rev()
            .filter(|m| from_message_id == MessageId(0) || m.id.0 < from_message_id.0)
            .filter(|m| {
                content::plain_text(&m.content)
                    .to_lowercase()
                    .contains(&needle)
            })
            .take(limit as usize)
            .map(|m| m.id)
            .collect()
    }

    fn download(&self, file_id: FileId) -> TdResponse {
        let mut state = self.lock();
        let Some(snapshot) = state.files.get_mut(&file_id) else {
            // Unknown file id: nothing this demo can serve. An `Ok` that
            // leaves `is_completed: false` keeps the card on its Download
            // affordance rather than failing the request outright.
            return TdResponse::File(FileSnapshot {
                id: file_id,
                expected_size: 0,
                downloaded_size: 0,
                uploaded_size: 0,
                is_downloading: false,
                is_completed: false,
                local_path: None,
            });
        };
        snapshot.is_downloading = false;
        snapshot.is_completed = true;
        snapshot.downloaded_size = snapshot.expected_size;
        TdResponse::File(snapshot.clone())
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[async_trait]
impl TdRuntime for DemoTd {
    async fn request(&self, req: TdRequest) -> Result<TdResponse, TdError> {
        let response = match req {
            // Never actually reached — this runtime skips the whole auth
            // wizard by emitting `Auth(Ready)` at construction — but answered
            // plausibly rather than left unmatched, in case anything ever
            // drives the auth screen against `DemoTd` directly.
            TdRequest::SetTdlibParameters(_)
            | TdRequest::SetAuthenticationPhoneNumber { .. }
            | TdRequest::CheckAuthenticationCode { .. }
            | TdRequest::CheckAuthenticationPassword { .. }
            | TdRequest::RequestQrCodeAuthentication
            | TdRequest::LogOut
            | TdRequest::LoadChats { .. }
            | TdRequest::OpenChat { .. }
            | TdRequest::CloseChat { .. }
            | TdRequest::ViewMessages { .. }
            | TdRequest::CancelDownloadFile { .. }
            // Forwarding is a no-op here rather than fabricating a message
            // in a chat the demo script may not expect it in.
            | TdRequest::ForwardMessages { .. } => TdResponse::Ok,

            TdRequest::GetChatHistory {
                chat_id,
                from_message_id,
                limit,
                ..
            } => TdResponse::Messages {
                messages: self.history_page(chat_id, from_message_id, limit),
            },

            TdRequest::GetMessageProperties {
                chat_id,
                message_id,
            } => TdResponse::MessageProperties(self.caps_for(chat_id, message_id)),

            TdRequest::SendMessageText {
                chat_id,
                reply_to,
                text,
            } => {
                let reply = reply_to.and_then(|id| self.reply_preview(chat_id, id));
                self.send(chat_id, reply, MessageContent::Text(text))
            }
            TdRequest::SendMessageFile {
                chat_id,
                path,
                kind,
                caption,
            } => self.send_file(chat_id, path, kind, caption),

            TdRequest::EditMessageText {
                chat_id,
                message_id,
                text,
            } => {
                self.edit_text(chat_id, message_id, text.clone());
                let _ = self.tx.try_send(TdUpdate::MessageContentChanged {
                    chat_id,
                    message_id,
                    content: MessageContent::Text(text),
                });
                TdResponse::Ok
            }
            TdRequest::DeleteMessages {
                chat_id,
                message_ids,
                ..
            } => {
                self.delete(chat_id, &message_ids);
                let _ = self
                    .tx
                    .try_send(TdUpdate::MessagesDeleted { chat_id, message_ids });
                TdResponse::Ok
            }
            TdRequest::ToggleReaction {
                chat_id,
                message_id,
                emoji,
            } => {
                if let Some(reactions) = self.toggle_reaction(chat_id, message_id, &emoji) {
                    let _ = self.tx.try_send(TdUpdate::MessageInteractionInfo {
                        chat_id,
                        message_id,
                        reactions,
                    });
                }
                TdResponse::Ok
            }
            TdRequest::DownloadFile { file_id, .. } => self.download(file_id),
            TdRequest::SearchChatMessages {
                chat_id,
                query,
                from_message_id,
                limit,
            } => TdResponse::FoundMessages {
                message_ids: self.search(chat_id, &query, from_message_id, limit),
            },
        };
        Ok(response)
    }

    /// Called exactly once by the runtime loop at boot; panics on a second
    /// call, matching `FakeTd` and the trait's documented contract.
    fn updates(&self) -> mpsc::Receiver<TdUpdate> {
        self.rx
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
            .expect("DemoTd::updates() called twice")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photo_path() -> PathBuf {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cat.png");
        // Leak the tempdir so the file outlives this function — fine in a
        // short-lived test process.
        std::mem::forget(dir);
        super::super::photo::write_placeholder(&path).expect("placeholder should render");
        path
    }

    #[tokio::test]
    async fn boots_straight_to_ready_with_chats_and_folders() {
        let td = DemoTd::new(photo_path());
        let mut updates = td.updates();

        assert_eq!(
            updates.recv().await.unwrap(),
            TdUpdate::Connection(ConnectionPhase::Ready)
        );
        assert_eq!(
            updates.recv().await.unwrap(),
            TdUpdate::Auth(AuthPhase::Ready)
        );
        assert!(matches!(
            updates.recv().await.unwrap(),
            TdUpdate::ChatFolders(folders) if folders.len() == 2
        ));

        let mut chat_ids = Vec::new();
        for _ in 0..5 {
            if let TdUpdate::NewChat(chat) = updates.recv().await.unwrap() {
                chat_ids.push(chat.id);
            } else {
                panic!("expected five NewChat updates");
            }
        }
        assert_eq!(chat_ids.len(), 5);
    }

    #[tokio::test]
    async fn get_chat_history_returns_newest_first_page_then_pages_backward() {
        let td = DemoTd::new(photo_path());
        let _updates = td.updates();

        let newest = td
            .request(TdRequest::GetChatHistory {
                chat_id: ChatId(1),
                from_message_id: MessageId(0),
                limit: 50,
                only_local: true,
            })
            .await
            .unwrap();
        let TdResponse::Messages { messages } = newest else {
            panic!("expected Messages");
        };
        assert_eq!(messages.len(), 9, "chat 1 seeds nine messages");
        assert_eq!(messages.first().unwrap().id, MessageId(1));
        assert_eq!(messages.last().unwrap().id, MessageId(9));

        let older = td
            .request(TdRequest::GetChatHistory {
                chat_id: ChatId(1),
                from_message_id: MessageId(1),
                limit: 50,
                only_local: false,
            })
            .await
            .unwrap();
        assert_eq!(
            older,
            TdResponse::Messages {
                messages: Vec::new()
            }
        );
    }

    #[tokio::test]
    async fn unknown_chat_history_is_empty_not_an_error() {
        let td = DemoTd::new(photo_path());
        let _updates = td.updates();

        let resp = td
            .request(TdRequest::GetChatHistory {
                chat_id: ChatId(999),
                from_message_id: MessageId(0),
                limit: 50,
                only_local: true,
            })
            .await
            .unwrap();
        assert_eq!(
            resp,
            TdResponse::Messages {
                messages: Vec::new()
            }
        );
    }

    #[tokio::test]
    async fn download_file_completes_the_known_photo() {
        let td = DemoTd::new(photo_path());
        let _updates = td.updates();

        let resp = td
            .request(TdRequest::DownloadFile {
                file_id: content::PHOTO_FILE_ID,
                priority: 32,
            })
            .await
            .unwrap();
        let TdResponse::File(snapshot) = resp else {
            panic!("expected File");
        };
        assert!(snapshot.is_completed);
        assert!(snapshot.local_path.is_some());
    }

    #[tokio::test]
    async fn send_message_text_round_trips_through_sending_to_sent() {
        let td = DemoTd::new(photo_path());
        let mut updates = td.updates();
        // Drain the boot burst: Connection, Auth, ChatFolders, five NewChat.
        for _ in 0..8 {
            updates.recv().await.unwrap();
        }

        let resp = td
            .request(TdRequest::SendMessageText {
                chat_id: ChatId(1),
                reply_to: None,
                text: FormattedText {
                    text: "hello from the demo".to_string(),
                    entities: Vec::new(),
                },
            })
            .await
            .unwrap();
        let TdResponse::Message(optimistic) = resp else {
            panic!("expected Message");
        };
        assert_eq!(optimistic.send_state, SendState::Sending);
        assert!(optimistic.id.0 < 0, "temp ids are negative");

        let confirmed = updates.recv().await.unwrap();
        let TdUpdate::MessageSendSucceeded {
            old_message_id,
            message,
            ..
        } = confirmed
        else {
            panic!("expected MessageSendSucceeded, got {confirmed:?}");
        };
        assert_eq!(old_message_id, optimistic.id);
        assert_eq!(message.send_state, SendState::Sent);
        assert!(message.id.0 > 0);
    }

    #[tokio::test]
    async fn toggle_reaction_adds_then_removes() {
        let td = DemoTd::new(photo_path());
        let _updates = td.updates();

        let added = td
            .toggle_reaction(ChatId(1), MessageId(1), "🔥")
            .expect("message 1 exists");
        assert_eq!(
            added,
            vec![ReactionView {
                emoji: "🔥".to_string(),
                count: 1,
                chosen_by_me: true,
            }]
        );

        let removed = td
            .toggle_reaction(ChatId(1), MessageId(1), "🔥")
            .expect("message 1 exists");
        assert!(removed.is_empty());
    }

    #[tokio::test]
    async fn delete_removes_the_message_and_emits_the_update() {
        let td = DemoTd::new(photo_path());
        let mut updates = td.updates();
        for _ in 0..8 {
            updates.recv().await.unwrap();
        }

        td.request(TdRequest::DeleteMessages {
            chat_id: ChatId(1),
            message_ids: vec![MessageId(1)],
            revoke: true,
        })
        .await
        .unwrap();

        let update = updates.recv().await.unwrap();
        assert_eq!(
            update,
            TdUpdate::MessagesDeleted {
                chat_id: ChatId(1),
                message_ids: vec![MessageId(1)],
            }
        );

        let history = td
            .request(TdRequest::GetChatHistory {
                chat_id: ChatId(1),
                from_message_id: MessageId(0),
                limit: 50,
                only_local: true,
            })
            .await
            .unwrap();
        let TdResponse::Messages { messages } = history else {
            panic!("expected Messages");
        };
        assert!(messages.iter().all(|m| m.id != MessageId(1)));
    }

    #[tokio::test]
    async fn search_matches_are_case_insensitive_and_bounded_by_from_message_id() {
        let td = DemoTd::new(photo_path());
        let _updates = td.updates();

        let hits = td
            .request(TdRequest::SearchChatMessages {
                chat_id: ChatId(1),
                query: "WALKTHROUGH".to_string(),
                from_message_id: MessageId(0),
                limit: 10,
            })
            .await
            .unwrap();
        assert_eq!(
            hits,
            TdResponse::FoundMessages {
                message_ids: vec![MessageId(1)]
            }
        );
    }

    #[test]
    fn every_seeded_chat_has_a_message_store() {
        let td = DemoTd::new(photo_path());
        let state = td.lock();
        for id in [1, 2, 3, 4, 5] {
            assert!(
                state.messages.contains_key(&ChatId(id)),
                "chat {id} must have a seeded message list"
            );
        }
    }
}
