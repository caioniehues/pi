//! Reference resolution of one compile (TS `resolver.ts`): schema resources, anchors and JSON Pointers over the root
//! schema and the registered documents. A Resolver lives only while a schema compiles (or while its errors are
//! explained, or a value coerced), borrowing the schema documents; schema objects are identified by address.

use crate::types::{CompileError, CompileOptions, Draft, UnresolvedRefs};
use pi_json::JsonValue;
use serde_json::Map;
use std::collections::{HashMap, HashSet};

/// A schema and the base URI its references resolve against.
pub(crate) struct Resolved<'d> {
	pub(crate) node: &'d JsonValue,
	pub(crate) base: String,
}

/// The base URI of a root schema without `$id`; relative references resolve against it.
pub(crate) const DEFAULT_BASE: &str = "https://json-schema.invalid/root.json";

/// The schema `false`, which an unresolved reference becomes with `UnresolvedRefs::Reject`.
static FALSE: JsonValue = JsonValue::Bool(false);

/// The identity of a schema object or a value: its address, stable while the documents are borrowed.
pub(crate) fn identity<T>(value: &T) -> usize {
	std::ptr::from_ref(value).addr()
}

/// Resolves references in the root schema and the registered documents. The schema is indexed on the first
/// reference or the first `$id` a walker meets, so a schema with neither is never walked for identifiers.
pub(crate) struct Resolver<'d> {
	pub(crate) draft: Draft,
	/// Schema object (by identity) to the base URI it was indexed under.
	base_of: HashMap<usize, String>,
	/// URI (with a `#name` fragment) of every `$dynamicAnchor`, to its schema.
	pub(crate) dynamic_anchors: HashMap<String, &'d JsonValue>,
	root: &'d JsonValue,
	documents: &'d [(String, JsonValue)],
	registry: HashMap<String, &'d JsonValue>,
	reject_unresolved: bool,
	/// The references that named nothing and resolved to `false` (with `UnresolvedRefs::Reject`).
	unresolved: HashSet<String>,
	resources: HashMap<String, &'d JsonValue>,
	anchors: HashMap<String, &'d JsonValue>,
	indexed: bool,
}

impl<'d> Resolver<'d> {
	/// A resolver over `root` and the documents of `options`.
	pub(crate) fn new(root: &'d JsonValue, options: &'d CompileOptions) -> Self {
		Self {
			draft: options.draft,
			base_of: HashMap::new(),
			dynamic_anchors: HashMap::new(),
			root,
			documents: &options.schemas,
			registry: HashMap::new(),
			reject_unresolved: options.unresolved_refs == UnresolvedRefs::Reject,
			unresolved: HashSet::new(),
			resources: HashMap::new(),
			anchors: HashMap::new(),
			indexed: false,
		}
	}

	/// The base URI of `node`, whose parent has `base`: its `$id` changes it, unless draft 7 makes the `$id` inert.
	/// Once the schema is indexed, the indexed base (a resolved reference target already has its own).
	pub(crate) fn base_for(&mut self, node: &'d Map<String, JsonValue>, base: &str) -> String {
		if let Some(indexed) = self.base_of.get(&identity(node)) {
			return indexed.clone();
		}
		let Some(id) = node.get("$id").and_then(JsonValue::as_str) else {
			return base.to_owned();
		};
		// Index before applying an `$id`: a caller's `base` may already include it, and the index applies it once.
		if !self.indexed {
			self.ensure_indexed();
			if let Some(indexed) = self.base_of.get(&identity(node)) {
				return indexed.clone();
			}
		}
		if self.draft == Draft::V7 && (node.get("$ref").is_some_and(JsonValue::is_string) || id.starts_with('#')) {
			return base.to_owned();
		}
		document_of(&resolve_uri(id, base)).to_owned()
	}

	/// The indexed base URI of a schema object, if it was indexed.
	pub(crate) fn indexed_base(&self, node: &Map<String, JsonValue>) -> Option<&str> {
		self.base_of.get(&identity(node)).map(String::as_str)
	}

