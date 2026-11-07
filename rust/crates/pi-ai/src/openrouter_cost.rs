//! OpenRouter's billed cost (ports `packages/ai/src/utils/openrouter-cost.ts`).

use pi_types::Usage;

/// OpenRouter's `usage` accounting fields (`OpenRouterUsageAccounting`), as read from the wire.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OpenRouterUsageAccounting {
	/// Amount charged to the OpenRouter account.
	pub cost: Option<f64>,
	/// Whether the user's upstream provider key was used; `None` when absent.
	pub is_byok: Option<bool>,
	/// Upstream cost details.
	pub cost_details: Option<OpenRouterCostDetails>,
}

/// `cost_details`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OpenRouterCostDetails {
	/// Total upstream inference cost, when available.
	pub upstream_inference_cost: Option<f64>,
	/// Upstream prompt cost.
	pub upstream_inference_prompt_cost: Option<f64>,
	/// Upstream completion cost.
	pub upstream_inference_completions_cost: Option<f64>,
}

fn valid_cost(value: Option<f64>) -> Option<f64> {
	value.filter(|value| value.is_finite() && *value >= 0.0)
}

/// Whether a model is served through OpenRouter (`isOpenRouterModel`).
pub fn is_openrouter_model(provider: &str, base_url: &str) -> bool {
	provider == "openrouter" || base_url.contains("openrouter.ai")
}

/// Replaces the catalog total with OpenRouter's billed cost when the accounting is complete and valid
/// (`applyOpenRouterReportedCost`).
pub fn apply_openrouter_reported_cost(usage: &mut Usage, reported: &OpenRouterUsageAccounting) {
	let Some(cost) = valid_cost(reported.cost) else { return };
	let total = match reported.is_byok {
		Some(false) => cost,
		Some(true) => {
			let Some(details) = &reported.cost_details else { return };
			let upstream = if let Some(upstream) = details.upstream_inference_cost {
				upstream
			} else {
				let (Some(prompt), Some(completions)) = (
					valid_cost(details.upstream_inference_prompt_cost),
					valid_cost(details.upstream_inference_completions_cost),
				) else {
					return;
				};
				prompt + completions
			};
			let Some(upstream) = valid_cost(Some(upstream)) else {
				return;
			};
			cost + upstream
		}
		None => return,
	};
	usage.cost.total = total;
	usage.cost.total_from_provider = Some(true);
}
