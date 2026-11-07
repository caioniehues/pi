//! The built-in model catalog (ports `packages/ai/src/model-catalog.ts` and the generated `*.models.ts` shards): each
//! provider's data file, embedded by build.rs, grouped by API and keyed `type:id`, flattened per model type into
//! [`MODELS`], [`IMAGE_MODELS`] and [`CLASSIFIER_MODELS`]. A provider's file is parsed on its first lookup.
//!
//! Without hydrated data (see build.rs) the catalog is empty and [`is_hydrated`] is false.

use pi_json::JsonValue;
use pi_types::{ClassifierModel, ImageModel, Model};
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::sync::OnceLock;

include!(concat!(env!("OUT_DIR"), "/model_data.rs"));

/// One provider's models, by type, in file order.
#[derive(Debug, Default)]
pub struct ProviderCatalog {
	/// Chat models.
	pub chat: Vec<Model>,
	/// Image models.
	pub image: Vec<ImageModel>,
	/// Classifier models.
	pub classifier: Vec<ClassifierModel>,
}

/// Models of one type, looked up by provider id.
pub struct Catalog<T: 'static> {
	select: fn(&'static ProviderCatalog) -> &'static [T],
}

/// Chat models by provider (`<PROVIDER>_MODELS`).
pub static MODELS: Catalog<Model> = Catalog {
	select: |catalog| &catalog.chat,
};

/// Image models by provider (`<PROVIDER>_IMAGE_MODELS`).
pub static IMAGE_MODELS: Catalog<ImageModel> = Catalog {
	select: |catalog| &catalog.image,
};

/// Classifier models by provider (`<PROVIDER>_CLASSIFIER_MODELS`).
pub static CLASSIFIER_MODELS: Catalog<ClassifierModel> = Catalog {
	select: |catalog| &catalog.classifier,
};

/// Something with a model id.
pub trait CatalogModel {
	/// The model id.
	fn model_id(&self) -> &str;
}

impl CatalogModel for Model {
	fn model_id(&self) -> &str {
		&self.id
	}
}

impl CatalogModel for ImageModel {
	fn model_id(&self) -> &str {
		&self.id
	}
}

impl CatalogModel for ClassifierModel {
	fn model_id(&self) -> &str {
		&self.id
	}
}

impl<T: CatalogModel + 'static> Catalog<T> {
	/// The provider's models of this type; empty for an unknown provider.
	pub fn get(&self, provider: &str) -> &'static [T] {
		provider_catalog(provider).map_or(&[], self.select)
	}

	/// One model by provider and id.
	pub fn model(&self, provider: &str, id: &str) -> Option<&'static T> {
		self.get(provider).iter().find(|model| model.model_id() == id)
	}
}

/// Whether model data was embedded at build time.
pub fn is_hydrated() -> bool {
	!PROVIDER_DATA.is_empty()
}

/// The provider ids of the catalog, sorted.
pub fn providers() -> Vec<&'static str> {
	PROVIDER_DATA.iter().map(|(provider, _)| *provider).collect()
}

/// The raw catalog JSON of a provider, as embedded.
pub fn provider_data(provider: &str) -> Option<&'static str> {
	PROVIDER_DATA
		.iter()
		.find(|(id, _)| *id == provider)
		.map(|(_, data)| *data)
}

fn provider_catalog(provider: &str) -> Option<&'static ProviderCatalog> {
	static CATALOGS: OnceLock<HashMap<&'static str, OnceLock<ProviderCatalog>>> = OnceLock::new();
	let catalogs = CATALOGS.get_or_init(|| PROVIDER_DATA.iter().map(|(id, _)| (*id, OnceLock::new())).collect());
	let (id, cell) = catalogs.get_key_value(provider)?;
	Some(cell.get_or_init(|| flatten(provider_data(id).unwrap_or("{}"))))
}

/// Flattens API groups into models by type (`flattenModelCatalog`); build.rs validated every entry.
fn flatten(data: &str) -> ProviderCatalog {
	let mut catalog = ProviderCatalog::default();
	let Ok(JsonValue::Object(groups)) = pi_json::parse(data) else {
		return catalog;
	};
	for group in groups.values().filter_map(JsonValue::as_object) {
		for model in group.values() {
			match model.get("type").and_then(JsonValue::as_str) {
				Some("chat") => push(&mut catalog.chat, model),
				Some("image") => push(&mut catalog.image, model),
				Some("classifier") => push(&mut catalog.classifier, model),
				_ => {}
			}
		}
	}
	catalog
}

fn push<T: DeserializeOwned + CatalogModel>(models: &mut Vec<T>, value: &JsonValue) {
	let Ok(model) = T::deserialize(value) else { return };
	// TS parity: `Object.fromEntries` keeps a repeated id at its first position with its last value.
	match models
		.iter_mut()
		.find(|existing| existing.model_id() == model.model_id())
	{
		Some(slot) => *slot = model,
		None => models.push(model),
	}
}