	/// Indexes the root schema and the registered documents' URIs (once).
	fn ensure_indexed(&mut self) {
		if self.indexed {
			return;
		}
		self.indexed = true;
		for (uri, schema) in self.documents {
			self.registry
				.insert(document_of(&resolve_uri(uri, DEFAULT_BASE)).to_owned(), schema);
		}
		self.resources.insert(DEFAULT_BASE.to_owned(), self.root);
		self.index(self.root, DEFAULT_BASE);
	}

	/// The schema a reference names, with the base URI it compiles under. Fails if it names nothing, unless
	/// `UnresolvedRefs::Reject` makes it `false`, the schema no value passes.
	pub(crate) fn resolve(&mut self, reference: &str, base: &str) -> Result<Resolved<'d>, CompileError> {
		self.ensure_indexed();
		match self.find(reference, base) {
			Err(_) if self.reject_unresolved => {
				self.unresolved.insert(resolve_uri(reference, base));
				Ok(Resolved {
					node: &FALSE,
					base: base.to_owned(),
				})
			}
			found => found,
		}
	}

	/// Whether `reference`, from `base`, named nothing and resolved to `false`.
	pub(crate) fn is_unresolved(&self, reference: &str, base: &str) -> bool {
		self.unresolved.contains(&resolve_uri(reference, base))
	}

	fn find(&mut self, reference: &str, base: &str) -> Result<Resolved<'d>, CompileError> {
		let uri = resolve_uri(reference, base);
		let doc = document_of(&uri).to_owned();
		let fragment = match uri.get(doc.len() + 1..) {
			Some(raw) => decode_uri_component(raw)?,
			None => String::new(),
		};
		let root = match self.resources.get(&doc) {
			Some(root) => *root,
			None => self.load(&doc).ok_or_else(|| {
				CompileError::UnresolvedRef(format!("Unresolved $ref {reference} (no schema registered for {doc})"))
			})?,
		};
		if fragment.is_empty() {
			return Ok(Resolved { node: root, base: doc });
		}
		if fragment.starts_with('/') {
			return self.pointer(root, &doc, &fragment);
		}
		let Some(target) = self.anchors.get(&format!("{doc}#{fragment}")).copied() else {
			return Err(CompileError::UnresolvedRef(format!(
				"Unresolved $ref {reference} (no anchor {fragment} in {doc})"
			)));
		};
		let base = target
			.as_object()
			.and_then(|map| self.indexed_base(map))
			.map_or(doc, str::to_owned);
		Ok(Resolved { node: target, base })
	}

	/// Whether `node` is the root of the schema resource at `base`.
	pub(crate) fn is_resource(&mut self, node: &Map<String, JsonValue>, base: &str) -> bool {
		self.ensure_indexed();
		self.resources
			.get(base)
			.and_then(|resource| resource.as_object())
			.is_some_and(|resource| std::ptr::eq(resource, node))
	}

	// Walks only keywords that hold subschemas: an `$id` inside `enum`, `const` or an unknown keyword names nothing.
	fn index(&mut self, node: &'d JsonValue, base: &str) {
		let Some(map) = node.as_object() else { return };
		if self.base_of.contains_key(&identity(map)) {
			return;
		}
		let mut b = base.to_owned();
		// In draft 7 a `$ref` makes every sibling keyword, `$id` included, inert.
		let ref_only = self.draft == Draft::V7 && map.get("$ref").is_some_and(JsonValue::is_string);
		if let Some(id) = map.get("$id").and_then(JsonValue::as_str).filter(|_| !ref_only) {
			if self.draft == Draft::V7 && id.starts_with('#') {
				self.anchors.insert(format!("{}{id}", document_of(&b)), node);
			} else {
				let uri = resolve_uri(id, &b);
				document_of(&uri).clone_into(&mut b);
				self.resources.insert(b.clone(), node);
				if uri.len() > b.len() + 1 {
					self.anchors.insert(uri, node);
				}
			}
		}
		self.base_of.insert(identity(map), b.clone());
		if ref_only {
			return;
		}
		if self.draft != Draft::V7 {
			if let Some(anchor) = map.get("$anchor").and_then(JsonValue::as_str) {
				self.anchors.insert(format!("{b}#{anchor}"), node);
			}
			if let Some(anchor) = map.get("$dynamicAnchor").and_then(JsonValue::as_str) {
				self.anchors.insert(format!("{b}#{anchor}"), node);
				self.dynamic_anchors.insert(format!("{b}#{anchor}"), node);
			}
		}
		for key in SCHEMA_KEYWORDS {
			if let Some(child) = map.get(key) {
				self.index(child, &b);
			}
		}
		for key in MAP_KEYWORDS {
			if let Some(children) = map.get(key).and_then(JsonValue::as_object) {
				for child in children.values() {
					self.index(child, &b);
				}
			}
		}
		for key in ARRAY_KEYWORDS {
			if let Some(children) = map.get(key).and_then(JsonValue::as_array) {
				for child in children {
					self.index(child, &b);
				}
			}
		}
	}

	fn load(&mut self, doc: &str) -> Option<&'d JsonValue> {
		let schema = *self.registry.get(doc)?;
		self.resources.insert(doc.to_owned(), schema);
		self.index(schema, doc);
		Some(schema)
	}

	fn pointer(&self, root: &'d JsonValue, base: &str, fragment: &str) -> Result<Resolved<'d>, CompileError> {
		let mut node = root;
		let mut b = base.to_owned();
		for raw in fragment.split('/').skip(1) {
			let key = raw.replace("~1", "/").replace("~0", "~");
			let next = match node {
				JsonValue::Object(map) => map.get(&key),
				JsonValue::Array(items) => array_index(&key).and_then(|i| items.get(i)),
				_ => None,
			};
			node = next.ok_or_else(|| CompileError::UnresolvedRef(format!("Unresolved $ref #{fragment} in {base}")))?;
			if let Some(indexed) = node.as_object().and_then(|map| self.indexed_base(map)) {
				indexed.clone_into(&mut b);
			}
		}
		Ok(Resolved { node, base: b })
	}
}

