//! Plan task 9.0 · tool calls a model wrote as text.
//!
//! DeepSeek sometimes answers with its native tool-call markup (DSML) in the message text instead
//! of the API's `tool_calls` field, often in a degraded form (`｜｜DSML｜｜ parameter`, with doubled
//! bars and spaces). Left as text it enters history, the model imitates it on later turns, and the
//! run loops until its turn limit. The provider keeps the markup out of the text: it parses it into
//! real tool calls, or — when it cannot — fails the call as a retryable protocol error so the
//! kernel's recovery asks again. Mirrors `node/src/providers/text-tool-calls.ts`.

use serde_json::{Map, Value};

use super::provider_error::{ProviderError, ProviderErrorKind};

/// Markup that does not parse fails like the Anthropic adapter's textual-tool-call guard (SPC-022).
pub const TEXTUAL_TOOL_CALL_CODE: &str = "textual_tool_call";
pub const TEXTUAL_TOOL_CALL_MESSAGE: &str =
    "Provider emitted a tool call as text instead of a native tool block";

const PARTIAL_LOOKBACK: usize = 24;

/// A tool call parsed from markup.
#[derive(Debug, Clone, PartialEq)]
pub struct TextToolCall {
    pub name: String,
    pub arguments: Map<String, Value>,
}

fn is_bar(c: char) -> bool {
    c == '｜' || c == '|'
}

/// Where a scan of a marker prefix ended.
enum Scan {
    /// The text ran out while it still matched (it may continue in the next chunk).
    Partial,
    /// A full `<` [/] bars `DSML` prefix; the byte offset just after `DSML`.
    Marker { close: bool, end: usize },
    NoMatch,
}

/// Scan `<` + optional `/` + bars + `DSML` at the start of `text` (which begins with `<`).
fn scan_marker(text: &str) -> Scan {
    let mut chars = text.char_indices().peekable();
    chars.next(); // '<'
    let skip_ws = |chars: &mut std::iter::Peekable<std::str::CharIndices>| {
        while chars.peek().is_some_and(|(_, c)| c.is_whitespace()) {
            chars.next();
        }
    };
    skip_ws(&mut chars);
    let mut close = false;
    if chars.peek().is_some_and(|(_, c)| *c == '/') {
        close = true;
        chars.next();
        skip_ws(&mut chars);
    }
    let mut bars = 0;
    while chars.peek().is_some_and(|(_, c)| is_bar(*c)) {
        bars += 1;
        chars.next();
    }
    if chars.peek().is_none() {
        return Scan::Partial;
    }
    if bars == 0 {
        return Scan::NoMatch;
    }
    skip_ws(&mut chars);
    for expected in ['D', 'S', 'M', 'L'] {
        match chars.next() {
            None => return Scan::Partial,
            Some((_, c)) if c.eq_ignore_ascii_case(&expected) => {}
            Some(_) => return Scan::NoMatch,
        }
    }
    let end = chars.peek().map_or(text.len(), |(i, _)| *i);
    Scan::Marker { close, end }
}

/// Byte index of the first DSML marker in `text`.
pub fn markup_start(text: &str) -> Option<usize> {
    text.match_indices('<')
        .map(|(i, _)| i)
        .find(|&i| matches!(scan_marker(&text[i..]), Scan::Marker { .. }))
}

/// How many trailing bytes of `text` could begin a marker and must wait for the next chunk.
pub fn partial_markup_length(text: &str) -> usize {
    let tail_start = text
        .char_indices()
        .rev()
        .nth(PARTIAL_LOOKBACK - 1)
        .map_or(0, |(i, _)| i);
    let Some(at) = text[tail_start..].rfind('<') else {
        return 0;
    };
    let at = tail_start + at;
    match scan_marker(&text[at..]) {
        Scan::Partial => text.len() - at,
        _ => 0,
    }
}

struct Tag<'a> {
    start: usize,
    end: usize,
    close: bool,
    name: String,
    attrs: &'a str,
}

/// Every complete DSML tag in `markup`, in order.
fn tags(markup: &str) -> Vec<Tag<'_>> {
    let mut out = Vec::new();
    for (start, _) in markup.match_indices('<') {
        let Scan::Marker { close, end } = scan_marker(&markup[start..]) else {
            continue;
        };
        let rest = &markup[start + end..];
        let after_bars = rest.trim_start_matches(|c: char| c.is_whitespace() || is_bar(c));
        if after_bars.len() == rest.len() {
            continue; // no bars after DSML
        }
        let name: String = after_bars
            .chars()
            .take_while(|c| c.is_ascii_alphabetic() || *c == '_')
            .collect();
        if name.is_empty() {
            continue;
        }
        let after_name = &after_bars[name.len()..];
        let Some(gt) = after_name.find('>') else {
            continue;
        };
        let attrs = &after_name[..gt];
        let tag_end = markup.len() - after_name.len() + gt + 1;
        out.push(Tag { start, end: tag_end, close, name: name.to_ascii_lowercase(), attrs });
    }
    out
}

