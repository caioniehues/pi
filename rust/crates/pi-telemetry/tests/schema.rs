//! Schemas as data (`schema.rs`): the span checks of `validate_against_schema`, which stand in for the TS
//! compile-time checks of `packages/telemetry/test/telemetry.test.ts`.

use pi_telemetry::{
	AttributeDefinition, AttributeKind, EventDefinition, InMemoryTelemetryContext, ParentRule, SchemaProblem,
	SpanAttributes, SpanDefinition, SpanOptions, TelemetryContext, TelemetrySchema, validate_against_schema,
};

static OPERATION_SCHEMA: TelemetrySchema = TelemetrySchema {
	version: 1,
	spans: &[SpanDefinition {
		name: "operation",
		description: "Operation",
		parents: ParentRule::RootOrExternal,
		start_attributes: &[AttributeDefinition::new(
			"kind",
			"Kind",
			AttributeKind::String {
				values: Some(&["read", "write"]),
			},
		)
		.required()],
		end_attributes: &[],
		events: &[EventDefinition {
			name: "result",
			description: "Result",
			attributes: &[AttributeDefinition::new(
				"outcome",
				"Outcome",
				AttributeKind::String {
					values: Some(&["ok", "error"]),
				},
			)
			.required()],
		}],
		error_when: "The operation fails",
	}],
	metrics: &[],
};

static REQUEST_SCHEMA: TelemetrySchema = TelemetrySchema {
	version: 3,
	spans: &[SpanDefinition {
		name: "request",
		description: "Request",
		parents: ParentRule::Spans(&["operation"]),
		start_attributes: &[
			AttributeDefinition::new("provider", "Provider", AttributeKind::String { values: None }).required(),
		],
		end_attributes: &[AttributeDefinition::new(
			"response",
			"Response kind",
			AttributeKind::String { values: None },
		)],
		events: &[],
		error_when: "The request fails",
	}],
	metrics: &[],
};

const SCHEMAS: [&TelemetrySchema; 2] = [&OPERATION_SCHEMA, &REQUEST_SCHEMA];

fn problems(telemetry: &InMemoryTelemetryContext) -> Vec<(String, SchemaProblem)> {
	validate_against_schema(&telemetry.get_spans(), &[], &SCHEMAS)
		.into_iter()
		.map(|violation| (violation.subject, violation.problem))
		.collect()
}

// TS: telemetry.test.ts "combines schema vocabularies and binds child starters to their parent spans".
#[test]
fn combined_vocabularies_accept_conforming_spans() {
	let telemetry = InMemoryTelemetryContext::new();
	let operation =
		telemetry.start_span(SpanOptions::new("operation").with_attributes(SpanAttributes::new().with("kind", "read")));
	operation.add_event("result", SpanAttributes::new().with("outcome", "ok"));
	let request = operation
		.start_span(SpanOptions::new("request").with_attributes(SpanAttributes::new().with("provider", "example")));
	request.set_attributes(SpanAttributes::new().with("response", "cached"));
	request.end();
	operation.end();
	assert_eq!(problems(&telemetry), []);
	let spans = telemetry.get_spans();
	assert_eq!(spans[1].parent_id, Some(spans[0].id));
}

// TS: telemetry.test.ts "preserves serializable definitions and infers exact attributes" (its `@ts-expect-error`
// lines) and the compile-time failures of "combines schema vocabularies ...".
#[test]
fn reports_what_ts_rejects_at_compile_time() {
	let telemetry = InMemoryTelemetryContext::new();
	let operation = telemetry
		.start_span(SpanOptions::new("operation").with_attributes(SpanAttributes::new().with("kind", "other")));
	operation.add_event("result", SpanAttributes::new());
	operation.add_event("unknown", SpanAttributes::new());
	operation.set_attributes(SpanAttributes::new().with("unknown", true));
	operation.end();
	telemetry.start_span(SpanOptions::new("unknown")).end();
	// A request outside an operation, without its required provider.
	telemetry.start_span(SpanOptions::new("request")).end();
	assert_eq!(
		problems(&telemetry),
		[
			("span operation".to_owned(), SchemaProblem::InvalidValue("kind".into())),
			(
				"span operation".to_owned(),
				SchemaProblem::UndeclaredAttribute("unknown".into())
			),
			(
				"span operation event result".to_owned(),
				SchemaProblem::MissingAttribute("outcome".into())
			),
			("span operation event unknown".to_owned(), SchemaProblem::Undeclared),
			("span unknown".to_owned(), SchemaProblem::Undeclared),
			(
				"span request".to_owned(),
				SchemaProblem::MissingAttribute("provider".into())
			),
			("span request".to_owned(), SchemaProblem::ParentNotAllowed),
		]
	);
}

// Rust only: TS does not enforce parent rules.
#[test]
fn root_or_external_parents_exclude_spans_of_the_schemas() {
	let telemetry = InMemoryTelemetryContext::new();
	let start = || SpanOptions::new("operation").with_attributes(SpanAttributes::new().with("kind", "write"));
	let external = telemetry.start_span(SpanOptions::new("caller"));
	external.start_span(start()).end();
	external.end();
	let outer = telemetry.start_span(start());
	outer.start_span(start()).end();
	outer.end();
	let parent_problems: Vec<_> = problems(&telemetry)
		.into_iter()
		.filter(|(_, problem)| *problem != SchemaProblem::Undeclared)
		.collect();
	assert_eq!(
		parent_problems,
		[("span operation".to_owned(), SchemaProblem::ParentNotAllowed)]
	);
}
