//! The history paging state machine — freestanding and pure.
//! See docs/architecture.md §4.6 and design spec §5.2 (the empty-response
//! trap: `getChatHistory` may legitimately return zero messages on the first
//! call for a chat while TDLib fetches from the server, even though more
//! history exists. A short or empty response is therefore never treated as
//! proof of end-of-history on its own).

use crate::model::ids::MessageId;
use crate::model::time::Millis;

pub const PAGE_SIZE: u8 = 50;
/// Trigger paging when the scroll anchor is within this many MESSAGES of the
/// oldest loaded one (core counts messages, not rows: rows are a ui concept).
pub const PAGE_TRIGGER_MESSAGES: usize = 20;
/// An empty response is NOT end-of-history (spec §5.2): retry with
/// only_local = false up to this bound before believing TDLib.
pub const MAX_EMPTY_ATTEMPTS: u8 = 3;
/// Fixed backoff used when TDLib reports an error without a `retry_after`
/// (e.g. a generic transient failure rather than FLOOD_WAIT). FLOOD_WAIT
/// itself always carries a `retry_after` and uses that instead.
pub const DEFAULT_ERROR_COOLDOWN_MS: u64 = 3_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PagingState {
    Idle,
    Loading {
        attempt: u8,
        only_local: bool,
    },
    /// FloodWait or transient error: no requests until `until`.
    Cooldown {
        until: Millis,
    },
    /// Only entered when a non-local request came back empty at max attempts.
    Exhausted,
}

/// What the caller (conversation.rs) must do after feeding an event in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PagingDirective {
    None,
    /// Issue Effect::Td(GetChatHistory { from_message_id, limit: PAGE_SIZE, only_local }).
    Request {
        from_message_id: MessageId,
        only_local: bool,
    },
}

/// The viewport scrolled within `PAGE_TRIGGER_MESSAGES` of the oldest loaded
/// message. Requests the next page unless we are already `Loading`, still in
/// `Cooldown`, or `Exhausted`.
///
/// Design choice: v1 always requests remote-capable (`only_local: false`).
/// The `only_local: true` optimization TDLib supports (serve from the local
/// database first, cheaper than a network round trip) is not used here — the
/// plan's directive examples always show `only_local: false`, and mixing the
/// two would need extra bookkeeping this milestone doesn't need yet.
///
/// `oldest_loaded: None` means nothing has been loaded into the window yet;
/// that is the conversation opener's job (T16 fires the *first* page some
/// other way), not scroll-triggered paging, so this returns `None` even from
/// `Idle`.
pub fn on_scroll_near_top(
    paging: &mut PagingState,
    oldest_loaded: Option<MessageId>,
    now: Millis,
) -> PagingDirective {
    match *paging {
        PagingState::Idle => match oldest_loaded {
            Some(oldest) => {
                *paging = PagingState::Loading {
                    attempt: 1,
                    only_local: false,
                };
                PagingDirective::Request {
                    from_message_id: oldest,
                    only_local: false,
                }
            }
            None => PagingDirective::None,
        },
        PagingState::Loading { .. } => PagingDirective::None,
        PagingState::Cooldown { until } => {
            if now >= until {
                *paging = PagingState::Idle;
                on_scroll_near_top(paging, oldest_loaded, now)
            } else {
                PagingDirective::None
            }
        }
        PagingState::Exhausted => PagingDirective::None,
    }
}

