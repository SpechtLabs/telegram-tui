//! Responsive root arrangement (spec §6.1). Two-pane at or above
//! `layout_breakpoint_cols`; a single-pane stack below it. Both arrangements
//! call the same content components — `chat_list::draw`, `conversation::draw`
//! — and differ only in the `Rect` arithmetic that feeds them; the stack is
//! not a second implementation.
//!
//! ## Chrome (docs/design-language.md §1)
//!
//! Regions are separated by space and contrast, not by boxes. There is no
//! outer frame: the app fills the terminal. The entire line budget for the
//! main view is two rules — one vertical between the sidebar and the main
//! column, one horizontal under the chat header — both in `theme.border`,
//! which is dimmer than `text_muted` so chrome never competes with content.
//! Everything else is separated by whitespace.
//!
//! [`pad`] is the one place that whitespace comes from: every region this
//! layout hands to a component is inset one column on each side and one row
//! at the top, so no content ever touches a rule or the terminal edge. The
//! components draw from `(0, 0)` of whatever they are given and know nothing
//! about the padding — which is also why the hit regions recorded here are
//! the *padded* rects: the map has to describe the cells that were actually
//! painted.
//!
//! Which single-pane screen shows is `tgt_core::app::conversation_pane_visible`
//! (T77 task #70): no open chat, or focus still on the chat list / its
//! filter, shows the list; otherwise (a chat is open and focus has moved
//! off the list, i.e. onto the composer or selection mode) shows the
//! conversation behind a breadcrumb. `Esc`'s "back to the chat list" is
//! core's job (`escape`, which swaps the focus base back to
//! `Focus::ChatList` and — since that function's own signature change —
//! also closes the chat with TDLib exactly when this view would stop
//! drawing it); this view only reads the result, via the same shared
//! function core uses to decide that.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use tgt_core::app::{AppState, conversation_pane_visible};
use tgt_core::model::hit::{HitTarget, ScrollArea};
use tgt_core::state::focus::Focus;

use crate::render::hit::HitMap;
use crate::render::state::RenderState;
use crate::theme::Theme;
use crate::view::{
    chat_list, chips, composer, conversation, header, help, hint_bar, modal, palette, toast,
};

const SIDEBAR_WIDTH: u16 = 30;

/// The composer's bare rounded box: two border rows and one row of text.
/// Banners stack on top of it (see [`composer_banner_rows`]).
const COMPOSER_BOX_ROWS: u16 = 3;

/// The header region: its blank breathing row plus the title line.
const HEADER_ROWS: u16 = 2;

/// The bottom row's region: its blank breathing row plus the hint (or chip)
/// line itself.
const BOTTOM_ROWS: u16 = 2;

/// Insets a region for its content: one column of horizontal padding on each
/// side, one blank row above the first content row (design language §1).
/// Saturating throughout, so a region squeezed to nothing by a tiny terminal
/// collapses to an empty `Rect` rather than wrapping around.
fn pad(area: Rect) -> Rect {
    Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(1),
    }
}

/// The vertical rule between the sidebar and the main column: one column
/// wide, drawn as a `Block`'s left edge so it spans the full height without
/// this module repeating a glyph.
fn draw_vertical_rule(area: Rect, theme: &Theme, f: &mut Frame) {
    f.render_widget(
        Block::new()
            .borders(Borders::LEFT)
            .border_style(Style::new().fg(theme.border)),
        area,
    );
}

/// The horizontal rule under the chat header, drawn the same way on a
/// one-row area.
fn draw_horizontal_rule(area: Rect, theme: &Theme, f: &mut Frame) {
    f.render_widget(
        Block::new()
            .borders(Borders::TOP)
            .border_style(Style::new().fg(theme.border)),
        area,
    );
}

/// Where the header's rule meets the sidebar's rule, so the two read as one
/// piece of chrome instead of a `│` sitting next to an unrelated `─`.
fn draw_rule_junction(x: u16, y: u16, theme: &Theme, f: &mut Frame) {
    f.render_widget(
        Paragraph::new(Line::from(Span::styled("├", Style::new().fg(theme.border)))),
        Rect {
            x,
            y,
            width: 1,
            height: 1,
        },
    );
}

