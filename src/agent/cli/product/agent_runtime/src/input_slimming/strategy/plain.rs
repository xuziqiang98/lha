use super::StrategyOutput;
use crate::product::agent::input_slimming::InputSlimmingOptions;
use crate::product::agent::input_slimming::InputSlimmingStrategy;
use crate::product::agent::truncate::TruncationPolicy;
use crate::product::agent::truncate::truncate_text;

pub(super) fn plain_text_head_tail(
    text: &str,
    options: InputSlimmingOptions,
) -> Option<StrategyOutput> {
    let truncated = truncate_text(text, TruncationPolicy::Tokens(options.target_tokens));
    if truncated == text {
        return None;
    }
    Some(StrategyOutput {
        strategy: InputSlimmingStrategy::PlainTextHeadTail,
        body: truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn plain_text_strategy_emits_head_tail_body() {
        let text = "abcdef".repeat(2_000);

        let output =
            plain_text_head_tail(&text, InputSlimmingOptions::default()).expect("strategy output");

        assert_eq!(output.strategy, InputSlimmingStrategy::PlainTextHeadTail);
        assert!(output.body.contains("truncated"));
    }
}