/// A `GetChatHistory` response arrived. `received` is the number of messages
/// TDLib returned; `was_only_local` is the `only_local` flag the request that
/// produced this response was made with.
///
/// Semantics (spec §5.2 — the empty-response trap):
/// - Any non-empty response, however short, means real progress: go back to
///   `Idle` and let the caller prepend. A single message is not exhaustion.
/// - An empty response from an `only_local: true` request proves nothing —
///   TDLib hasn't even asked the server yet — so it is re-issued remote
///   (`only_local: false`) unconditionally. Local empties never advance
///   `attempt` and never count toward `MAX_EMPTY_ATTEMPTS`: v1 never sends
///   `only_local: true` requests itself (see `on_scroll_near_top`), but this
///   path exists so the state machine is correct if that ever changes, and
///   the attempt counter is reset to 1 for the remote retry it triggers.
/// - An empty, non-local response is the only kind that can end paging: retry
///   up to `MAX_EMPTY_ATTEMPTS` times, incrementing `attempt`, before
///   transitioning to `Exhausted`.
/// - If this fires while `paging` is not `Loading` (a stale completion — the
///   request that produced it is no longer the one we're tracking, e.g. a
///   `Cooldown` or `Exhausted` state was entered in the meantime), the state
///   is left untouched and `None` is returned: nothing this response says is
///   still relevant.
pub fn on_history_loaded(
    paging: &mut PagingState,
    received: usize,
    was_only_local: bool,
    oldest_loaded: Option<MessageId>,
) -> PagingDirective {
    let PagingState::Loading { attempt, .. } = *paging else {
        return PagingDirective::None;
    };

    if received > 0 {
        *paging = PagingState::Idle;
        return PagingDirective::None;
    }

    // received == 0 from here on.
    if was_only_local {
        *paging = PagingState::Loading {
            attempt: 1,
            only_local: false,
        };
    } else if attempt < MAX_EMPTY_ATTEMPTS {
        *paging = PagingState::Loading {
            attempt: attempt + 1,
            only_local: false,
        };
    } else {
        *paging = PagingState::Exhausted;
        return PagingDirective::None;
    }

    match oldest_loaded {
        Some(oldest) => PagingDirective::Request {
            from_message_id: oldest,
            only_local: false,
        },
        None => PagingDirective::None,
    }
}

