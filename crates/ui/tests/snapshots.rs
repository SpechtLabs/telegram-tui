//! Frame-snapshot suite (spec §15.3; plan.md T55): the design-regression
//! net. Every test below drives the real root entry point,
//! `tgt_ui::view(state, theme, f, cache)` — the same function `tgt-app`
//! calls every frame, dispatching on `AppState::screen` to the consent
//! screen, the auth wizard, or the two-pane/single-pane main shell (see
//! `crates/ui/src/lib.rs`) — never an individual component directly.
//!
//! Widths are drawn from spec §6.1's three reference points: 80 (below the
//! responsive breakpoint, so the single-pane stack), 100 (exactly at the
//! default `layout_breakpoint_cols`, still two-pane since the comparison is
//! `>=`), and 140 (comfortably above it). Heights are ~30 or ~40 rows.
//! Snapshot names encode `<scenario>_<width>x<height>`.
//!
//! `AppState` fixtures come from `fixtures/states.rs` (included below), a
//! small composable builder library so each test body reads as the handful
//! of steps that make its scenario, not a restatement of every field
//! `AppState` has. Every state fixes `now` (`states::NOW`) rather than
//! reading a clock, so renders are exactly reproducible.

#[path = "fixtures/states.rs"]
mod states;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tgt_core::app::AppState;
use tgt_core::model::chat::ChatListId;
use tgt_core::model::chips::Chip;
use tgt_core::model::hit::{HitTarget, ScrollArea};
use tgt_core::model::ids::{ChatId, FileId, MessageId, UserId};
use tgt_core::model::message::{ReactionView, Sender};
use tgt_core::state::auth::{AuthField, FieldError, LoginMethod};
use tgt_core::state::consent::ConsentChoice;
use tgt_core::state::conversation::Scroll;
use tgt_core::state::focus::{Focus, FocusStack};
use tgt_core::state::palette::{CommandId, PaletteItem};
use tgt_core::td::error::TdError;
use tgt_core::td::update::AuthPhase;
use tgt_ui::render::cache::LayoutCache;
use tgt_ui::render::hit::HitMap;
use tgt_ui::theme::Theme;

use states::*;

