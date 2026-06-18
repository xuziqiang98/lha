use lha_llm::ModelPricing;
use lha_llm::TokenUsage;
use lha_llm::UsdPerMillionTokensMicros;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct InputSlimmingBillingPending {
    pub(crate) saved_historical_tokens: i64,
    pub(crate) saved_live_tokens: i64,
}

impl InputSlimmingBillingPending {
    pub(crate) fn tokens_saved(self) -> i64 {
        self.saved_historical_tokens
            .saturating_add(self.saved_live_tokens)
    }

    fn saved_cached_input_tokens(self, after: &TokenUsage) -> i64 {
        if after.cached_input_tokens > 0 {
            self.saved_historical_tokens
        } else {
            0
        }
    }
}

pub(crate) fn input_slimming_saved_usd_micros(
    pricing: Option<&ModelPricing>,
    after: &TokenUsage,
    pending: InputSlimmingBillingPending,
) -> Option<i64> {
    let pricing = pricing?;
    if pending.tokens_saved() <= 0 {
        return None;
    }
    let saved_cached_input_tokens = pending.saved_cached_input_tokens(after);
    let before = TokenUsage {
        input_tokens: after.input_tokens.checked_add(pending.tokens_saved())?,
        cached_input_tokens: after
            .cached_input_tokens
            .checked_add(saved_cached_input_tokens)?,
        output_tokens: after.output_tokens,
        reasoning_output_tokens: after.reasoning_output_tokens,
        total_tokens: after.total_tokens.checked_add(pending.tokens_saved())?,
    };
    let before = usage_cost_usd_micros(pricing, &before)?;
    let after = usage_cost_usd_micros(pricing, after)?;
    Some(before.saturating_sub(after).max(0))
}

pub(crate) fn usage_cost_usd_micros(pricing: &ModelPricing, usage: &TokenUsage) -> Option<i64> {
    if usage.input_tokens < 0 || usage.cached_input_tokens < 0 {
        return None;
    }
    if usage.cached_input_tokens > usage.input_tokens {
        return None;
    }
    let band = pricing.band_for_input_tokens(usage.input_tokens)?;
    let non_cached_input = usage.non_cached_input();
    let cached_input = usage.cached_input();
    let output = usage.output_tokens.max(0);

    token_cost_usd_micros(non_cached_input, band.input)?
        .checked_add(token_cost_usd_micros(cached_input, band.cached_input)?)?
        .checked_add(token_cost_usd_micros(output, band.output)?)
}

fn token_cost_usd_micros(tokens: i64, rate: UsdPerMillionTokensMicros) -> Option<i64> {
    if tokens < 0 {
        return None;
    }
    tokens.checked_mul(rate.as_micros())?.checked_div(1_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lha_llm::ModelPricingBand;
    use lha_llm::ModelPricingBilling;
    use lha_llm::ModelPricingCurrency;
    use lha_llm::ModelPricingUnit;

    fn pricing() -> ModelPricing {
        ModelPricing {
            currency: ModelPricingCurrency::Usd,
            unit: ModelPricingUnit::UsdPerMillionTokens,
            billing: ModelPricingBilling::Standard,
            context_bands: vec![
                ModelPricingBand {
                    min_input_tokens: None,
                    max_input_tokens: Some(272_000),
                    input: UsdPerMillionTokensMicros::from_micros(2_500_000),
                    cached_input: UsdPerMillionTokensMicros::from_micros(250_000),
                    output: UsdPerMillionTokensMicros::from_micros(15_000_000),
                },
                ModelPricingBand {
                    min_input_tokens: Some(272_001),
                    max_input_tokens: None,
                    input: UsdPerMillionTokensMicros::from_micros(5_000_000),
                    cached_input: UsdPerMillionTokensMicros::from_micros(500_000),
                    output: UsdPerMillionTokensMicros::from_micros(22_500_000),
                },
            ],
        }
    }

    #[test]
    fn usage_cost_prices_non_cached_cached_and_output_tokens() {
        let usage = TokenUsage {
            input_tokens: 2_000,
            cached_input_tokens: 500,
            output_tokens: 100,
            total_tokens: 2_100,
            ..TokenUsage::default()
        };

        assert_eq!(usage_cost_usd_micros(&pricing(), &usage), Some(5_375));
    }

    #[test]
    fn saved_cost_uses_cached_rate_for_historical_tokens_when_actual_cache_hit() {
        let after = TokenUsage {
            input_tokens: 10_000,
            cached_input_tokens: 1_000,
            output_tokens: 500,
            total_tokens: 10_500,
            ..TokenUsage::default()
        };
        let pending = InputSlimmingBillingPending {
            saved_historical_tokens: 1_000,
            saved_live_tokens: 2_000,
        };

        assert_eq!(
            input_slimming_saved_usd_micros(Some(&pricing()), &after, pending),
            Some(5_250)
        );
    }

    #[test]
    fn saved_cost_includes_band_change_output_delta() {
        let after = TokenUsage {
            input_tokens: 270_000,
            cached_input_tokens: 0,
            output_tokens: 1_000,
            total_tokens: 271_000,
            ..TokenUsage::default()
        };
        let pending = InputSlimmingBillingPending {
            saved_historical_tokens: 0,
            saved_live_tokens: 3_000,
        };

        assert_eq!(
            input_slimming_saved_usd_micros(Some(&pricing()), &after, pending),
            Some(697_500)
        );
    }

    #[test]
    fn missing_pricing_fails_open() {
        let after = TokenUsage {
            input_tokens: 10_000,
            total_tokens: 10_000,
            ..TokenUsage::default()
        };
        let pending = InputSlimmingBillingPending {
            saved_historical_tokens: 1_000,
            saved_live_tokens: 0,
        };

        assert_eq!(input_slimming_saved_usd_micros(None, &after, pending), None);
    }
}
