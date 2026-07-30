//! THE COMPLETE allowlist. Additions are a deliberate, snapshotted, reviewed
//! diff (spec §13.8). See docs/architecture.md §4.8.

pub mod keys {
    pub const APP_VERSION: &str = "app.version";
    pub const OS_VERSION: &str = "os.version";
    pub const TERM_PROGRAM: &str = "term.program";
    pub const TERM_GRAPHICS_PROTOCOL: &str = "term.graphics_protocol"; // kitty|iterm2|sixel|none
    pub const TERM_WIDTH_BUCKET: &str = "term.width_bucket"; // <80|80-120|120-160|>160
    pub const INSTALL_ID: &str = "install.id";
    pub const SESSION_ID: &str = "session.id";
    pub const ACTION: &str = "action";
    pub const OUTCOME: &str = "outcome"; // ok|error|cancelled
    pub const ERROR_KIND: &str = "error.kind";
    pub const DURATION_MS: &str = "duration_ms";
    pub const CHAT_KIND: &str = "chat.kind"; // private|group|supergroup|channel
    pub const CHAT_HASH: &str = "chat.hash"; // HMAC-SHA256, 8 bytes, hex
    pub const HISTORY_PAGE_DEPTH: &str = "history.page_depth";
    pub const DOWNLOAD_SIZE_BUCKET: &str = "download.size_bucket";
    pub const PUBLIC_MARKER: &str = "telemetry.public";
}

pub const ALLOWED_KEYS: &[&str] = &[
    keys::APP_VERSION,
    keys::OS_VERSION,
    keys::TERM_PROGRAM,
    keys::TERM_GRAPHICS_PROTOCOL,
    keys::TERM_WIDTH_BUCKET,
    keys::INSTALL_ID,
    keys::SESSION_ID,
    keys::ACTION,
    keys::OUTCOME,
    keys::ERROR_KIND,
    keys::DURATION_MS,
    keys::CHAT_KIND,
    keys::CHAT_HASH,
    keys::HISTORY_PAGE_DEPTH,
    keys::DOWNLOAD_SIZE_BUCKET,
    keys::PUBLIC_MARKER,
];

pub mod actions {
    pub const APP_START: &str = "app.start";
    pub const APP_QUIT: &str = "app.quit";
    pub const QR_LOGIN: &str = "qr_login";
    pub const PHONE_LOGIN: &str = "phone_login";
    pub const CHAT_OPEN: &str = "chat.open";
    pub const MESSAGE_SEND: &str = "message.send";
    pub const MESSAGE_REPLY: &str = "message.reply";
    pub const MESSAGE_FORWARD: &str = "message.forward";
    pub const MESSAGE_DELETE: &str = "message.delete";
    pub const MESSAGE_EDIT: &str = "message.edit";
    pub const MESSAGE_REACT: &str = "message.react";
    pub const HISTORY_PAGE: &str = "history.page";
    pub const PALETTE_OPEN: &str = "palette.open";
    pub const SEARCH_RUN: &str = "search.run";
    pub const FILE_DOWNLOAD: &str = "file.download";
    pub const FILE_UPLOAD: &str = "file.upload";
    pub const THEME_CHANGE: &str = "theme.change";
}

pub mod error_kinds {
    pub const TD_FLOOD_WAIT: &str = "td.flood_wait";
    pub const TD_AUTH: &str = "td.auth";
    pub const TD_RATE_LIMIT: &str = "td.rate_limit";
    pub const TD_OTHER: &str = "td.other";
    pub const NET_TIMEOUT: &str = "net.timeout";
    pub const NET_OFFLINE: &str = "net.offline";
    pub const LAYOUT_PANIC: &str = "layout.panic";
    pub const IO_DENIED: &str = "io.denied";
    pub const IO_OTHER: &str = "io.other";
}

pub mod buckets {
    pub fn width(cols: u16) -> &'static str {
        match cols {
            0..=79 => "<80",
            80..=120 => "80-120",
            121..=160 => "120-160",
            _ => ">160",
        }
    }
    pub fn download_size(bytes: u64) -> &'static str {
        const MB: u64 = 1_000_000;
        match bytes {
            b if b < MB => "<1MB",
            b if b < 10 * MB => "1-10MB",
            b if b < 100 * MB => "10-100MB",
            _ => ">100MB",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_snapshot() {
        insta::assert_json_snapshot!(ALLOWED_KEYS);
    }

    #[test]
    fn width_bucket_boundaries() {
        assert_eq!(buckets::width(79), "<80");
        assert_eq!(buckets::width(80), "80-120");
        assert_eq!(buckets::width(120), "80-120");
        assert_eq!(buckets::width(121), "120-160");
        assert_eq!(buckets::width(161), ">160");
    }
}