/// `rs` is threaded down to the conversation pane, the only view that lays
/// messages out — and therefore the only one that can hit or fill the layout
/// cache, or place an inline image (architecture §4.9.1).
///
/// ## Hit regions (architecture §7.5)
///
/// The panes record what they painted into a [`HitMap`] as they draw it, so
/// the geometry a click resolves against is the geometry that actually
/// rendered rather than a second derivation of it. This function owns the two
/// regions no pane can see for itself: the composer's box (it draws inside a
/// `Rect` this layout carved) and the two scrollable pane rects.
///
/// While an overlay owns the focus, the map is dropped on the floor and an
/// empty one is returned instead of the panes'. Two reasons to make it a
/// *return* rather than a `clear()`: core already ignores clicks and scrolls
/// under an overlay focus (`app.rs::dispatch_click`), so this is the second
/// of two independent guards and should be the simplest possible statement of
/// "nothing here is clickable"; and building the panes' map anyway keeps the
/// draw path branch-free — the overlay decision is made once, at the end,
/// where the focus is already being read to decide what to paint.
pub fn draw(state: &AppState, theme: &Theme, f: &mut Frame, rs: &mut RenderState) -> HitMap {
    let area = f.area();
    f.render_widget(Block::new().style(Style::new().bg(theme.surface)), area);

    let mut hits = HitMap::new();
    if state.width >= state.layout_breakpoint_cols {
        draw_two_pane(area, state, theme, f, rs, &mut hits);
    } else {
        draw_single_pane(area, state, theme, f, rs, &mut hits);
    }

    draw_overlays(state, theme, f);

    if overlay_is_focused(state) {
        HitMap::new()
    } else {
        hits
    }
}

/// Whether one of the keyboard-only layers is on top. Mirrors the conditions
/// [`draw_overlays`] paints under — toasts excluded, since they belong to no
/// focus level and never take input.
fn overlay_is_focused(state: &AppState) -> bool {
    matches!(
        state.focus.current(),
        Focus::Modal(_) | Focus::Palette | Focus::Help
    )
}

/// The layers that sit over the whole frame rather than inside a pane, in
/// the order they stack.
///
/// A modal and the palette are both raised by a focus level and are mutually
/// exclusive by construction: `ctrl+p` is a global-layer key, and a modal is
/// the one context that never lets a key reach that layer (spec §6.2), so at
/// most one of these two is on top at a time. Each draws only while its own
/// level is current — core guarantees the transient state exists exactly
/// then (`app.rs::sync_modal_storage` for the modal, `toggle_palette` for
/// the palette).
///
/// Toasts go last, unconditionally: they belong to no focus level (nothing
/// is focused on them — `esc` dismisses the newest from wherever the user
/// is), and `view::toast` is written to paint over whatever the frame
/// already holds. A toast that arrived while the palette is up is still the
/// newest thing on screen and still has to be readable.
fn draw_overlays(state: &AppState, theme: &Theme, f: &mut Frame) {
    if matches!(state.focus.current(), Focus::Modal(_)) {
        modal::draw(state, theme, f);
    }
    if matches!(state.focus.current(), Focus::Palette) {
        palette::draw(state, theme, f);
    }
    if matches!(state.focus.current(), Focus::Help) {
        help::draw(state, theme, f);
    }
    if !state.toasts.toasts.is_empty() {
        toast::draw(state, theme, f);
    }
}

/// The bottom row: selection mode replaces the hint bar with its chip row
/// (spec §6.3), every other focus gets the hint line for its context.
/// `hint_bar::hint_for` returning `None` for `Focus::Selection` is that
/// module's way of saying "not mine to draw"; this is where the alternative
/// is chosen, because the frame layout is what knows there is a row here.
fn draw_bottom_row(area: Rect, state: &AppState, theme: &Theme, f: &mut Frame) {
    if matches!(state.focus.current(), Focus::Selection) {
        chips::draw(area, state, theme, f);
    } else {
        hint_bar::draw_for(area, state.focus.current(), theme, f);
    }
}