/// The `GetChatHistory` request failed (FLOOD_WAIT or any other TDLib
/// error). Enters `Cooldown` from any state — including `Exhausted`, since an
/// error can only reach this function via a request that was in flight, and
/// whatever caused it deserves a backoff regardless of what `paging` was
/// before. `retry_after` is TDLib's FLOOD_WAIT seconds when present;
/// otherwise `DEFAULT_ERROR_COOLDOWN_MS` is used as a fixed backoff.
pub fn on_history_error(paging: &mut PagingState, retry_after: Option<u32>, now: Millis) {
    let cooldown_ms = match retry_after {
        Some(secs) => u64::from(secs).saturating_mul(1_000),
        None => DEFAULT_ERROR_COOLDOWN_MS,
    };
    *paging = PagingState::Cooldown {
        until: now.saturating_add(cooldown_ms),
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    const M1: MessageId = MessageId(1);
    const M2: MessageId = MessageId(2);

    #[test]
    fn idle_scroll_near_top_requests_page() {
        let mut paging = PagingState::Idle;
        let directive = on_scroll_near_top(&mut paging, Some(M1), Millis(0));
        assert_eq!(
            directive,
            PagingDirective::Request {
                from_message_id: M1,
                only_local: false,
            }
        );
        assert_eq!(
            paging,
            PagingState::Loading {
                attempt: 1,
                only_local: false,
            }
        );
    }

    #[test]
    fn idle_scroll_near_top_with_nothing_loaded_does_not_request() {
        let mut paging = PagingState::Idle;
        let directive = on_scroll_near_top(&mut paging, None, Millis(0));
        assert_eq!(directive, PagingDirective::None);
        assert_eq!(paging, PagingState::Idle);
    }

    #[test]
    fn loading_ignores_further_scroll() {
        let mut paging = PagingState::Loading {
            attempt: 1,
            only_local: false,
        };
        let before = paging;
        let directive = on_scroll_near_top(&mut paging, Some(M1), Millis(0));
        assert_eq!(directive, PagingDirective::None);
        assert_eq!(paging, before);
    }

    #[test]
    fn empty_response_retries_up_to_max() {
        // attempt 1 -> empty, non-local -> retry as attempt 2.
        let mut paging = PagingState::Loading {
            attempt: 1,
            only_local: false,
        };
        let directive = on_history_loaded(&mut paging, 0, false, Some(M1));
        assert_eq!(
            directive,
            PagingDirective::Request {
                from_message_id: M1,
                only_local: false,
            }
        );
        assert_eq!(
            paging,
            PagingState::Loading {
                attempt: 2,
                only_local: false,
            }
        );

        // attempt 2 -> empty, non-local -> retry as attempt 3.
        let directive = on_history_loaded(&mut paging, 0, false, Some(M1));
        assert_eq!(
            directive,
            PagingDirective::Request {
                from_message_id: M1,
                only_local: false,
            }
        );
        assert_eq!(
            paging,
            PagingState::Loading {
                attempt: 3,
                only_local: false,
            }
        );

        // attempt 3 (== MAX_EMPTY_ATTEMPTS) -> empty, non-local -> Exhausted.
        let directive = on_history_loaded(&mut paging, 0, false, Some(M1));
        assert_eq!(directive, PagingDirective::None);
        assert_eq!(paging, PagingState::Exhausted);
    }

    #[test]
    fn empty_local_response_never_exhausts() {
        // Even parked at the max attempt, a *local* empty response always
        // re-requests remote instead of exhausting — local emptiness never
        // proves anything about the server.
        let mut paging = PagingState::Loading {
            attempt: MAX_EMPTY_ATTEMPTS,
            only_local: true,
        };
        let directive = on_history_loaded(&mut paging, 0, true, Some(M1));
        assert_eq!(
            directive,
            PagingDirective::Request {
                from_message_id: M1,
                only_local: false,
            }
        );
        assert_eq!(
            paging,
            PagingState::Loading {
                attempt: 1,
                only_local: false,
            }
        );
    }

    #[test]
    fn nonempty_response_resets_to_idle_and_prepends() {
        let mut paging = PagingState::Loading {
            attempt: 1,
            only_local: false,
        };
        let directive = on_history_loaded(&mut paging, PAGE_SIZE as usize, false, Some(M1));
        assert_eq!(directive, PagingDirective::None);
        assert_eq!(paging, PagingState::Idle);
    }

    #[test]
    fn short_but_nonempty_response_is_not_exhausted() {
        let mut paging = PagingState::Loading {
            attempt: 1,
            only_local: false,
        };
        let directive = on_history_loaded(&mut paging, 1, false, Some(M1));
        assert_eq!(directive, PagingDirective::None);
        assert_eq!(paging, PagingState::Idle);
    }

    #[test]
    fn stale_completion_while_not_loading_is_ignored() {
        for mut paging in [
            PagingState::Idle,
            PagingState::Cooldown { until: Millis(50) },
            PagingState::Exhausted,
        ] {
            let before = paging;
            let directive = on_history_loaded(&mut paging, 0, false, Some(M1));
            assert_eq!(directive, PagingDirective::None);
            assert_eq!(paging, before);

            let directive = on_history_loaded(&mut paging, 5, false, Some(M1));
            assert_eq!(directive, PagingDirective::None);
            assert_eq!(paging, before);
        }
    }

    #[test]
    fn flood_wait_enters_cooldown_until_deadline() {
        let mut paging = PagingState::Loading {
            attempt: 1,
            only_local: false,
        };
        on_history_error(&mut paging, Some(5), Millis(1_000));
        assert_eq!(
            paging,
            PagingState::Cooldown {
                until: Millis(6_000)
            }
        );
    }

    #[test]
    fn error_without_retry_after_uses_default_cooldown() {
        let mut paging = PagingState::Loading {
            attempt: 1,
            only_local: false,
        };
        on_history_error(&mut paging, None, Millis(1_000));
        assert_eq!(
            paging,
            PagingState::Cooldown {
                until: Millis(1_000 + DEFAULT_ERROR_COOLDOWN_MS),
            }
        );
    }

    #[test]
    fn error_enters_cooldown_from_any_state() {
        for paging in [
            PagingState::Idle,
            PagingState::Loading {
                attempt: 2,
                only_local: false,
            },
            PagingState::Cooldown { until: Millis(10) },
            PagingState::Exhausted,
        ] {
            let mut paging = paging;
            on_history_error(&mut paging, Some(1), Millis(0));
            assert_eq!(
                paging,
                PagingState::Cooldown {
                    until: Millis(1_000)
                }
            );
        }
    }

    #[test]
    fn cooldown_expires_back_to_idle_on_next_scroll() {
        let mut paging = PagingState::Cooldown {
            until: Millis(1_000),
        };
        let directive = on_scroll_near_top(&mut paging, Some(M2), Millis(1_000));
        assert_eq!(
            directive,
            PagingDirective::Request {
                from_message_id: M2,
                only_local: false,
            }
        );
        assert_eq!(
            paging,
            PagingState::Loading {
                attempt: 1,
                only_local: false,
            }
        );
    }

    #[test]
    fn cooldown_still_active_returns_none() {
        let mut paging = PagingState::Cooldown {
            until: Millis(1_000),
        };
        let directive = on_scroll_near_top(&mut paging, Some(M2), Millis(999));
        assert_eq!(directive, PagingDirective::None);
        assert_eq!(
            paging,
            PagingState::Cooldown {
                until: Millis(1_000)
            }
        );
    }

    #[test]
    fn exhausted_never_requests_again() {
        let mut paging = PagingState::Exhausted;
        let directive = on_scroll_near_top(&mut paging, Some(M1), Millis(999_999));
        assert_eq!(directive, PagingDirective::None);
        assert_eq!(paging, PagingState::Exhausted);
    }
}