fn attribute<'a>(attrs: &'a str, name: &str) -> Option<&'a str> {
    let lower = attrs.to_ascii_lowercase();
    let mut from = 0;
    while let Some(found) = lower[from..].find(name) {
        let at = from + found;
        from = at + name.len();
        let boundary = at == 0
            || !lower[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        if !boundary {
            continue;
        }
        let rest = attrs[at + name.len()..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix('"') else {
            continue;
        };
        let close = rest.find('"')?;
        return Some(&rest[..close]);
    }
    None
}

fn parameter_value(raw: &str, is_string: bool) -> Value {
    if is_string {
        return Value::String(raw.to_string());
    }
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}

/// The tool calls in `markup`, or `None` when it is not a complete, well-formed set of calls.
pub fn parse_text_tool_calls(markup: &str) -> Option<Vec<TextToolCall>> {
    let mut calls = Vec::new();
    let mut current: Option<TextToolCall> = None;
    let mut parameter: Option<(String, bool, usize)> = None;
    for tag in tags(markup) {
        match tag.name.as_str() {
            "invoke" if !tag.close => {
                let name = attribute(tag.attrs, "name").filter(|n| !n.is_empty())?;
                if current.is_some() || parameter.is_some() {
                    return None;
                }
                current = Some(TextToolCall { name: name.to_string(), arguments: Map::new() });
            }
            "invoke" => {
                if parameter.is_some() {
                    return None;
                }
                calls.push(current.take()?);
            }
            "parameter" if !tag.close => {
                let name = attribute(tag.attrs, "name").filter(|n| !n.is_empty())?;
                if current.is_none() || parameter.is_some() {
                    return None;
                }
                let is_string = attribute(tag.attrs, "string") != Some("false");
                parameter = Some((name.to_string(), is_string, tag.end));
            }
            "parameter" => {
                let (name, is_string, start) = parameter.take()?;
                let value = parameter_value(&markup[start..tag.start], is_string);
                current.as_mut()?.arguments.insert(name, value);
            }
            // Wrappers (function_calls, tool_calls, a bare calls) carry nothing.
            _ => {}
        }
    }
    if current.is_some() || parameter.is_some() || calls.is_empty() {
        return None;
    }
    Some(calls)
}

/// A call id for a parsed call: unique, so read-backs and tool results never collide.
pub fn text_tool_call_id() -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    format!("call_text_{}", &id[..10])
}

pub fn textual_tool_call_error(provider: &str) -> ProviderError {
    let mut error =
        ProviderError::new(provider, ProviderErrorKind::Protocol, true, TEXTUAL_TOOL_CALL_MESSAGE);
    error.provider_code = Some(TEXTUAL_TOOL_CALL_CODE.to_string());
    error
}

/// Streams content through, holding tool-call markup out of the visible text.
#[derive(Debug, Default)]
pub struct TextToolCallFilter {
    watch: bool,
    held: String,
    markup: Option<String>,
}

impl TextToolCallFilter {
    pub fn new(watch: bool) -> Self {
        Self { watch, ..Self::default() }
    }

    /// The part of `text` that is safe to show now.
    pub fn feed(&mut self, text: &str) -> String {
        if !self.watch {
            return text.to_string();
        }
        if let Some(markup) = self.markup.as_mut() {
            markup.push_str(text);
            return String::new();
        }
        let combined = std::mem::take(&mut self.held) + text;
        if let Some(at) = markup_start(&combined) {
            self.markup = Some(combined[at..].to_string());
            return combined[..at].to_string();
        }
        let keep = partial_markup_length(&combined);
        let cut = combined.len() - keep;
        self.held = combined[cut..].to_string();
        combined[..cut].to_string()
    }

    /// At the end of the turn: the held-back text, and the calls the markup held (empty without
    /// markup). `Err` when there was markup that does not parse.
    pub fn finish(&mut self) -> Result<(String, Vec<TextToolCall>), ()> {
        let held = std::mem::take(&mut self.held);
        match self.markup.take() {
            None => Ok((held, Vec::new())),
            Some(markup) => parse_text_tool_calls(&markup).map(|calls| (held, calls)).ok_or(()),
        }
    }
}
