//! A tiny JSON writer, just enough for the CLI's `--json` summaries.
//!
//! The control API will bring its own serialisation; this exists so `fluxd` can
//! emit machine-readable output without adding a dependency or, worse, hand-built
//! string formatting that breaks on a path containing a quote.

use std::fmt;

/// Streaming JSON writer.
///
/// Callers open containers with [`JsonOut::object`] / [`JsonOut::array`] and write
/// values inside them. Commas and colons are handled automatically, so the output
/// is always well-formed even if a field is conditionally skipped.
#[derive(Debug, Default)]
pub struct JsonOut {
    buffer: String,
    /// One `first element not yet written` flag per open container.
    stack: Vec<bool>,
    /// A key was just written and its value is next (no separator before it).
    pending_value: bool,
}

impl JsonOut {
    /// An empty writer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Writes a string field name.
    pub fn key(&mut self, key: &str) {
        self.prefix();
        self.buffer.push_str(&quote(key));
        self.buffer.push(':');
        self.pending_value = true;
    }

    /// Writes a string value.
    pub fn string(&mut self, value: &str) {
        self.prefix();
        self.buffer.push_str(&quote(value));
    }

    /// Writes a number.
    pub fn number(&mut self, value: impl fmt::Display) {
        self.prefix();
        use fmt::Write as _;
        // Writing into a `String` cannot fail.
        let _ = write!(self.buffer, "{value}");
    }

    /// Writes `true` or `false`.
    pub fn boolean(&mut self, value: bool) {
        self.prefix();
        self.buffer.push_str(if value { "true" } else { "false" });
    }

    /// Writes `null`.
    pub fn null(&mut self) {
        self.prefix();
        self.buffer.push_str("null");
    }

    /// Writes an object built by `body`.
    pub fn object(&mut self, body: impl FnOnce(&mut Self)) {
        self.prefix();
        self.buffer.push('{');
        self.stack.push(true);
        body(self);
        self.stack.pop();
        self.buffer.push('}');
    }

    /// Writes an array built by `body`.
    pub fn array(&mut self, body: impl FnOnce(&mut Self)) {
        self.prefix();
        self.buffer.push('[');
        self.stack.push(true);
        body(self);
        self.stack.pop();
        self.buffer.push(']');
    }

    /// Returns the finished document.
    pub fn finish(self) -> String {
        self.buffer
    }

    fn prefix(&mut self) {
        if self.pending_value {
            self.pending_value = false;
            return;
        }
        if let Some(first) = self.stack.last_mut() {
            if *first {
                *first = false;
            } else {
                self.buffer.push(',');
            }
        }
    }
}

/// Quotes and escapes a string.
pub fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => {
                use fmt::Write as _;
                // Control characters below 0x20 are the only ones possible here.
                let _ = write!(out, "\\u{:04x}", u32::from(ch));
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn nests_objects_and_arrays() {
        let mut json = JsonOut::new();
        json.object(|out| {
            out.key("name");
            out.string("a \"quoted\" \\ path");
            out.key("count");
            out.number(3u64);
            out.key("ok");
            out.boolean(true);
            out.key("nothing");
            out.null();
            out.key("files");
            out.array(|out| {
                out.object(|out| {
                    out.key("index");
                    out.number(0);
                });
                out.object(|out| {
                    out.key("index");
                    out.number(1);
                });
            });
        });
        assert_eq!(
            json.finish(),
            r#"{"name":"a \"quoted\" \\ path","count":3,"ok":true,"nothing":null,"files":[{"index":0},{"index":1}]}"#
        );
    }

    #[test]
    fn escapes_control_characters() {
        assert_eq!(quote("a\nb\tc"), "\"a\\nb\\tc\"");
        assert_eq!(quote("\u{1}"), "\"\\u0001\"");
    }
}