/// Drives the real root `view()` at `width`x`height` into a `TestBackend`
/// and flattens the resulting buffer into one string per row — the same
/// shape every `render_to_string` helper in `crates/ui/src/view/*.rs`'s own
/// tests uses, so a diff here reads the same way those diffs do.
fn render(width: u16, height: u16, state: &AppState) -> String {
    let theme = Theme::default_dark();
    let mut cache = LayoutCache::new();
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|f| {
            tgt_ui::view(state, &theme, f, &mut cache);
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    let mut out = String::with_capacity(buffer.content.len() + buffer.area.height as usize);
    for row in buffer.content.chunks(buffer.area.width as usize) {
        for cell in row {
            out.push_str(cell.symbol());
        }
        out.push('\n');
    }
    out
}

// --- chat list + conversation: both sides of the responsive breakpoint ---

#[test]
fn two_pane_chat_list_and_conversation_140x40() {
    let convo = conversation_with(MAIN_CHAT, sample_history(MAIN_CHAT), Scroll::Bottom);
    let state = with_open_chat(base_main_state(), MAIN_CHAT, convo);
    let state = with_focus(state, FocusStack::new(Focus::Composer));
    let state = with_size(state, 140, 40);
    insta::assert_snapshot!(render(140, 40, &state));
}

#[test]
fn two_pane_at_breakpoint_chat_list_and_conversation_100x30() {
    let convo = conversation_with(MAIN_CHAT, sample_history(MAIN_CHAT), Scroll::Bottom);
    let state = with_open_chat(base_main_state(), MAIN_CHAT, convo);
    let state = with_focus(state, FocusStack::new(Focus::Composer));
    let state = with_size(state, 100, 30);
    insta::assert_snapshot!(render(100, 30, &state));
}

#[test]
fn single_pane_stack_full_width_chat_list_80x30() {
    let state = with_size(base_main_state(), 80, 30);
    insta::assert_snapshot!(render(80, 30, &state));
}

#[test]
fn single_pane_stack_breadcrumb_and_conversation_80x40() {
    let convo = conversation_with(MAIN_CHAT, sample_history(MAIN_CHAT), Scroll::Bottom);
    let state = with_open_chat(base_main_state(), MAIN_CHAT, convo);
    let state = with_focus(state, FocusStack::new(Focus::Composer));
    let state = with_size(state, 80, 40);
    insta::assert_snapshot!(render(80, 40, &state));
}

// --- selection mode with chips (spec §6.3): replaces the hint bar --------

#[test]
fn selection_mode_chip_row_two_pane_140x40() {
    let convo = conversation_with(MAIN_CHAT, sample_history(MAIN_CHAT), Scroll::Bottom);
    let convo = with_selection(
        convo,
        MessageId(3),
        vec![Chip::Reply, Chip::Copy, Chip::Delete],
    );
    let state = with_open_chat(base_main_state(), MAIN_CHAT, convo);
    let mut focus = FocusStack::new(Focus::Composer);
    focus.push(Focus::Selection);
    let state = with_focus(state, focus);
    let state = with_size(state, 140, 40);
    insta::assert_snapshot!(render(140, 40, &state));
}

#[test]
fn selection_mode_chip_row_single_pane_80x30() {
    let convo = conversation_with(MAIN_CHAT, sample_history(MAIN_CHAT), Scroll::Bottom);
    let convo = with_selection(
        convo,
        MessageId(7),
        vec![Chip::Reply, Chip::Forward, Chip::Download, Chip::Delete],
    );
    let state = with_open_chat(base_main_state(), MAIN_CHAT, convo);
    let mut focus = FocusStack::new(Focus::Composer);
    focus.push(Focus::Selection);
    let state = with_focus(state, focus);
    let state = with_size(state, 80, 30);
    insta::assert_snapshot!(render(80, 30, &state));
}

// --- delete modal: both the with-revoke and without-revoke variants ------

#[test]
fn delete_modal_with_revoke_140x40() {
    let convo = conversation_with(MAIN_CHAT, sample_history(MAIN_CHAT), Scroll::Bottom);
    let state = with_open_chat(base_main_state(), MAIN_CHAT, convo);
    let state = with_focus(state, FocusStack::new(Focus::Composer));
    let state = with_delete_modal(state, MAIN_CHAT, MessageId(3), true, 1);
    let state = with_size(state, 140, 40);
    insta::assert_snapshot!(render(140, 40, &state));
}

#[test]
fn delete_modal_without_revoke_100x30() {
    let convo = conversation_with(MAIN_CHAT, sample_history(MAIN_CHAT), Scroll::Bottom);
    let state = with_open_chat(base_main_state(), MAIN_CHAT, convo);
    let state = with_focus(state, FocusStack::new(Focus::Composer));
    let state = with_delete_modal(state, MAIN_CHAT, MessageId(9), false, 0);
    let state = with_size(state, 100, 30);
    insta::assert_snapshot!(render(100, 30, &state));
}

// --- command palette open, with results ----------------------------------

#[test]
fn palette_query_results_with_match_highlighting_140x40() {
    let state = with_palette(
        base_main_state(),
        "al",
        2,
        vec![
            PaletteItem::Chat {
                id: MAIN_CHAT,
                score: 120,
            },
            PaletteItem::Command {
                id: CommandId::ToggleTheme,
                score: 40,
            },
        ],
        0,
    );
    let state = with_size(state, 140, 40);
    insta::assert_snapshot!(render(140, 40, &state));
}

#[test]
fn palette_empty_query_recency_then_commands_100x30() {
    let state = with_palette(
        base_main_state(),
        "",
        0,
        vec![
            PaletteItem::Chat {
                id: MAIN_CHAT,
                score: 0,
            },
            PaletteItem::Chat {
                id: ChatId(2),
                score: 0,
            },
            PaletteItem::Command {
                id: CommandId::ToggleTheme,
                score: 0,
            },
            PaletteItem::Command {
                id: CommandId::TelemetrySettings,
                score: 0,
            },
            PaletteItem::Command {
                id: CommandId::SendFile,
                score: 0,
            },
            PaletteItem::Command {
                id: CommandId::LogOut,
                score: 0,
            },
            PaletteItem::Command {
                id: CommandId::Quit,
                score: 0,
            },
        ],
        0,
    );
    let state = with_size(state, 100, 30);
    insta::assert_snapshot!(render(100, 30, &state));
}

// --- in-chat search active, with hits -------------------------------------

#[test]
fn search_active_with_hits_two_pane_140x40() {
    let convo = conversation_with(MAIN_CHAT, sample_history(MAIN_CHAT), Scroll::Bottom);
    let convo = with_search_hits(convo, vec![MessageId(1), MessageId(4), MessageId(8)]);
    let state = with_open_chat(base_main_state(), MAIN_CHAT, convo);
    let state = with_focus(state, FocusStack::new(Focus::Composer));
    let state = with_chat_search(state, "pr", 2, 0);
    let state = with_size(state, 140, 40);
    insta::assert_snapshot!(render(140, 40, &state));
}

#[test]
fn search_active_with_hits_single_pane_80x30() {
    let convo = conversation_with(MAIN_CHAT, sample_history(MAIN_CHAT), Scroll::Bottom);
    let convo = with_search_hits(convo, vec![MessageId(1), MessageId(4), MessageId(8)]);
    let state = with_open_chat(base_main_state(), MAIN_CHAT, convo);
    let state = with_focus(state, FocusStack::new(Focus::Composer));
    let state = with_chat_search(state, "pr", 2, 1);
    let state = with_size(state, 80, 30);
    insta::assert_snapshot!(render(80, 30, &state));
}

// --- toasts visible, over both the two-pane and single-pane arrangements -

#[test]
fn toast_stack_over_two_pane_140x40() {
    let convo = conversation_with(MAIN_CHAT, sample_history(MAIN_CHAT), Scroll::Bottom);
    let state = with_open_chat(base_main_state(), MAIN_CHAT, convo);
    let state = with_focus(state, FocusStack::new(Focus::Composer));
    let state = with_toasts(
        state,
        vec![
            toast(
                ChatId(4),
                "Grace Hopper",
                "the compiler is done",
                NOW.0 + 5_000,
            ),
            toast(ChatId(5), "Ada Lovelace", "see you at 6", NOW.0 + 9_000),
        ],
    );
    let state = with_size(state, 140, 40);
    insta::assert_snapshot!(render(140, 40, &state));
}

#[test]
fn toast_stack_over_single_pane_chat_list_80x30() {
    let state = with_toasts(
        base_main_state(),
        vec![toast(
            ChatId(4),
            "Grace Hopper",
            "the compiler is done",
            NOW.0 + 5_000,
        )],
    );
    let state = with_size(state, 80, 30);
    insta::assert_snapshot!(render(80, 30, &state));
}

// --- auth: QR screen -------------------------------------------------------

#[test]
fn auth_qr_screen_100x30() {
    let state = with_auth_phase(
        base_auth_state(),
        AuthPhase::WaitOtherDeviceConfirmation {
            link: "tg://login?token=AAAABBBBCCCC".to_string(),
        },
    );
    let state = with_auth_method(state, Some(LoginMethod::Qr));
    let state = with_size(state, 100, 30);
    insta::assert_snapshot!(render(100, 30, &state));
}

// --- auth: code entry --------------------------------------------------

#[test]
fn auth_code_entry_with_error_100x30() {
    let state = with_auth_phase(
        base_auth_state(),
        AuthPhase::WaitCode {
            delivery_hint: "SMS to +1***34".to_string(),
            length: 5,
        },
    );
    let state = with_auth_field(
        state,
        AuthField::Code,
        "0000",
        4,
        Some(FieldError {
            field: AuthField::Code,
            error: TdError::CodeInvalid,
        }),
    );
    let state = with_size(state, 100, 30);
    insta::assert_snapshot!(render(100, 30, &state));
}

#[test]
fn auth_code_entry_no_error_140x40() {
    let state = with_auth_phase(
        base_auth_state(),
        AuthPhase::WaitCode {
            delivery_hint: "SMS to +1***34".to_string(),
            length: 5,
        },
    );
    let state = with_auth_field(state, AuthField::Code, "123", 3, None);
    let state = with_size(state, 140, 40);
    insta::assert_snapshot!(render(140, 40, &state));
}

// --- consent screen ------------------------------------------------------

#[test]
fn consent_screen_enable_selected_100x30() {
    let state = base_consent_state(ConsentChoice::Enable);
    insta::assert_snapshot!(render(100, 30, &state));
}

#[test]
fn consent_screen_disable_selected_140x40() {
    let state = with_size(base_consent_state(ConsentChoice::Disable), 140, 40);
    insta::assert_snapshot!(render(140, 40, &state));
}

// --- conversation with reactions and read receipts ------------------------

#[test]
fn conversation_reactions_and_receipts_140x30() {
    let alice = Sender::User(UserId(1));
    let me = Sender::User(UserId(3));
    let liked = with_reactions(
        text_message(1, MAIN_CHAT, alice, "Alice", false, 0, "final answer"),
        vec![
            ReactionView {
                emoji: "👍".to_string(),
                count: 3,
                chosen_by_me: true,
            },
            ReactionView {
                emoji: "❤".to_string(),
                count: 1,
                chosen_by_me: false,
            },
        ],
    );
    let read = text_message(2, MAIN_CHAT, me, "You", true, 60, "on it");
    let unread = text_message(3, MAIN_CHAT, me, "You", true, 120, "done");

    let convo = conversation_with(MAIN_CHAT, vec![liked, read, unread], Scroll::Bottom);
    let convo = with_last_read_outbox(convo, MessageId(2));
    let state = with_open_chat(base_main_state(), MAIN_CHAT, convo);
    let state = with_focus(state, FocusStack::new(Focus::Composer));
    let state = with_size(state, 140, 30);
    insta::assert_snapshot!(render(140, 30, &state));
}

// --- file cards: downloading and complete ----------------------------------

#[test]
fn file_cards_downloading_and_complete_140x30() {
    let bob = Sender::User(UserId(2));
    let downloading = doc_message(
        1,
        MAIN_CHAT,
        bob,
        "Bob",
        0,
        FileId(7),
        "architecture.pdf",
        2_516_582,
    );
    let complete = doc_message(
        2,
        MAIN_CHAT,
        bob,
        "Bob",
        60,
        FileId(8),
        "notes.pdf",
        128_000,
    );

    let convo = conversation_with(MAIN_CHAT, vec![downloading, complete], Scroll::Bottom);
    let state = with_open_chat(base_main_state(), MAIN_CHAT, convo);
    let state = with_focus(state, FocusStack::new(Focus::Composer));
    let state = with_file(
        state,
        file_snapshot(FileId(7), 1_000_000, 2_516_582, true, false),
    );
    let state = with_file(
        state,
        file_snapshot(FileId(8), 128_000, 128_000, false, true),
    );
    let state = with_size(state, 140, 30);
    insta::assert_snapshot!(render(140, 30, &state));
}

// --- sidebar: pinned, archive, folder tabs --------------------------------

#[test]
fn sidebar_pinned_archive_and_folder_tabs_140x40() {
    let state = with_chat_list(base_main_state(), sidebar_chat_list());
    let state = with_size(state, 140, 40);
    insta::assert_snapshot!(render(140, 40, &state));
}

#[test]
fn sidebar_archive_active_100x30() {
    let mut list = sidebar_chat_list();
    list.active_list = ChatListId::Archive;
    list.selected = Some(ChatId(5));
    let state = with_chat_list(base_main_state(), list);
    let state = with_size(state, 100, 30);
    insta::assert_snapshot!(render(100, 30, &state));
}

// --- hit map: what the frame reports as clickable (architecture §7.5) -----

/// Same drive as [`render`], keeping the [`HitMap`] `view()` hands back
/// alongside the flattened buffer, so a test can look up where something
/// rendered and then probe that exact cell.
fn render_with_hits(width: u16, height: u16, state: &AppState) -> (String, HitMap) {
    let theme = Theme::default_dark();
    let mut cache = LayoutCache::new();
    let mut hits = HitMap::new();
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|f| {
            hits = tgt_ui::view(state, &theme, f, &mut cache);
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    let mut out = String::with_capacity(buffer.content.len() + buffer.area.height as usize);
    for row in buffer.content.chunks(buffer.area.width as usize) {
        for cell in row {
            out.push_str(cell.symbol());
        }
        out.push('\n');
    }
    (out, hits)
}

/// The screen row `needle` rendered on. Coordinates are looked up from the
/// buffer rather than hard-coded, so these tests assert "the cell showing X
/// resolves to X" instead of restating the layout arithmetic they exist to
/// check.
fn row_showing(rendered: &str, needle: &str) -> u16 {
    rendered
        .lines()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("no rendered row contains {needle:?}:\n{rendered}")) as u16
}

/// The screen column `needle` starts at within one rendered row. Counts
/// characters rather than bytes — the box-drawing borders to the left of any
/// content are multi-byte but exactly one cell wide.
fn column_showing(line: &str, needle: &str) -> u16 {
    let byte = line
        .find(needle)
        .unwrap_or_else(|| panic!("row does not contain {needle:?}: {line:?}"));
    line[..byte].chars().count() as u16
}

#[test]
fn hit_map_resolves_chat_rows_messages_and_the_composer_140x40() {
    let convo = conversation_with(MAIN_CHAT, sample_history(MAIN_CHAT), Scroll::Bottom);
    let state = with_open_chat(base_main_state(), MAIN_CHAT, convo);
    let state = with_focus(state, FocusStack::new(Focus::Composer));
    let state = with_size(state, 140, 40);

    let (rendered, hits) = render_with_hits(140, 40, &state);

    // Sidebar: the selected chat's own row, found by its `▏` selection bar
    // (docs/design-language.md §5). Column 5 is inside the sidebar, past the
    // region padding and the bar.
    let chat_row = row_showing(&rendered, "▏ Alice Müller");
    assert_eq!(
        hits.target_at(5, chat_row),
        Some(HitTarget::ChatRow(MAIN_CHAT))
    );
    // A second chat row, to prove rows are recorded individually rather than
    // the whole list resolving to whatever was selected.
    let other_row = row_showing(&rendered, "Team Rust");
    assert_eq!(
        hits.target_at(5, other_row),
        Some(HitTarget::ChatRow(ChatId(2)))
    );

    // Conversation: the first message of `sample_history` is `MessageId(1)`.
    let message_row = row_showing(&rendered, "hey, did you see the PR?");
    assert_eq!(
        hits.target_at(45, message_row),
        Some(HitTarget::Message(MessageId(1)))
    );

    let composer_row = row_showing(&rendered, "›  message…");
    assert_eq!(hits.target_at(45, composer_row), Some(HitTarget::Composer));

    // Wheel targets: the panes themselves, looked up independently of the
    // click targets that sit inside them.
    assert_eq!(hits.area_at(5, chat_row), Some(ScrollArea::ChatList));
    assert_eq!(
        hits.area_at(45, message_row),
        Some(ScrollArea::Conversation)
    );

    // The hint bar at the bottom is neither clickable nor scrollable.
    let hint_row = row_showing(&rendered, "⏎ send");
    assert_eq!(hits.target_at(5, hint_row), None);
    assert_eq!(hits.area_at(5, hint_row), None);
}

/// The archive pseudo-row and the folder tab strip are clickable in their own
/// right; the dim rule between the pinned and unpinned groups is not.
#[test]
fn hit_map_resolves_the_archive_row_and_folder_tabs_140x40() {
    let state = with_chat_list(base_main_state(), sidebar_chat_list());
    let state = with_size(state, 140, 40);

    let (rendered, hits) = render_with_hits(140, 40, &state);

    let archive_row = row_showing(&rendered, "Archived");
    assert_eq!(hits.target_at(5, archive_row), Some(HitTarget::ArchiveRow));

    let tabs_row = row_showing(&rendered, "Main · Folder 1");
    let tabs_line = rendered.lines().nth(tabs_row as usize).unwrap();
    let main_col = column_showing(tabs_line, "Main");
    assert_eq!(
        hits.target_at(main_col, tabs_row),
        Some(HitTarget::FolderTab(ChatListId::Main))
    );
    let folder_1_col = column_showing(tabs_line, "Folder 1");
    assert_eq!(
        hits.target_at(folder_1_col, tabs_row),
        Some(HitTarget::FolderTab(ChatListId::Folder(1)))
    );
    // The ` · ` between two tabs belongs to neither of them.
    assert_eq!(hits.target_at(folder_1_col - 2, tabs_row), None);
}

/// Overlays are keyboard-only (architecture §7.5), so a frame with one up
/// publishes nothing at all — the panes underneath must not stay clickable
/// through the modal covering them.
#[test]
fn an_overlay_frame_publishes_an_empty_hit_map() {
    let convo = conversation_with(MAIN_CHAT, sample_history(MAIN_CHAT), Scroll::Bottom);
    let state = with_open_chat(base_main_state(), MAIN_CHAT, convo);
    let state = with_focus(state, FocusStack::new(Focus::Composer));
    let state = with_size(state, 140, 40);

    let (_, hits) = render_with_hits(140, 40, &state);
    assert!(!hits.is_empty(), "the plain frame should have targets");

    let state = with_delete_modal(state, MAIN_CHAT, MessageId(3), true, 1);
    let (_, hits) = render_with_hits(140, 40, &state);
    assert!(
        hits.is_empty(),
        "a modal frame must publish no clickable or scrollable regions"
    );
}