/// The index an array has as an own property named `key`: canonical decimal, no sign or leading zero.
fn array_index(key: &str) -> Option<usize> {
	let canonical = key == "0" || (!key.starts_with('0') && !key.is_empty() && key.bytes().all(|b| b.is_ascii_digit()));
	if canonical { key.parse().ok() } else { None }
}

const SCHEMA_KEYWORDS: [&str; 12] = [
	"additionalProperties",
	"propertyNames",
	"contains",
	"not",
	"if",
	"then",
	"else",
	"unevaluatedProperties",
	"unevaluatedItems",
	"additionalItems",
	"contentSchema",
	"items",
];

const MAP_KEYWORDS: [&str; 6] = [
	"properties",
	"patternProperties",
	"$defs",
	"definitions",
	"dependentSchemas",
	"dependencies",
];

const ARRAY_KEYWORDS: [&str; 5] = ["allOf", "anyOf", "oneOf", "prefixItems", "items"];

/// `reference` resolved against `base`, as `new URL(reference, base).href`. URNs and other bases a URL parser
/// rejects take fragment-only references.
pub(crate) fn resolve_uri(reference: &str, base: &str) -> String {
	match url::Url::parse(base).and_then(|base| base.join(reference)) {
		Ok(url) => url.into(),
		Err(_) if reference.starts_with('#') => format!("{}{reference}", document_of(base)),
		Err(_) => reference.to_owned(),
	}
}

/// `uri` without its fragment.
pub(crate) fn document_of(uri: &str) -> &str {
	uri.find('#').map_or(uri, |hash| &uri[..hash])
}

/// `decodeURIComponent`: every `%XX` escape decoded, the bytes read as UTF-8; a malformed escape or invalid UTF-8
/// fails as JS throws `URIError`.
fn decode_uri_component(text: &str) -> Result<String, CompileError> {
	if !text.contains('%') {
		return Ok(text.to_owned());
	}
	let bytes = text.as_bytes();
	let mut out = Vec::with_capacity(bytes.len());
	let mut i = 0;
	while i < bytes.len() {
		if bytes[i] == b'%' {
			let hex = text
				.get(i + 1..i + 3)
				.filter(|hex| hex.bytes().all(|b| b.is_ascii_hexdigit()))
				.ok_or(CompileError::MalformedUri)?;
			out.push(u8::from_str_radix(hex, 16).map_err(|_| CompileError::MalformedUri)?);
			i += 3;
		} else {
			out.push(bytes[i]);
			i += 1;
		}
	}
	String::from_utf8(out).map_err(|_| CompileError::MalformedUri)
}
