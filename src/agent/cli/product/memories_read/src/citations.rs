use crate::product::protocol::ThreadId;
use crate::product::protocol::memory_citation::MemoryCitation;
use crate::product::protocol::memory_citation::MemoryCitationEntry;
use std::collections::HashSet;

const OPEN_TAG: &str = "<oai-mem-citation>";
const CLOSE_TAG: &str = "</oai-mem-citation>";
const CITATION_ENTRIES_OPEN_TAG: &str = "<citation_entries>";
const ROLLOUT_IDS_OPEN_TAG: &str = "<rollout_ids>";
const THREAD_IDS_OPEN_TAG: &str = "<thread_ids>";
const STRUCTURED_INNER_OPEN_TAGS: [&str; 3] = [
    CITATION_ENTRIES_OPEN_TAG,
    ROLLOUT_IDS_OPEN_TAG,
    THREAD_IDS_OPEN_TAG,
];
const STRUCTURED_INNER_CLOSE_TAGS: [&str; 3] =
    ["</citation_entries>", "</rollout_ids>", "</thread_ids>"];
const CONTINUATION_CLOSE_TAGS: [&str; 4] = [
    STRUCTURED_INNER_CLOSE_TAGS[0],
    STRUCTURED_INNER_CLOSE_TAGS[1],
    STRUCTURED_INNER_CLOSE_TAGS[2],
    CLOSE_TAG,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JsonTextTransition {
    Continue,
    Complete,
    Invalid,
}

#[derive(Debug)]
struct MemoryCitationTextContext {
    root_value_possible: bool,
    active_json: Option<JsonTracker>,
}

impl Default for MemoryCitationTextContext {
    fn default() -> Self {
        Self {
            root_value_possible: true,
            active_json: None,
        }
    }
}

impl MemoryCitationTextContext {
    fn reset(&mut self) {
        self.root_value_possible = true;
        self.active_json = None;
    }

    fn inside_json_string(&self) -> bool {
        self.active_json
            .as_ref()
            .is_some_and(JsonTracker::inside_string)
    }

    fn abandon_json(&mut self) {
        self.root_value_possible = false;
        self.active_json = None;
    }

    fn consume_visible(&mut self, text: &str) {
        for ch in text.chars() {
            if let Some(json) = self.active_json.as_mut() {
                match json.push(ch) {
                    JsonTextTransition::Continue => continue,
                    JsonTextTransition::Complete => {
                        self.active_json = None;
                        self.root_value_possible = false;
                    }
                    JsonTextTransition::Invalid => {
                        self.active_json = None;
                        self.root_value_possible = false;
                        self.start_json_if_possible(ch);
                    }
                }
                continue;
            }

            self.start_json_if_possible(ch);
        }
    }

    fn take_json(&mut self) -> Option<JsonTracker> {
        self.active_json.take()
    }

    fn restore_json(&mut self, json: JsonTracker) {
        self.active_json = Some(json);
        self.root_value_possible = false;
    }

    fn complete_json(&mut self) {
        self.active_json = None;
        self.root_value_possible = false;
    }

    fn start_json_if_possible(&mut self, ch: char) {
        if matches!(ch, '{' | '[') || (self.root_value_possible && ch == '"') {
            self.active_json = Some(JsonTracker::new(ch));
            self.root_value_possible = false;
        } else if !ch.is_whitespace() {
            self.root_value_possible = false;
        }
    }
}

#[derive(Debug)]
struct JsonTracker {
    source: String,
    root: JsonRootState,
    stack: Vec<JsonContainer>,
    token: Option<JsonToken>,
}

impl JsonTracker {
    fn new(start: char) -> Self {
        let mut tracker = Self {
            source: String::new(),
            root: JsonRootState::Value,
            stack: Vec::new(),
            token: None,
        };
        let transition = tracker.push(start);
        debug_assert_eq!(transition, JsonTextTransition::Continue);
        tracker
    }

    fn inside_string(&self) -> bool {
        matches!(self.token, Some(JsonToken::String(_)))
    }

    fn push(&mut self, ch: char) -> JsonTextTransition {
        self.source.push(ch);

        loop {
            let Some(token) = self.token.take() else {
                return self.consume_structural_char(ch);
            };

            match token {
                JsonToken::String(mut string) => match string.escape {
                    JsonStringEscape::None => {
                        if matches!(ch, '\u{0000}'..='\u{001f}') {
                            return JsonTextTransition::Invalid;
                        }
                        match ch {
                            '\\' => {
                                string.escape = JsonStringEscape::Escaped;
                                self.token = Some(JsonToken::String(string));
                                return JsonTextTransition::Continue;
                            }
                            '"' => {
                                return match string.role {
                                    JsonStringRole::Key => self.finish_key(),
                                    JsonStringRole::Value => self.finish_value(),
                                };
                            }
                            _ => {
                                self.token = Some(JsonToken::String(string));
                                return JsonTextTransition::Continue;
                            }
                        }
                    }
                    JsonStringEscape::Escaped => {
                        string.escape = match ch {
                            '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't' => {
                                JsonStringEscape::None
                            }
                            'u' => JsonStringEscape::Unicode(4),
                            _ => return JsonTextTransition::Invalid,
                        };
                        self.token = Some(JsonToken::String(string));
                        return JsonTextTransition::Continue;
                    }
                    JsonStringEscape::Unicode(remaining) => {
                        if !ch.is_ascii_hexdigit() {
                            return JsonTextTransition::Invalid;
                        }
                        string.escape = if remaining == 1 {
                            JsonStringEscape::None
                        } else {
                            JsonStringEscape::Unicode(remaining - 1)
                        };
                        self.token = Some(JsonToken::String(string));
                        return JsonTextTransition::Continue;
                    }
                },
                JsonToken::Number(number) => match number.advance(ch) {
                    JsonNumberTransition::Continue(next) => {
                        self.token = Some(JsonToken::Number(next));
                        return JsonTextTransition::Continue;
                    }
                    JsonNumberTransition::Complete => {
                        let transition = self.finish_value();
                        if !matches!(transition, JsonTextTransition::Continue) {
                            return transition;
                        }
                    }
                    JsonNumberTransition::Invalid => return JsonTextTransition::Invalid,
                },
                JsonToken::Keyword { literal, index } => {
                    if index == literal.len() {
                        if !is_json_delimiter(ch) {
                            return JsonTextTransition::Invalid;
                        }
                        let transition = self.finish_value();
                        if !matches!(transition, JsonTextTransition::Continue) {
                            return transition;
                        }
                    } else if ch.is_ascii() && literal[index] == ch as u8 {
                        self.token = Some(JsonToken::Keyword {
                            literal,
                            index: index + 1,
                        });
                        return JsonTextTransition::Continue;
                    } else {
                        return JsonTextTransition::Invalid;
                    }
                }
            }
        }
    }

    fn consume_structural_char(&mut self, ch: char) -> JsonTextTransition {
        if is_json_whitespace(ch) {
            return JsonTextTransition::Continue;
        }

        match ch {
            '{' if self.expects_value() => {
                self.stack
                    .push(JsonContainer::Object(JsonObjectState::KeyOrEnd));
                JsonTextTransition::Continue
            }
            '[' if self.expects_value() => {
                self.stack
                    .push(JsonContainer::Array(JsonArrayState::ValueOrEnd));
                JsonTextTransition::Continue
            }
            '}' => self.close_object(),
            ']' => self.close_array(),
            '"' => {
                if matches!(
                    self.stack.last(),
                    Some(JsonContainer::Object(JsonObjectState::KeyOrEnd))
                ) {
                    self.token = Some(JsonToken::String(JsonString {
                        role: JsonStringRole::Key,
                        escape: JsonStringEscape::None,
                    }));
                    JsonTextTransition::Continue
                } else if self.expects_value() {
                    self.token = Some(JsonToken::String(JsonString {
                        role: JsonStringRole::Value,
                        escape: JsonStringEscape::None,
                    }));
                    JsonTextTransition::Continue
                } else {
                    JsonTextTransition::Invalid
                }
            }
            ':' => {
                let Some(JsonContainer::Object(state)) = self.stack.last_mut() else {
                    return JsonTextTransition::Invalid;
                };
                if !matches!(*state, JsonObjectState::Colon) {
                    return JsonTextTransition::Invalid;
                }
                *state = JsonObjectState::Value;
                JsonTextTransition::Continue
            }
            ',' => match self.stack.last_mut() {
                Some(JsonContainer::Object(state))
                    if matches!(*state, JsonObjectState::CommaOrEnd) =>
                {
                    *state = JsonObjectState::KeyOrEnd;
                    JsonTextTransition::Continue
                }
                Some(JsonContainer::Array(state))
                    if matches!(*state, JsonArrayState::CommaOrEnd) =>
                {
                    *state = JsonArrayState::ValueOrEnd;
                    JsonTextTransition::Continue
                }
                Some(JsonContainer::Object(_)) | Some(JsonContainer::Array(_)) | None => {
                    JsonTextTransition::Invalid
                }
            },
            '-' | '0'..='9' if self.expects_value() => {
                self.token = JsonNumberState::new(ch).map(JsonToken::Number);
                JsonTextTransition::Continue
            }
            't' if self.expects_value() => {
                self.token = Some(JsonToken::Keyword {
                    literal: b"true",
                    index: 1,
                });
                JsonTextTransition::Continue
            }
            'f' if self.expects_value() => {
                self.token = Some(JsonToken::Keyword {
                    literal: b"false",
                    index: 1,
                });
                JsonTextTransition::Continue
            }
            'n' if self.expects_value() => {
                self.token = Some(JsonToken::Keyword {
                    literal: b"null",
                    index: 1,
                });
                JsonTextTransition::Continue
            }
            _ => JsonTextTransition::Invalid,
        }
    }

    fn expects_value(&self) -> bool {
        match self.stack.last() {
            Some(JsonContainer::Object(state)) => matches!(*state, JsonObjectState::Value),
            Some(JsonContainer::Array(state)) => matches!(*state, JsonArrayState::ValueOrEnd),
            None => matches!(self.root, JsonRootState::Value),
        }
    }

    fn close_object(&mut self) -> JsonTextTransition {
        let Some(JsonContainer::Object(state)) = self.stack.last() else {
            return JsonTextTransition::Invalid;
        };
        if !matches!(
            *state,
            JsonObjectState::KeyOrEnd | JsonObjectState::CommaOrEnd
        ) {
            return JsonTextTransition::Invalid;
        }
        self.stack.pop();
        self.finish_value()
    }

    fn close_array(&mut self) -> JsonTextTransition {
        let Some(JsonContainer::Array(state)) = self.stack.last() else {
            return JsonTextTransition::Invalid;
        };
        if !matches!(
            *state,
            JsonArrayState::ValueOrEnd | JsonArrayState::CommaOrEnd
        ) {
            return JsonTextTransition::Invalid;
        }
        self.stack.pop();
        self.finish_value()
    }

    fn finish_key(&mut self) -> JsonTextTransition {
        let Some(JsonContainer::Object(state)) = self.stack.last_mut() else {
            return JsonTextTransition::Invalid;
        };
        if !matches!(*state, JsonObjectState::KeyOrEnd) {
            return JsonTextTransition::Invalid;
        }
        *state = JsonObjectState::Colon;
        JsonTextTransition::Continue
    }

    fn finish_value(&mut self) -> JsonTextTransition {
        match self.stack.last_mut() {
            Some(JsonContainer::Object(state)) if matches!(*state, JsonObjectState::Value) => {
                *state = JsonObjectState::CommaOrEnd;
                JsonTextTransition::Continue
            }
            Some(JsonContainer::Array(state)) if matches!(*state, JsonArrayState::ValueOrEnd) => {
                *state = JsonArrayState::CommaOrEnd;
                JsonTextTransition::Continue
            }
            Some(JsonContainer::Object(_)) | Some(JsonContainer::Array(_)) => {
                JsonTextTransition::Invalid
            }
            None if matches!(self.root, JsonRootState::Value) => {
                self.root = JsonRootState::Complete;
                if serde_json::from_str::<serde_json::Value>(&self.source).is_ok() {
                    JsonTextTransition::Complete
                } else {
                    JsonTextTransition::Invalid
                }
            }
            None => JsonTextTransition::Invalid,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum JsonRootState {
    Value,
    Complete,
}

#[derive(Debug)]
enum JsonContainer {
    Object(JsonObjectState),
    Array(JsonArrayState),
}

#[derive(Debug, Clone, Copy)]
enum JsonObjectState {
    KeyOrEnd,
    Colon,
    Value,
    CommaOrEnd,
}

#[derive(Debug, Clone, Copy)]
enum JsonArrayState {
    ValueOrEnd,
    CommaOrEnd,
}

#[derive(Debug)]
enum JsonToken {
    String(JsonString),
    Number(JsonNumberState),
    Keyword {
        literal: &'static [u8],
        index: usize,
    },
}

#[derive(Debug)]
struct JsonString {
    role: JsonStringRole,
    escape: JsonStringEscape,
}

#[derive(Debug)]
enum JsonStringRole {
    Key,
    Value,
}

#[derive(Debug)]
enum JsonStringEscape {
    None,
    Escaped,
    Unicode(u8),
}

#[derive(Debug, Clone, Copy)]
enum JsonNumberState {
    Sign,
    Zero,
    Integer,
    FractionStart,
    Fraction,
    ExponentStart,
    ExponentSign,
    Exponent,
}

impl JsonNumberState {
    fn new(ch: char) -> Option<Self> {
        match ch {
            '-' => Some(Self::Sign),
            '0' => Some(Self::Zero),
            '1'..='9' => Some(Self::Integer),
            _ => None,
        }
    }

    fn advance(self, ch: char) -> JsonNumberTransition {
        match self {
            Self::Sign => match ch {
                '0' => JsonNumberTransition::Continue(Self::Zero),
                '1'..='9' => JsonNumberTransition::Continue(Self::Integer),
                _ => JsonNumberTransition::Invalid,
            },
            Self::Zero => match ch {
                '.' => JsonNumberTransition::Continue(Self::FractionStart),
                'e' | 'E' => JsonNumberTransition::Continue(Self::ExponentStart),
                _ if is_json_delimiter(ch) => JsonNumberTransition::Complete,
                _ => JsonNumberTransition::Invalid,
            },
            Self::Integer => match ch {
                '0'..='9' => JsonNumberTransition::Continue(Self::Integer),
                '.' => JsonNumberTransition::Continue(Self::FractionStart),
                'e' | 'E' => JsonNumberTransition::Continue(Self::ExponentStart),
                _ if is_json_delimiter(ch) => JsonNumberTransition::Complete,
                _ => JsonNumberTransition::Invalid,
            },
            Self::FractionStart => match ch {
                '0'..='9' => JsonNumberTransition::Continue(Self::Fraction),
                _ => JsonNumberTransition::Invalid,
            },
            Self::Fraction => match ch {
                '0'..='9' => JsonNumberTransition::Continue(Self::Fraction),
                'e' | 'E' => JsonNumberTransition::Continue(Self::ExponentStart),
                _ if is_json_delimiter(ch) => JsonNumberTransition::Complete,
                _ => JsonNumberTransition::Invalid,
            },
            Self::ExponentStart => match ch {
                '+' | '-' => JsonNumberTransition::Continue(Self::ExponentSign),
                '0'..='9' => JsonNumberTransition::Continue(Self::Exponent),
                _ => JsonNumberTransition::Invalid,
            },
            Self::ExponentSign => match ch {
                '0'..='9' => JsonNumberTransition::Continue(Self::Exponent),
                _ => JsonNumberTransition::Invalid,
            },
            Self::Exponent => match ch {
                '0'..='9' => JsonNumberTransition::Continue(Self::Exponent),
                _ if is_json_delimiter(ch) => JsonNumberTransition::Complete,
                _ => JsonNumberTransition::Invalid,
            },
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum JsonNumberTransition {
    Continue(JsonNumberState),
    Complete,
    Invalid,
}

fn is_json_whitespace(ch: char) -> bool {
    matches!(ch, ' ' | '\n' | '\r' | '\t')
}

fn is_json_delimiter(ch: char) -> bool {
    is_json_whitespace(ch) || matches!(ch, ',' | '}' | ']')
}

#[derive(Debug, Default)]
struct MemoryCitationScanner {
    text_context: MemoryCitationTextContext,
    mode: CitationScanMode,
    collect_citations: bool,
    citations: Vec<String>,
    #[cfg(test)]
    work_units: usize,
}

impl MemoryCitationScanner {
    fn new(collect_citations: bool) -> Self {
        Self {
            collect_citations,
            ..Self::default()
        }
    }

    fn reset(&mut self) {
        self.text_context.reset();
        self.mode = CitationScanMode::default();
        self.citations.clear();
        #[cfg(test)]
        {
            self.work_units = 0;
        }
    }

    fn push(&mut self, text: &str) -> String {
        let mut output = String::new();
        self.consume_fragment(text, &mut output);
        output
    }

    fn finish(&mut self) -> String {
        let mut output = String::new();

        loop {
            match std::mem::take(&mut self.mode) {
                CitationScanMode::Text(prefix) => {
                    if !prefix.value.is_empty() {
                        self.emit_visible(&prefix.value, &mut output);
                    }
                    self.mode = CitationScanMode::default();
                    return output;
                }
                CitationScanMode::Candidate(candidate) => {
                    if candidate.prefix.is_structural() {
                        self.text_context.abandon_json();
                        self.mode = CitationScanMode::default();
                        return output;
                    }
                    self.release_outer_literal(candidate, &mut output);
                }
                CitationScanMode::Deferred(candidate) => {
                    if candidate.should_suppress_on_finish() {
                        self.text_context.abandon_json();
                        self.mode = CitationScanMode::default();
                        return output;
                    }
                    self.release_deferred_literal(candidate, &mut output);
                }
                CitationScanMode::Suppressing(_) => {
                    self.text_context.abandon_json();
                    self.mode = CitationScanMode::default();
                    return output;
                }
            }
        }
    }

    fn into_citations(self) -> Vec<String> {
        self.citations
    }

    #[cfg(test)]
    fn work_units(&self) -> usize {
        self.work_units
    }

    fn consume_fragment(&mut self, text: &str, output: &mut String) {
        for ch in text.chars() {
            self.consume_char(ch, output);
        }
    }

    fn consume_char(&mut self, ch: char, output: &mut String) {
        self.record_work();

        match std::mem::take(&mut self.mode) {
            CitationScanMode::Text(prefix) => self.consume_text_char(prefix, ch, output),
            CitationScanMode::Candidate(candidate) => {
                self.consume_outer_candidate_char(candidate, ch, output);
            }
            CitationScanMode::Deferred(candidate) => {
                self.consume_deferred_candidate_char(candidate, ch, output);
            }
            CitationScanMode::Suppressing(mut citation) => {
                if citation.push(ch) {
                    self.record_completed_citation(citation);
                    self.mode = CitationScanMode::default();
                } else {
                    self.mode = CitationScanMode::Suppressing(citation);
                }
            }
        }
    }

    fn consume_text_char(&mut self, mut prefix: OpenTagPrefix, ch: char, output: &mut String) {
        if prefix.value.is_empty() {
            if ch == '<' {
                prefix.value.push(ch);
                self.mode = CitationScanMode::Text(prefix);
            } else {
                self.emit_visible_char(ch, output);
                self.mode = CitationScanMode::Text(prefix);
            }
            return;
        }

        let prefix_len = prefix.value.len();
        if ch.is_ascii() && OPEN_TAG.as_bytes()[prefix_len] == ch as u8 {
            prefix.value.push(ch);
            if prefix.value.len() == OPEN_TAG.len() {
                if self.text_context.inside_json_string() {
                    let Some(json) = self.text_context.take_json() else {
                        unreachable!("active JSON string should have a tracker");
                    };
                    self.mode = CitationScanMode::Deferred(DeferredJsonCandidate::new(json));
                } else {
                    self.mode = CitationScanMode::Candidate(OuterTagCandidate::new());
                }
            } else {
                self.mode = CitationScanMode::Text(prefix);
            }
            return;
        }

        let literal = std::mem::take(&mut prefix.value);
        self.emit_visible(&literal, output);
        self.mode = CitationScanMode::Text(prefix);
        self.consume_char(ch, output);
    }

    fn consume_outer_candidate_char(
        &mut self,
        mut candidate: OuterTagCandidate,
        ch: char,
        output: &mut String,
    ) {
        candidate.body.push(ch);
        match candidate.prefix.push(ch) {
            PrefixTransition::Pending => {
                self.mode = CitationScanMode::Candidate(candidate);
            }
            PrefixTransition::Literal => self.release_outer_literal(candidate, output),
            PrefixTransition::Structural => {
                self.begin_suppressing(candidate.full_text());
            }
        }
    }

    fn consume_deferred_candidate_char(
        &mut self,
        mut candidate: DeferredJsonCandidate,
        ch: char,
        output: &mut String,
    ) {
        match candidate.phase {
            DeferredJsonPhase::InsideJson => {
                candidate.content.push(ch);
                let json_transition = candidate.json.push(ch);
                let was_structural = candidate.prefix.is_structural();
                let prefix_transition = if was_structural {
                    PrefixTransition::Pending
                } else {
                    candidate.prefix.push(ch)
                };

                if matches!(prefix_transition, PrefixTransition::Literal) {
                    self.release_deferred_literal(candidate, output);
                    return;
                }

                if matches!(json_transition, JsonTextTransition::Invalid) {
                    if candidate.prefix.is_structural() {
                        self.begin_suppressing(candidate.full_text());
                    } else {
                        candidate.phase = DeferredJsonPhase::JsonInvalid;
                        self.mode = CitationScanMode::Deferred(candidate);
                    }
                    return;
                }

                if was_structural && candidate.outer_close.push(ch) {
                    candidate.outer_closed_inside_json = true;
                }

                if matches!(json_transition, JsonTextTransition::Complete) {
                    if candidate.outer_closed_inside_json {
                        self.release_deferred_literal(candidate, output);
                        return;
                    }
                    candidate.phase = if candidate.prefix.is_structural() {
                        DeferredJsonPhase::AfterJson(AfterJsonState::AwaitingContinuation(
                            ContinuationProbe::default(),
                        ))
                    } else {
                        DeferredJsonPhase::AfterJson(AfterJsonState::PendingPrefix)
                    };
                }
                self.mode = CitationScanMode::Deferred(candidate);
            }
            DeferredJsonPhase::JsonInvalid => {
                candidate.content.push(ch);
                match candidate.prefix.push(ch) {
                    PrefixTransition::Pending => {
                        self.mode = CitationScanMode::Deferred(candidate);
                    }
                    PrefixTransition::Literal => self.release_deferred_literal(candidate, output),
                    PrefixTransition::Structural => self.begin_suppressing(candidate.full_text()),
                }
            }
            DeferredJsonPhase::AfterJson(AfterJsonState::PendingPrefix) => {
                candidate.after.push(ch);
                match candidate.prefix.push(ch) {
                    PrefixTransition::Pending => {
                        self.mode = CitationScanMode::Deferred(candidate);
                    }
                    PrefixTransition::Literal => self.release_deferred_literal(candidate, output),
                    PrefixTransition::Structural => self.begin_suppressing(candidate.full_text()),
                }
            }
            DeferredJsonPhase::AfterJson(AfterJsonState::AwaitingContinuation(ref mut probe)) => {
                candidate.after.push(ch);
                match probe.push(ch) {
                    ContinuationTransition::Pending => {
                        self.mode = CitationScanMode::Deferred(candidate);
                    }
                    ContinuationTransition::Continuation => {
                        self.begin_suppressing(candidate.full_text());
                    }
                    ContinuationTransition::Literal => {
                        self.release_deferred_literal(candidate, output);
                    }
                }
            }
        }
    }

    fn release_outer_literal(&mut self, candidate: OuterTagCandidate, output: &mut String) {
        self.emit_visible(OPEN_TAG, output);
        self.mode = CitationScanMode::default();
        self.consume_fragment(&candidate.body, output);
    }

    fn release_deferred_literal(&mut self, candidate: DeferredJsonCandidate, output: &mut String) {
        let DeferredJsonCandidate {
            content,
            after,
            json,
            phase,
            ..
        } = candidate;

        match phase {
            DeferredJsonPhase::InsideJson => self.text_context.restore_json(json),
            DeferredJsonPhase::JsonInvalid => self.text_context.abandon_json(),
            DeferredJsonPhase::AfterJson(_) => self.text_context.complete_json(),
        }

        output.push_str(&content);
        self.mode = CitationScanMode::default();
        self.consume_fragment(&after, output);
    }

    fn begin_suppressing(&mut self, initial: String) {
        self.text_context.abandon_json();
        let citation = SuppressedCitation::new(initial, self.collect_citations);
        if citation.initial_is_complete() {
            self.record_completed_citation(citation);
            self.mode = CitationScanMode::default();
        } else {
            self.mode = CitationScanMode::Suppressing(citation);
        }
    }

    fn record_completed_citation(&mut self, mut citation: SuppressedCitation) {
        if let Some(citation) = citation.take_captured() {
            self.citations.push(citation);
        }
    }

    fn emit_visible_char(&mut self, ch: char, output: &mut String) {
        let mut encoded = [0; 4];
        self.emit_visible(ch.encode_utf8(&mut encoded), output);
    }

    fn emit_visible(&mut self, text: &str, output: &mut String) {
        self.text_context.consume_visible(text);
        output.push_str(text);
    }

    fn record_work(&mut self) {
        #[cfg(test)]
        {
            self.work_units += 1;
        }
    }
}

#[derive(Debug)]
enum CitationScanMode {
    Text(OpenTagPrefix),
    Candidate(OuterTagCandidate),
    Deferred(DeferredJsonCandidate),
    Suppressing(SuppressedCitation),
}

impl Default for CitationScanMode {
    fn default() -> Self {
        Self::Text(OpenTagPrefix::default())
    }
}

#[derive(Debug, Default)]
struct OpenTagPrefix {
    value: String,
}

#[derive(Debug)]
struct OuterTagCandidate {
    body: String,
    prefix: StructuredPrefixMatcher,
}

impl OuterTagCandidate {
    fn new() -> Self {
        Self {
            body: String::new(),
            prefix: StructuredPrefixMatcher::default(),
        }
    }

    fn full_text(self) -> String {
        format!("{OPEN_TAG}{}", self.body)
    }
}

#[derive(Debug)]
struct DeferredJsonCandidate {
    content: String,
    after: String,
    json: JsonTracker,
    prefix: StructuredPrefixMatcher,
    outer_close: PatternMatcher,
    outer_closed_inside_json: bool,
    phase: DeferredJsonPhase,
}

impl DeferredJsonCandidate {
    fn new(mut json: JsonTracker) -> Self {
        for ch in OPEN_TAG.chars() {
            let transition = json.push(ch);
            debug_assert_eq!(transition, JsonTextTransition::Continue);
        }
        Self {
            content: OPEN_TAG.to_string(),
            after: String::new(),
            json,
            prefix: StructuredPrefixMatcher::default(),
            outer_close: PatternMatcher::new(CLOSE_TAG),
            outer_closed_inside_json: false,
            phase: DeferredJsonPhase::InsideJson,
        }
    }

    fn full_text(&self) -> String {
        format!("{}{}", self.content, self.after)
    }

    fn should_suppress_on_finish(&self) -> bool {
        matches!(
            self.phase,
            DeferredJsonPhase::InsideJson | DeferredJsonPhase::JsonInvalid
        ) && self.prefix.is_structural()
    }
}

#[derive(Debug)]
enum DeferredJsonPhase {
    InsideJson,
    JsonInvalid,
    AfterJson(AfterJsonState),
}

#[derive(Debug)]
enum AfterJsonState {
    PendingPrefix,
    AwaitingContinuation(ContinuationProbe),
}

#[derive(Debug, Default)]
struct StructuredPrefixMatcher {
    state: StructuredPrefixState,
}

impl StructuredPrefixMatcher {
    fn push(&mut self, ch: char) -> PrefixTransition {
        match &mut self.state {
            StructuredPrefixState::LeadingWhitespace => {
                if ch.is_whitespace() {
                    return PrefixTransition::Pending;
                }
                let mut active = [false; STRUCTURED_INNER_OPEN_TAGS.len()];
                let mut any_active = false;
                for (idx, tag) in STRUCTURED_INNER_OPEN_TAGS.iter().enumerate() {
                    if ch.is_ascii() && tag.as_bytes()[0] == ch as u8 {
                        active[idx] = true;
                        any_active = true;
                    }
                }
                if !any_active {
                    self.state = StructuredPrefixState::Literal;
                    PrefixTransition::Literal
                } else {
                    self.state = StructuredPrefixState::Matching {
                        position: 1,
                        active,
                    };
                    PrefixTransition::Pending
                }
            }
            StructuredPrefixState::Matching { position, active } => {
                let mut any_active = false;
                let mut complete = false;
                for (idx, tag) in STRUCTURED_INNER_OPEN_TAGS.iter().enumerate() {
                    if active[idx] && ch.is_ascii() && tag.as_bytes()[*position] == ch as u8 {
                        any_active = true;
                        if *position + 1 == tag.len() {
                            complete = true;
                        }
                    } else {
                        active[idx] = false;
                    }
                }
                if complete {
                    self.state = StructuredPrefixState::Structural;
                    PrefixTransition::Structural
                } else if any_active {
                    *position += 1;
                    PrefixTransition::Pending
                } else {
                    self.state = StructuredPrefixState::Literal;
                    PrefixTransition::Literal
                }
            }
            StructuredPrefixState::Structural => PrefixTransition::Pending,
            StructuredPrefixState::Literal => PrefixTransition::Literal,
        }
    }

    fn is_structural(&self) -> bool {
        matches!(self.state, StructuredPrefixState::Structural)
    }
}

#[derive(Debug, Default)]
enum StructuredPrefixState {
    #[default]
    LeadingWhitespace,
    Matching {
        position: usize,
        active: [bool; STRUCTURED_INNER_OPEN_TAGS.len()],
    },
    Structural,
    Literal,
}

#[derive(Debug, Clone, Copy)]
enum PrefixTransition {
    Pending,
    Structural,
    Literal,
}

#[derive(Debug, Default)]
struct ContinuationProbe {
    state: ContinuationProbeState,
}

impl ContinuationProbe {
    fn push(&mut self, ch: char) -> ContinuationTransition {
        match &mut self.state {
            ContinuationProbeState::LeadingWhitespace => {
                if ch.is_whitespace() {
                    return ContinuationTransition::Pending;
                }
                let mut active = [false; CONTINUATION_CLOSE_TAGS.len()];
                let mut any_active = false;
                for (idx, tag) in CONTINUATION_CLOSE_TAGS.iter().enumerate() {
                    if ch.is_ascii() && tag.as_bytes()[0] == ch as u8 {
                        active[idx] = true;
                        any_active = true;
                    }
                }
                if any_active {
                    self.state = ContinuationProbeState::Matching {
                        position: 1,
                        active,
                    };
                    ContinuationTransition::Pending
                } else {
                    self.state = ContinuationProbeState::Literal;
                    ContinuationTransition::Literal
                }
            }
            ContinuationProbeState::Matching { position, active } => {
                let mut any_active = false;
                let mut complete = false;
                for (idx, tag) in CONTINUATION_CLOSE_TAGS.iter().enumerate() {
                    if active[idx] && ch.is_ascii() && tag.as_bytes()[*position] == ch as u8 {
                        any_active = true;
                        if *position + 1 == tag.len() {
                            complete = true;
                        }
                    } else {
                        active[idx] = false;
                    }
                }
                if complete {
                    self.state = ContinuationProbeState::Continuation;
                    ContinuationTransition::Continuation
                } else if any_active {
                    *position += 1;
                    ContinuationTransition::Pending
                } else {
                    self.state = ContinuationProbeState::Literal;
                    ContinuationTransition::Literal
                }
            }
            ContinuationProbeState::Continuation => ContinuationTransition::Continuation,
            ContinuationProbeState::Literal => ContinuationTransition::Literal,
        }
    }
}

#[derive(Debug, Default)]
enum ContinuationProbeState {
    #[default]
    LeadingWhitespace,
    Matching {
        position: usize,
        active: [bool; CONTINUATION_CLOSE_TAGS.len()],
    },
    Continuation,
    Literal,
}

#[derive(Debug, Clone, Copy)]
enum ContinuationTransition {
    Pending,
    Continuation,
    Literal,
}

#[derive(Debug)]
struct SuppressedCitation {
    close: PatternMatcher,
    captured: Option<String>,
    complete: bool,
}

impl SuppressedCitation {
    fn new(initial: String, collect: bool) -> Self {
        let mut citation = Self {
            close: PatternMatcher::new(CLOSE_TAG),
            captured: collect.then(|| initial.clone()),
            complete: false,
        };
        for ch in initial.chars() {
            if citation.close.push(ch) {
                citation.complete = true;
                break;
            }
        }
        citation
    }

    fn initial_is_complete(&self) -> bool {
        self.complete
    }

    fn push(&mut self, ch: char) -> bool {
        if let Some(captured) = self.captured.as_mut() {
            captured.push(ch);
        }
        self.complete = self.close.push(ch);
        self.complete
    }

    fn take_captured(&mut self) -> Option<String> {
        self.captured.take()
    }
}

#[derive(Debug)]
struct PatternMatcher {
    pattern: &'static str,
    matched: usize,
}

impl PatternMatcher {
    fn new(pattern: &'static str) -> Self {
        Self {
            pattern,
            matched: 0,
        }
    }

    fn push(&mut self, ch: char) -> bool {
        let Some(byte) = ch.is_ascii().then_some(ch as u8) else {
            self.matched = 0;
            return false;
        };
        let pattern = self.pattern.as_bytes();

        if pattern[self.matched] == byte {
            self.matched += 1;
            if self.matched == pattern.len() {
                self.matched = 0;
                return true;
            }
            return false;
        }

        self.matched = usize::from(byte == pattern[0]);
        false
    }
}

#[derive(Debug, Default)]
pub(crate) struct MemoryCitationDeltaFilter {
    trailing_whitespace: String,
    scanner: MemoryCitationScanner,
}

impl MemoryCitationDeltaFilter {
    pub(crate) fn reset(&mut self) {
        self.trailing_whitespace.clear();
        self.scanner.reset();
    }

    pub(crate) fn push(&mut self, delta: &str) -> String {
        let visible = self.scanner.push(delta);
        self.emit_visible(&visible)
    }

    pub(crate) fn finish(&mut self) -> String {
        let visible = self.scanner.finish();
        let output = self.emit_visible(&visible);
        self.trailing_whitespace.clear();
        output
    }

    pub(crate) fn abort(&mut self) -> String {
        let visible = self.scanner.finish();
        let mut output = self.emit_visible(&visible);
        output.push_str(&self.trailing_whitespace);
        self.trailing_whitespace.clear();
        output
    }

    fn emit_visible(&mut self, visible: &str) -> String {
        let visible_end = visible.trim_end().len();
        let mut output = String::new();
        if visible_end == 0 {
            self.trailing_whitespace.push_str(visible);
        } else {
            output.push_str(&self.trailing_whitespace);
            self.trailing_whitespace.clear();
            output.push_str(&visible[..visible_end]);
            self.trailing_whitespace.push_str(&visible[visible_end..]);
        }
        output
    }
}

pub fn strip_memory_citation_block(text: &str) -> (String, Vec<String>) {
    let mut scanner = MemoryCitationScanner::new(/* collect_citations */ true);
    let mut stripped = scanner.push(text);
    stripped.push_str(&scanner.finish());
    (stripped.trim_end().to_string(), scanner.into_citations())
}

pub fn parse_memory_citation(citations: Vec<String>) -> Option<MemoryCitation> {
    let mut entries = Vec::new();
    let mut rollout_ids = Vec::new();
    let mut seen_rollout_ids = HashSet::new();

    for citation in citations {
        if let Some(entries_block) =
            extract_block(&citation, "<citation_entries>", "</citation_entries>")
        {
            entries.extend(
                entries_block
                    .lines()
                    .filter_map(parse_memory_citation_entry),
            );
        }

        if let Some(ids_block) = extract_ids_block(&citation) {
            for id in ids_block
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
            {
                if seen_rollout_ids.insert(id.to_string()) {
                    rollout_ids.push(id.to_string());
                }
            }
        }
    }

    if entries.is_empty() && rollout_ids.is_empty() {
        None
    } else {
        Some(MemoryCitation {
            entries,
            rollout_ids,
        })
    }
}

pub fn thread_ids_from_memory_citation(memory_citation: &MemoryCitation) -> Vec<ThreadId> {
    memory_citation
        .rollout_ids
        .iter()
        .filter_map(|id| ThreadId::try_from(id.as_str()).ok())
        .collect()
}

fn parse_memory_citation_entry(line: &str) -> Option<MemoryCitationEntry> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }

    let (location, note) = line.rsplit_once("|note=[")?;
    let note = note.strip_suffix(']')?.trim().to_string();
    let (path, line_range) = location.rsplit_once(':')?;
    let (line_start, line_end) = line_range.split_once('-')?;

    Some(MemoryCitationEntry {
        path: path.trim().to_string(),
        line_start: line_start.trim().parse().ok()?,
        line_end: line_end.trim().parse().ok()?,
        note,
    })
}

fn extract_block<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let (_, rest) = text.split_once(open)?;
    let (body, _) = rest.split_once(close)?;
    Some(body)
}

fn extract_ids_block(text: &str) -> Option<&str> {
    extract_block(text, "<rollout_ids>", "</rollout_ids>")
        .or_else(|| extract_block(text, "<thread_ids>", "</thread_ids>"))
}

#[cfg(test)]
mod memory_citation_tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const STRUCTURED_CITATION: &str = "<oai-mem-citation>\n<citation_entries>\nMEMORY.md:1-2|note=[used preference]\n</citation_entries>\n<rollout_ids>\n00000000-0000-0000-0000-000000000001\n</rollout_ids>\n</oai-mem-citation>";
    const QUOTED_STRUCTURED_CITATION: &str = "<oai-mem-citation><citation_entries>MEMORY.md:1-2|note=[used \"quoted\" preference]</citation_entries><rollout_ids>00000000-0000-0000-0000-000000000001</rollout_ids></oai-mem-citation>";

    fn expected_citation(note: &str) -> MemoryCitation {
        MemoryCitation {
            entries: vec![MemoryCitationEntry {
                path: "MEMORY.md".into(),
                line_start: 1,
                line_end: 2,
                note: note.into(),
            }],
            rollout_ids: vec!["00000000-0000-0000-0000-000000000001".into()],
        }
    }

    #[test]
    fn strips_and_parses_citation_block() {
        let text = format!("answer\n{STRUCTURED_CITATION}");
        let (stripped, blocks) = strip_memory_citation_block(&text);
        assert_eq!(stripped, "answer");
        let citation = parse_memory_citation(blocks).expect("citation");
        assert_eq!(citation.entries.len(), 1);
        assert_eq!(
            citation.rollout_ids,
            vec!["00000000-0000-0000-0000-000000000001"]
        );
    }

    #[test]
    fn preserves_literal_open_tag() {
        let text = "answer mentions <oai-mem-citation> literally";
        let (stripped, blocks) = strip_memory_citation_block(text);

        assert_eq!(stripped, text);
        assert_eq!(blocks, Vec::<String>::new());
    }

    #[test]
    fn preserves_non_structured_complete_tag_pair() {
        let text = "answer\n<oai-mem-citation>\nordinary text\n</oai-mem-citation>";
        let (stripped, blocks) = strip_memory_citation_block(text);

        assert_eq!(stripped, text);
        assert_eq!(blocks, Vec::<String>::new());
    }

    #[test]
    fn preserves_incomplete_structured_literal_inside_json_string() {
        let text = r#"{"body":"explain <oai-mem-citation><citation_entries> handling"}"#;
        let (stripped, blocks) = strip_memory_citation_block(text);

        assert_eq!(stripped, text);
        assert_eq!(blocks, Vec::<String>::new());
    }

    #[test]
    fn preserves_complete_structured_literal_inside_json_and_strips_real_suffix() {
        let json = serde_json::json!({
            "body": format!("literal example: {STRUCTURED_CITATION}"),
            "escaped": "quote: \" and slash: \\",
        })
        .to_string();
        let text = format!("{json}\n{STRUCTURED_CITATION}");
        let (stripped, blocks) = strip_memory_citation_block(&text);

        assert_eq!(stripped, json);
        assert_eq!(blocks, vec![STRUCTURED_CITATION.to_string()]);
    }

    #[test]
    fn preserves_structured_literal_inside_json_array_and_strips_real_suffix() {
        let json =
            serde_json::json!([format!("literal example: {STRUCTURED_CITATION}")]).to_string();
        let text = format!("{json}\n{STRUCTURED_CITATION}");
        let (stripped, blocks) = strip_memory_citation_block(&text);

        assert_eq!(stripped, json);
        assert_eq!(blocks, vec![STRUCTURED_CITATION.to_string()]);
    }

    #[test]
    fn preserves_structured_literal_inside_root_json_string_and_strips_real_suffix() {
        let json = serde_json::to_string(&format!("literal example: {STRUCTURED_CITATION}"))
            .expect("root JSON string");
        let text = format!("{json}\n{STRUCTURED_CITATION}");
        let (stripped, blocks) = strip_memory_citation_block(&text);

        assert_eq!(stripped, json);
        assert_eq!(blocks, vec![STRUCTURED_CITATION.to_string()]);
    }

    #[test]
    fn preserves_structured_literal_inside_fenced_json_and_strips_real_suffix() {
        let json = serde_json::json!({
            "body": "explain <oai-mem-citation><citation_entries> handling",
        })
        .to_string();
        let visible = format!("Review result:\n```json\n{json}\n```");
        let text = format!("{visible}\n{STRUCTURED_CITATION}");
        let (stripped, blocks) = strip_memory_citation_block(&text);

        assert_eq!(stripped, visible);
        assert_eq!(blocks, vec![STRUCTURED_CITATION.to_string()]);
    }

    #[test]
    fn strips_structured_citation_inside_proposed_plan() {
        let text = format!(
            "<proposed_plan>\nKeep this\n{STRUCTURED_CITATION}\nAnd this\n</proposed_plan>"
        );
        let (stripped, blocks) = strip_memory_citation_block(&text);

        assert_eq!(
            stripped,
            "<proposed_plan>\nKeep this\n\nAnd this\n</proposed_plan>"
        );
        assert_eq!(blocks, vec![STRUCTURED_CITATION.to_string()]);
    }

    #[test]
    fn strips_malformed_structured_citation_tail() {
        let text = "answer\n<oai-mem-citation>\n<citation_entries>\nhidden";
        let (stripped, blocks) = strip_memory_citation_block(text);

        assert_eq!(stripped, "answer");
        assert_eq!(blocks, Vec::<String>::new());
        assert_eq!(parse_memory_citation(blocks), None);
    }

    #[test]
    fn strips_and_parses_citation_after_unclosed_json_strings() {
        for prefix in [r#"{"body":"unfinished"#, r#""unfinished"#] {
            for separator in ["", " ", "\n"] {
                let text = format!("{prefix}{separator}{STRUCTURED_CITATION}");
                let (stripped, blocks) = strip_memory_citation_block(&text);

                assert_eq!(stripped, prefix);
                assert_eq!(
                    parse_memory_citation(blocks),
                    Some(expected_citation("used preference"))
                );
            }
        }
    }

    #[test]
    fn citation_quote_does_not_validate_unclosed_json() {
        let prefix = r#"{"body":"unfinished"#;
        let text = format!("{prefix}{QUOTED_STRUCTURED_CITATION}");
        let (stripped, blocks) = strip_memory_citation_block(&text);

        assert_eq!(stripped, prefix);
        assert_eq!(
            parse_memory_citation(blocks),
            Some(expected_citation("used \"quoted\" preference"))
        );
    }

    #[test]
    fn handles_literal_and_structured_candidates_in_order() {
        let literal = "<oai-mem-citation>`literal`";
        let text = format!("before {literal} middle {STRUCTURED_CITATION} after");
        let (stripped, blocks) = strip_memory_citation_block(&text);

        assert_eq!(stripped, format!("before {literal} middle  after"));
        assert_eq!(blocks, vec![STRUCTURED_CITATION.to_string()]);
    }

    #[test]
    fn parses_legacy_thread_ids_block() {
        let text = "<oai-mem-citation>\n<thread_ids>\n00000000-0000-0000-0000-000000000001\n</thread_ids>\n</oai-mem-citation>";
        let (stripped, blocks) = strip_memory_citation_block(text);
        let citation = parse_memory_citation(blocks).expect("citation");

        assert_eq!(stripped, "");
        assert_eq!(
            citation.rollout_ids,
            vec!["00000000-0000-0000-0000-000000000001"]
        );
    }

    #[test]
    fn deduplicates_rollout_ids() {
        let text = "<oai-mem-citation>\n<rollout_ids>\n00000000-0000-0000-0000-000000000001\n00000000-0000-0000-0000-000000000001\n00000000-0000-0000-0000-000000000002\n</rollout_ids>\n</oai-mem-citation>";
        let (_stripped, blocks) = strip_memory_citation_block(text);
        let citation = parse_memory_citation(blocks).expect("citation");

        assert_eq!(
            citation.rollout_ids,
            vec![
                "00000000-0000-0000-0000-000000000001",
                "00000000-0000-0000-0000-000000000002"
            ]
        );
    }

    #[test]
    fn ignores_malformed_citation_entries() {
        let text = "<oai-mem-citation>\n<citation_entries>\nnot-a-citation\nMEMORY.md:3-4|note=[valid]\n</citation_entries>\n</oai-mem-citation>";
        let (_stripped, blocks) = strip_memory_citation_block(text);
        let citation = parse_memory_citation(blocks).expect("citation");

        assert_eq!(
            citation.entries,
            vec![MemoryCitationEntry {
                path: "MEMORY.md".to_string(),
                line_start: 3,
                line_end: 4,
                note: "valid".to_string(),
            }]
        );
    }

    #[test]
    fn leaves_text_without_citation_unchanged() {
        let text = "answer without hidden citation";
        let (stripped, blocks) = strip_memory_citation_block(text);

        assert_eq!(stripped, text);
        assert_eq!(blocks, Vec::<String>::new());
    }

    #[test]
    fn delta_filter_suppresses_complete_citation_block() {
        let mut filter = MemoryCitationDeltaFilter::default();

        assert_eq!(
            filter.push(&format!("answer \n{STRUCTURED_CITATION}tail")),
            "answer \ntail"
        );
        assert_eq!(filter.finish(), "");
    }

    #[test]
    fn delta_filter_trims_final_whitespace_before_citation() {
        let mut filter = MemoryCitationDeltaFilter::default();

        assert_eq!(
            filter.push(&format!("answer \n{STRUCTURED_CITATION}")),
            "answer"
        );
        assert_eq!(filter.finish(), "");
    }

    #[test]
    fn delta_filter_buffers_open_tag_split_across_deltas() {
        let mut filter = MemoryCitationDeltaFilter::default();

        assert_eq!(filter.push("answer<oai"), "answer");
        assert_eq!(filter.push("-mem-citation>\n<citation_entries>"), "");
        assert_eq!(
            filter
                .push("\nMEMORY.md:1-2|note=[used]\n</citation_entries>\n</oai-mem-citation>tail"),
            "tail"
        );
        assert_eq!(filter.finish(), "");
    }

    #[test]
    fn delta_filter_buffers_inner_tag_split_across_deltas() {
        let mut filter = MemoryCitationDeltaFilter::default();

        assert_eq!(
            filter.push("answer<oai-mem-citation>\n<citation_"),
            "answer"
        );
        assert_eq!(filter.push("entries>\nhidden"), "");
        assert_eq!(
            filter.push("</citation_entries></oai-mem-citation>tail"),
            "tail"
        );
        assert_eq!(filter.finish(), "");
    }

    #[test]
    fn delta_filter_buffers_close_tag_split_across_deltas() {
        let mut filter = MemoryCitationDeltaFilter::default();

        assert_eq!(
            filter.push("answer<oai-mem-citation><rollout_ids>id</rollout_ids></oai-mem"),
            "answer"
        );
        assert_eq!(filter.push("-citation>tail"), "tail");
        assert_eq!(filter.finish(), "");
    }

    #[test]
    fn delta_filter_releases_literal_candidate_when_disambiguated() {
        let mut filter = MemoryCitationDeltaFilter::default();

        assert_eq!(filter.push("answer <oai-mem-citation>"), "answer");
        assert_eq!(
            filter.push("`literal` tail"),
            " <oai-mem-citation>`literal` tail"
        );
        assert_eq!(filter.finish(), "");
    }

    #[test]
    fn delta_filter_handles_literal_before_structured_citation() {
        let mut filter = MemoryCitationDeltaFilter::default();

        assert_eq!(
            filter.push(&format!(
                "before <oai-mem-citation>\"literal\" middle {STRUCTURED_CITATION} after"
            )),
            "before <oai-mem-citation>\"literal\" middle  after"
        );
        assert_eq!(filter.finish(), "");
    }

    #[test]
    fn delta_filter_preserves_json_literal_and_suppresses_real_suffix_across_deltas() {
        let json = r#"{"body":"quote: \" then <oai-mem-citation><citation_entries> literal"}"#;
        let text = format!("{json}\n{STRUCTURED_CITATION}");
        let literal_split = text.find(OPEN_TAG).expect("literal open tag") + "<oai".len();
        let suffix_open = text.rfind(OPEN_TAG).expect("suffix open tag");
        let suffix_split = suffix_open + "<oai-mem-citation>\n<citation_".len();
        let close_split = text.rfind(CLOSE_TAG).expect("suffix close tag") + "</oai-mem".len();
        let chunks = [
            &text[..literal_split],
            &text[literal_split..suffix_split],
            &text[suffix_split..close_split],
            &text[close_split..],
        ];
        let mut filter = MemoryCitationDeltaFilter::default();
        let mut output = String::new();

        for chunk in chunks {
            output.push_str(&filter.push(chunk));
        }
        output.push_str(&filter.finish());

        assert_eq!(output, json);
    }

    #[test]
    fn delta_filter_preserves_fenced_json_literal_one_character_at_a_time() {
        let json = serde_json::json!({
            "body": format!("literal example: {STRUCTURED_CITATION}"),
            "escaped": "quote: \" and slash: \\",
        })
        .to_string();
        let visible = format!("Review result:\n```json\n{json}\n```");
        let text = format!("{visible}\n{STRUCTURED_CITATION}");
        let mut filter = MemoryCitationDeltaFilter::default();
        let mut output = String::new();

        for ch in text.chars() {
            let mut encoded = [0; 4];
            output.push_str(&filter.push(ch.encode_utf8(&mut encoded)));
        }
        output.push_str(&filter.finish());

        assert_eq!(output, visible);
    }

    #[test]
    fn delta_filter_preserves_root_json_string_literal_one_character_at_a_time() {
        let json = serde_json::to_string(&format!("literal example: {STRUCTURED_CITATION}"))
            .expect("root JSON string");
        let text = format!("{json}\n{STRUCTURED_CITATION}");
        let mut filter = MemoryCitationDeltaFilter::default();
        let mut output = String::new();

        for ch in text.chars() {
            let mut encoded = [0; 4];
            output.push_str(&filter.push(ch.encode_utf8(&mut encoded)));
        }
        output.push_str(&filter.finish());

        assert_eq!(output, json);
    }

    #[test]
    fn delta_filter_suppresses_suffix_after_unclosed_json_strings() {
        for prefix in [r#"{"body":"unfinished"#, r#""unfinished"#] {
            for separator in ["", " ", "\n"] {
                let text = format!("{prefix}{separator}{STRUCTURED_CITATION}");
                let mut filter = MemoryCitationDeltaFilter::default();
                let mut output = String::new();

                for ch in text.chars() {
                    let mut encoded = [0; 4];
                    output.push_str(&filter.push(ch.encode_utf8(&mut encoded)));
                }
                output.push_str(&filter.finish());

                assert_eq!(output, prefix);
            }
        }
    }

    #[test]
    fn delta_filter_finish_fail_closes_unresolved_json_candidate() {
        let prefix = r#"{"body":"unfinished"#;
        let text = format!("{prefix}<oai-mem-citation><citation_entries>hidden");
        let mut filter = MemoryCitationDeltaFilter::default();

        assert_eq!(filter.push(&text), prefix);
        assert_eq!(filter.finish(), "");
    }

    #[test]
    fn delta_filter_finish_flushes_partial_open_tag() {
        let mut filter = MemoryCitationDeltaFilter::default();

        assert_eq!(filter.push("literal <oai"), "literal");
        assert_eq!(filter.finish(), " <oai");
    }

    #[test]
    fn delta_filter_finish_flushes_literal_tag() {
        let mut filter = MemoryCitationDeltaFilter::default();

        assert_eq!(filter.push("literal <oai-mem-citation>"), "literal");
        assert_eq!(filter.finish(), " <oai-mem-citation>");
    }

    #[test]
    fn delta_filter_finish_discards_unclosed_structured_citation() {
        let mut filter = MemoryCitationDeltaFilter::default();

        assert_eq!(
            filter.push("answer<oai-mem-citation>\n<citation_entries>\nhidden"),
            "answer"
        );
        assert_eq!(filter.finish(), "");
    }

    #[test]
    fn array_closure_inside_citation_does_not_leak_structured_metadata() {
        let prefix = r#"["unfinished"#;
        let citation = "<oai-mem-citation><citation_entries>MEMORY.md:1-2|note=[x\"]</citation_entries><rollout_ids>00000000-0000-0000-0000-000000000001</rollout_ids></oai-mem-citation>";
        let text = format!("{prefix}{citation}");

        let (stripped, blocks) = strip_memory_citation_block(&text);

        assert_eq!(stripped, prefix);
        assert_eq!(
            parse_memory_citation(blocks),
            Some(expected_citation("x\""))
        );

        let mut filter = MemoryCitationDeltaFilter::default();
        let mut streamed = String::new();
        for ch in text.chars() {
            let mut encoded = [0; 4];
            streamed.push_str(&filter.push(ch.encode_utf8(&mut encoded)));
        }
        streamed.push_str(&filter.finish());

        assert_eq!(streamed, prefix);
    }

    #[test]
    fn prose_brace_does_not_hide_literal_tag_in_later_fenced_json() {
        let json = serde_json::json!({
            "body": format!("literal {STRUCTURED_CITATION}"),
        })
        .to_string();
        let visible = format!("Prose starts {{ not JSON\n```json\n{json}\n```");
        let text = format!("{visible}\n{STRUCTURED_CITATION}");

        let (stripped, blocks) = strip_memory_citation_block(&text);

        assert_eq!(stripped, visible);
        assert_eq!(blocks, vec![STRUCTURED_CITATION.to_string()]);

        let mut filter = MemoryCitationDeltaFilter::default();
        let mut streamed = String::new();
        for ch in text.chars() {
            let mut encoded = [0; 4];
            streamed.push_str(&filter.push(ch.encode_utf8(&mut encoded)));
        }
        streamed.push_str(&filter.finish());

        assert_eq!(streamed, visible);
    }

    #[test]
    fn delta_filter_abort_flushes_visible_trailing_whitespace() {
        let mut filter = MemoryCitationDeltaFilter::default();

        assert_eq!(filter.push("partial answer\n"), "partial answer");
        assert_eq!(filter.abort(), "\n");

        assert_eq!(filter.push("partial answer  "), "partial answer");
        assert_eq!(filter.abort(), "  ");
    }

    #[test]
    fn deferred_json_citation_scanning_stays_linear_for_fragmented_output() {
        let literal = format!(
            "{OPEN_TAG}{CITATION_ENTRIES_OPEN_TAG}{}",
            "x".repeat(64 * 1024)
        );
        let json = serde_json::json!({ "body": literal }).to_string();
        let text = format!("{json}\n{STRUCTURED_CITATION}");
        let mut scanner = MemoryCitationScanner::default();
        let mut output = String::new();

        for ch in text.chars() {
            let mut encoded = [0; 4];
            output.push_str(&scanner.push(ch.encode_utf8(&mut encoded)));
        }
        output.push_str(&scanner.finish());

        assert_eq!(output, format!("{json}\n"));
        assert!(scanner.work_units() <= text.chars().count() * 2);
    }
}