/// Two-pane arrangement (spec §6.1's layout, this module's chrome): fixed
/// sidebar, one vertical rule, main column with the chat header over the
/// conversation, hint bar spanning the bottom.
fn draw_two_pane(
    area: Rect,
    state: &AppState,
    theme: &Theme,
    f: &mut Frame,
    rs: &mut RenderState,
    hits: &mut HitMap,
) {
    let [content_area, bottom_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(BOTTOM_ROWS)]).areas(area);

    let [sidebar_area, rule_area, main_area] = Layout::horizontal([
        Constraint::Length(SIDEBAR_WIDTH),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(content_area);

    draw_vertical_rule(rule_area, theme, f);

    let sidebar = pad(sidebar_area);
    hits.push_area(sidebar, ScrollArea::ChatList);
    chat_list::draw(sidebar, state, theme, f, hits);

    let [header_area, header_rule, body_area] = Layout::vertical([
        Constraint::Length(HEADER_ROWS),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(main_area);
    header::draw(pad(header_area), state, theme, f);
    draw_horizontal_rule(header_rule, theme, f);
    draw_rule_junction(rule_area.x, header_rule.y, theme, f);
    draw_conversation_and_composer(body_area, state, theme, f, rs, hits);

    draw_bottom_row(pad(bottom_area), state, theme, f);
}

/// Single-pane stack below the breakpoint (spec §6.1): full-width chat list,
/// or a breadcrumb (`telegram ▸ <chat title>`) over a full-width conversation
/// once a chat is open and focus has left the list.
fn draw_single_pane(
    area: Rect,
    state: &AppState,
    theme: &Theme,
    f: &mut Frame,
    rs: &mut RenderState,
    hits: &mut HitMap,
) {
    // `conversation_pane_visible` (architecture §7.5.1's neighbor, T77 task
    // #70): core needs the identical fact to decide whether leaving the
    // conversation side should tell TDLib the chat is no longer open, so it
    // lives there and this calls it rather than keeping a second copy of
    // the same three-field comparison that could drift from it.
    let showing_chat_list = !conversation_pane_visible(state);

    if showing_chat_list {
        let [list_area, bottom_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(BOTTOM_ROWS)]).areas(area);
        let list = pad(list_area);
        hits.push_area(list, ScrollArea::ChatList);
        chat_list::draw(list, state, theme, f, hits);
        draw_bottom_row(pad(bottom_area), state, theme, f);
        return;
    }

    let [breadcrumb_area, breadcrumb_rule, body_area, bottom_area] = Layout::vertical([
        Constraint::Length(HEADER_ROWS),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(BOTTOM_ROWS),
    ])
    .areas(area);

    draw_breadcrumb(pad(breadcrumb_area), state, theme, f);
    draw_horizontal_rule(breadcrumb_rule, theme, f);
    draw_conversation_and_composer(body_area, state, theme, f, rs, hits);
    draw_bottom_row(pad(bottom_area), state, theme, f);
}

/// `telegram ▸ <chat title>` (spec §6.1): the single-pane stack's back
/// affordance, standing in for the two-pane sidebar. Only reached once
/// `draw_single_pane` has established `open_chat` is `Some`.
fn draw_breadcrumb(area: Rect, state: &AppState, theme: &Theme, f: &mut Frame) {
    let title = state
        .open_chat
        .and_then(|id| state.chat_list.chats.get(&id))
        .map(|chat| chat.title.as_str())
        .unwrap_or_default();

    let line = Line::from(vec![
        Span::styled("telegram ", Style::new().fg(theme.text_muted)),
        Span::styled("▸ ", Style::new().fg(theme.text_muted)),
        Span::styled(
            title.to_string(),
            Style::new().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

/// The conversation viewport and the composer beneath it: the part of the
/// layout both arrangements show identically once a chat is (or can be)
/// open. Factored out so neither arrangement duplicates this rendering — the
/// two-pane and single-pane call sites only differ in what `Rect` they hand
/// in and what sits above it (the chat header vs. the breadcrumb).
fn draw_conversation_and_composer(
    area: Rect,
    state: &AppState,
    theme: &Theme,
    f: &mut Frame,
    rs: &mut RenderState,
    hits: &mut HitMap,
) {
    let composer_height = COMPOSER_BOX_ROWS + composer_banner_rows(state);
    // The blank row between the two is the same "regions are separated by
    // space" rule the rest of this layout follows; the composer's own box
    // would otherwise butt straight against the last message.
    let [conversation_area, _gap, composer_area] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(composer_height),
    ])
    .areas(pad(area));

    hits.push_area(conversation_area, ScrollArea::Conversation);
    conversation::draw(conversation_area, state, theme, f, rs, hits);
    // The whole composer area, banners included: a click anywhere in this
    // block means "type here", and a reply banner is part of the composer,
    // not a separate thing to hit.
    hits.push(composer_area, HitTarget::Composer);
    composer::draw(composer_area, state, theme, f);
}

/// How many banner rows `view::composer` will stack above its input box.
/// That module takes the `Rect` it is handed and never grows it, so sizing
/// the area is this caller's job — and getting it wrong is not cosmetic: a
/// bare three-row area with one banner in it leaves the bordered box zero
/// rows of interior, and the text being typed disappears.
///
/// Mirrors the banner conditions in `view::composer::draw`, which stays the
/// source of truth for what actually renders.
fn composer_banner_rows(state: &AppState) -> u16 {
    let composer = &state.composer;
    u16::from(composer.reply_to.is_some())
        + u16::from(composer.editing.is_some())
        + u16::from(composer.pending_send.is_some())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap, VecDeque};

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tgt_core::app::{AppState, Screen};
    use tgt_core::effect::TelemetryMode;
    use tgt_core::model::chat::{ChatKind, ChatOrderKey, ChatView};
    use tgt_core::model::chips::Chip;
    use tgt_core::model::ids::{ChatId, MessageId};
    use tgt_core::model::key::KeyBindings;
    use tgt_core::model::time::Millis;
    use tgt_core::state::auth::{AuthField, AuthState, InputField};
    use tgt_core::state::chat_list::ChatListState;
    use tgt_core::state::composer::ComposerState;
    use tgt_core::state::consent::{ConsentChoice, ConsentState};
    use tgt_core::state::conversation::{ConversationState, Scroll};
    use tgt_core::state::focus::{FocusStack, ModalKind};
    use tgt_core::state::history::PagingState;
    use tgt_core::state::media::MediaState;
    use tgt_core::state::modal::ModalState;
    use tgt_core::state::palette::{CommandId, PaletteItem, PaletteState};
    use tgt_core::state::presence::PresenceState;
    use tgt_core::state::selection::SelectionState;
    use tgt_core::state::toasts::{Toast, ToastState};
    use tgt_core::td::update::{AuthPhase, ConnectionPhase};

    use super::*;

    const CHAT: ChatId = ChatId(1);

    fn chat_list_with_one_chat() -> ChatListState {
        let mut list = ChatListState::default();
        list.chats.insert(
            CHAT,
            ChatView {
                id: CHAT,
                kind: ChatKind::Private,
                title: "Alice Müller".to_string(),
                positions: Vec::new(),
                unread_count: 2,
                unread_mention_count: 0,
                last_message: None,
                is_muted: false,
            },
        );
        let mut orders = BTreeSet::new();
        orders.insert(ChatOrderKey {
            order: 1,
            chat_id: CHAT,
        });
        list.orders.insert(list.active_list, orders);
        list.selected = Some(CHAT);
        list
    }

    /// `open_chat` and `focus` are the two knobs the tests below flip: which
    /// single-pane screen shows is a pure function of them (module docs).
    fn fixture_state(width: u16, open_chat: Option<ChatId>, focus: FocusStack) -> AppState {
        let mut conversations = HashMap::new();
        if let Some(chat_id) = open_chat {
            conversations.insert(
                chat_id,
                ConversationState {
                    chat_id,
                    messages: Default::default(),
                    paging: PagingState::Idle,
                    scroll: Scroll::Bottom,
                    revealed_spoilers: BTreeSet::new(),
                    last_read_inbox: tgt_core::model::ids::MessageId(0),
                    last_read_outbox: tgt_core::model::ids::MessageId(0),
                    pending_view: None,
                    search_hits: Vec::new(),
                    selection: None,
                    hunt: None,
                },
            );
        }
        AppState {
            screen: Screen::Main,
            focus,
            connection: ConnectionPhase::Ready,
            consent: ConsentState {
                selected: ConsentChoice::Enable,
                acknowledged: true,
            },
            auth: AuthState {
                phase: AuthPhase::Ready,
                method: None,
                api_id: InputField::default(),
                api_hash: InputField::default(),
                phone: InputField::default(),
                code: InputField::default(),
                password: InputField::default(),
                active_field: AuthField::Phone,
                field_error: None,
                flood_wait_until: None,
                in_flight: false,
            },
            chat_list: chat_list_with_one_chat(),
            conversations,
            open_chat,
            composer: ComposerState::default(),
            modal_ui: None,
            palette: None,
            chat_search: None,
            toasts: ToastState::default(),
            media: MediaState::default(),
            presence: PresenceState::default(),
            width,
            height: 30,
            layout_breakpoint_cols: 100,
            theme_name: "dark".to_string(),
            theme_generation: 0,
            bindings: KeyBindings::default(),
            telemetry_mode: TelemetryMode::Off,
            crash_reports_available: false,
            telemetry_salt: [0u8; 32],
            now: Millis(0),
            visible_messages: None,
        }
    }

    fn render_to_string(width: u16, height: u16, state: &AppState) -> String {
        let theme = Theme::default_dark();
        let mut rs = RenderState::new(None);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| {
                draw(state, &theme, f, &mut rs);
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

    #[test]
    fn single_pane_stack_shows_full_width_chat_list_99x30() {
        let state = fixture_state(99, None, FocusStack::new(Focus::ChatList));
        let rendered = render_to_string(99, 30, &state);
        assert!(rendered.contains("Alice Müller"));
        assert!(rendered.contains(hint_bar::HINT_TEXT));
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn single_pane_stack_shows_breadcrumb_and_conversation_99x30() {
        let state = fixture_state(99, Some(CHAT), FocusStack::new(Focus::Composer));
        let rendered = render_to_string(99, 30, &state);
        assert!(rendered.contains("telegram"));
        assert!(rendered.contains("▸"));
        assert!(rendered.contains("Alice Müller"));
        assert!(rendered.contains("message…"));
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn two_pane_arrangement_at_breakpoint_100x30() {
        let state = fixture_state(100, Some(CHAT), FocusStack::new(Focus::Composer));
        let rendered = render_to_string(100, 30, &state);
        // Both panes visible at once: the sidebar's chat row and the
        // composer placeholder share a frame, which never happens in the
        // single-pane stack.
        assert!(rendered.contains("Alice Müller"));
        assert!(rendered.contains("message…"));
        insta::assert_snapshot!(rendered);
    }

    /// Spec §6.3: while a message is selected, the chip row *replaces* the
    /// hint bar rather than sharing the row with it.
    #[test]
    fn selection_mode_draws_the_chip_row_instead_of_the_hint_bar() {
        let mut state = fixture_state(120, Some(CHAT), FocusStack::new(Focus::Composer));
        state.focus.push(Focus::Selection);
        state.conversations.get_mut(&CHAT).unwrap().selection = Some(SelectionState {
            message_id: MessageId(1),
            chips: vec![Chip::Reply, Chip::Copy, Chip::Delete],
            chip_cursor: 0,
            chip_scroll: 0,
        });

        let rendered = render_to_string(120, 30, &state);
        assert!(
            rendered.contains("[R Reply]"),
            "chip row missing:\n{rendered}"
        );
        assert!(rendered.contains("[X Delete]"));
        assert!(
            !rendered.contains(hint_bar::HINT_TEXT),
            "the hint bar must give the row up entirely:\n{rendered}"
        );
    }

    /// The modal is drawn last, over everything: the sidebar behind it is
    /// covered, not merely overlapped.
    #[test]
    fn modal_overlay_covers_the_panes_beneath_it() {
        let mut state = fixture_state(120, Some(CHAT), FocusStack::new(Focus::Composer));
        state.focus.push(Focus::Modal(ModalKind::ConfirmDelete {
            chat_id: CHAT,
            message_id: MessageId(1),
            can_revoke: true,
        }));
        state.modal_ui = Some(ModalState::default());

        let rendered = render_to_string(120, 30, &state);
        assert!(
            rendered.contains("Delete for everyone"),
            "modal missing:\n{rendered}"
        );
        assert!(
            !rendered.contains("Alice Müller"),
            "the sidebar shows through the modal:\n{rendered}"
        );
    }

    /// The palette is a second overlay above the same panes the modal covers,
    /// raised by `Focus::Palette` rather than by `Focus::Modal`. Without this
    /// wiring T46's view is unreachable from the running binary.
    #[test]
    fn palette_overlay_draws_over_the_two_pane_arrangement_120x40() {
        let mut state = fixture_state(120, Some(CHAT), FocusStack::new(Focus::Composer));
        state.height = 40;
        state.focus.push(Focus::Palette);
        state.palette = Some(PaletteState {
            input: InputField {
                text: "al".to_string(),
                cursor: 2,
            },
            results: vec![
                PaletteItem::Chat {
                    id: CHAT,
                    score: 120,
                },
                PaletteItem::Command {
                    id: CommandId::ToggleTheme,
                    score: 40,
                },
            ],
            selected: 0,
        });

        let rendered = render_to_string(120, 40, &state);
        assert!(
            rendered.contains("palette"),
            "the palette overlay is missing:\n{rendered}"
        );
        assert!(rendered.contains("Toggle theme"));
        insta::assert_snapshot!(rendered);
    }

    /// Toasts are the last thing painted and belong to no focus level, so
    /// the chat list keeps its focus (and its hint bar) underneath them.
    #[test]
    fn toast_stack_draws_over_the_frame_120x40() {
        let mut state = fixture_state(120, Some(CHAT), FocusStack::new(Focus::Composer));
        state.height = 40;
        state.toasts = ToastState {
            toasts: VecDeque::from(vec![Toast {
                chat_id: Some(ChatId(2)),
                title: "Grace Hopper".to_string(),
                body: "the compiler is done".to_string(),
                expires_at: Millis(5_000),
            }]),
        };

        let rendered = render_to_string(120, 40, &state);
        assert!(
            rendered.contains("the compiler is done"),
            "the toast is missing:\n{rendered}"
        );
        insta::assert_snapshot!(rendered);
    }

    /// The composer's banners are rows the *caller* has to reserve. Without
    /// the extra row the bordered box has no interior and the draft the user
    /// is replying with vanishes.
    #[test]
    fn reply_banner_gets_a_row_of_its_own_without_squeezing_the_input() {
        let mut state = fixture_state(120, Some(CHAT), FocusStack::new(Focus::Composer));
        state.composer.reply_to = Some(MessageId(1));
        state.composer.input.text = "on it".to_string();
        state.composer.input.cursor = 5;

        assert_eq!(composer_banner_rows(&state), 1);
        let rendered = render_to_string(120, 30, &state);
        assert!(
            rendered.contains("on it"),
            "the draft was squeezed out by the banner:\n{rendered}"
        );
    }

    #[test]
    fn single_pane_stack_falls_back_to_chat_list_once_focus_returns_there() {
        // `Esc`'s back button (T28's `escape`): open_chat stays `Some`, only
        // the focus base swaps back to `Focus::ChatList`. The view must
        // follow focus, not `open_chat`, or "back" would render nothing new.
        let state = fixture_state(99, Some(CHAT), FocusStack::new(Focus::ChatList));
        let rendered = render_to_string(99, 30, &state);
        assert!(rendered.contains(hint_bar::HINT_TEXT));
        assert!(!rendered.contains("telegram ▸"));
        assert!(!rendered.contains("message…"));
    }
}
