//! The format checks of JSON Schema 2020-12 (TS `formats/index.ts`), for `CompileOptions::formats`. A value, not a
//! global: a schema compiled without it treats `format` as an annotation. The checks are adapted from TypeBox 1.3.27
//! (MIT), whose license is in `packages/schema/THIRD_PARTY_NOTICES/`.

mod bidi;
mod idna;
mod patterns;
mod punycode;
mod time;
mod unicode;
mod url;

use crate::types::FormatSet;

/// A format's name and check.
type Named = (&'static str, fn(&str) -> bool);

/// Every format JSON Schema 2020-12 defines, by name, plus `url` and `json-pointer-uri-fragment`, which TypeBox and
/// ajv-formats also check.
pub fn standard() -> FormatSet {
	let checks: [Named; 21] = [
		("date", time::is_date),
		("date-time", time::is_date_time),
		("duration", patterns::is_duration),
		("email", patterns::is_email),
		("hostname", idna::is_hostname),
		("idn-email", patterns::is_idn_email),
		("idn-hostname", idna::is_idn_hostname),
		("ipv4", patterns::is_ipv4),
		("ipv6", patterns::is_ipv6),
		("iri", url::is_iri),
		("iri-reference", url::is_iri_reference),
		("json-pointer", patterns::is_json_pointer),
		("json-pointer-uri-fragment", patterns::is_json_pointer_uri_fragment),
		("regex", patterns::is_regex),
		("relative-json-pointer", patterns::is_relative_json_pointer),
		("time", time::is_time),
		("uri", patterns::is_uri),
		("uri-reference", patterns::is_uri_reference),
		("uri-template", patterns::is_uri_template),
		("url", url::is_url),
		("uuid", patterns::is_uuid),
	];
	let mut set = FormatSet::new();
	for (name, check) in checks {
		set.insert(name, check);
	}
	set
}
