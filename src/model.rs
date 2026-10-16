use std::collections::HashMap;

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchItem {
    pub id: u64,
    #[serde(default)]
    pub source: StoreSource,
    pub input: String,
    pub product_url: String,
    pub variant_filter: String,
    pub enabled: bool,
    pub created_at: DateTime<Local>,

    #[serde(default)]
    pub purchase: PurchaseRule,

    #[serde(skip)]
    pub runtime: RuntimeState,
}

impl WatchItem {
    pub fn new(
        id: u64,
        source: StoreSource,
        input: String,
        product_url: String,
        variant_filter: String,
    ) -> Self {
        Self {
            id,
            source,
            input,
            product_url,
            variant_filter,
            enabled: true,
            created_at: Local::now(),
            purchase: PurchaseRule::default(),
            runtime: RuntimeState::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PurchaseRule {
    /// Optional size priority, e.g. "M | S". Color priority remains in
    /// WatchItem::variant_filter for backwards-compatible saved watchlists.
    pub size_priority: String,
    /// 0 means no price ceiling. Stored as whole yen for a friendlier UI.
    pub max_price_yen: u64,
    pub quantity: u32,
    pub auto_open_checkout: bool,
}

impl Default for PurchaseRule {
    fn default() -> Self {
        Self {
            size_priority: String::new(),
            max_price_yen: 0,
            quantity: 1,
            auto_open_checkout: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
pub enum StoreSource {
    #[default]
    JapanOnlineStore,
    ChinaLaceRococo,
}

impl StoreSource {
    pub fn region_label(self) -> &'static str {
        match self {
            Self::JapanOnlineStore => "Japan",
            Self::ChinaLaceRococo => "Mainland China",
        }
    }

    pub fn store_label(self) -> &'static str {
        match self {
            Self::JapanOnlineStore => "BABY Japan Official Online Store",
            Self::ChinaLaceRococo => "LaceRococo · China Partner Store",
        }
    }

    pub fn host(self) -> &'static str {
        match self {
            Self::JapanOnlineStore => "store.babyssb.co.jp",
            Self::ChinaLaceRococo => "www.lacerococo.com",
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            Self::JapanOnlineStore => "JP",
            Self::ChinaLaceRococo => "CN",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct RuntimeState {
    pub title: Option<String>,
    pub product_code: Option<String>,
    pub vendor: Option<String>,
    pub image_url: Option<String>,
    pub variants: Vec<VariantState>,
    pub selected_variant: Option<u64>,
    pub last_checked: Option<DateTime<Local>>,
    pub last_error: Option<String>,
    pub checking: bool,
}

impl RuntimeState {
    pub fn matching_available_count(&self, filter: &str) -> usize {
        self.variants
            .iter()
            .filter(|v| v.available && v.matches_filter(filter))
            .count()
    }

    pub fn any_matching_available(&self, filter: &str) -> bool {
        self.matching_available_count(filter) > 0
    }

    pub fn selectable_variants<'a>(&'a self, filter: &str) -> Vec<&'a VariantState> {
        self.variants
            .iter()
            .filter(|v| v.matches_filter(filter))
            .collect()
    }

    pub fn colors(&self) -> Vec<ColorState> {
        aggregate_colors(&self.variants)
    }
}

#[derive(Debug, Clone)]
pub struct VariantOption {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone)]
pub struct VariantState {
    pub id: u64,
    pub title: String,
    pub sku: Option<String>,
    pub available: bool,
    pub price_minor: i64,
    pub options: Vec<VariantOption>,
    pub color: Option<String>,
    pub size: Option<String>,
}

impl VariantState {
    pub fn matches_filter(&self, filter: &str) -> bool {
        let filter = filter.trim();
        if filter.is_empty() {
            return true;
        }

        filter_tokens(filter).any(|token| {
            contains_ci(self.color.as_deref(), token)
                || contains_ci(self.size.as_deref(), token)
                || self.title.to_lowercase().contains(&token.to_lowercase())
                || contains_ci(self.sku.as_deref(), token)
                || self.options.iter().any(|option| {
                    option.value.to_lowercase().contains(&token.to_lowercase())
                        || option.name.to_lowercase().contains(&token.to_lowercase())
                })
        })
    }

    pub fn matches_color_priority(&self, filter: &str) -> bool {
        dimension_priority(self.color.as_deref(), &self.title, filter).is_some()
    }

    pub fn color_priority(&self, filter: &str) -> Option<usize> {
        dimension_priority(self.color.as_deref(), &self.title, filter)
    }

    pub fn matches_size_priority(&self, filter: &str) -> bool {
        size_priority(self.size.as_deref(), &self.title, filter).is_some()
    }

    pub fn size_priority(&self, filter: &str) -> Option<usize> {
        size_priority(self.size.as_deref(), &self.title, filter)
    }

    pub fn within_price_limit(&self, max_price_yen: u64) -> bool {
        max_price_yen == 0 || self.price_minor <= (max_price_yen as i64).saturating_mul(100)
    }

    pub fn display_label(&self) -> String {
        match (self.color.as_deref(), self.size.as_deref()) {
            (Some(color), Some(size)) if !size.is_empty() => format!("{color} · {size}"),
            (Some(color), _) => color.to_owned(),
            (_, Some(size)) if !size.is_empty() => format!("Size {size}"),
            _ if !self.title.trim().is_empty() && self.title != "Default Title" => {
                self.title.clone()
            }
            _ => self.sku.clone().unwrap_or_else(|| "Default".into()),
        }
    }

    pub fn color_or_display(&self) -> String {
        self.color.clone().unwrap_or_else(|| self.display_label())
    }
}

#[derive(Debug, Clone)]
pub struct ColorState {
    pub name: String,
    pub available: bool,
    pub variant_count: usize,
    pub available_variant_count: usize,
}

#[derive(Debug, Clone)]
pub struct CatalogProduct {
    pub id: u64,
    pub title: String,
    pub handle: String,
    pub vendor: String,
    pub product_type: String,
    pub product_code: Option<String>,
    pub product_url: String,
    pub image_url: Option<String>,
    pub variants: Vec<VariantState>,
}

impl CatalogProduct {
    pub fn available_variant_count(&self) -> usize {
        self.variants.iter().filter(|v| v.available).count()
    }

    pub fn any_available(&self) -> bool {
        self.available_variant_count() > 0
    }

    pub fn min_price_minor(&self) -> Option<i64> {
        self.variants.iter().map(|v| v.price_minor).min()
    }

    pub fn colors(&self) -> Vec<ColorState> {
        aggregate_colors(&self.variants)
    }

    pub fn search_matches(&self, query: &str) -> bool {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return true;
        }

        self.title.to_lowercase().contains(&q)
            || self.vendor.to_lowercase().contains(&q)
            || self.product_type.to_lowercase().contains(&q)
            || self
                .product_code
                .as_deref()
                .unwrap_or_default()
                .to_lowercase()
                .contains(&q)
            || self.variants.iter().any(|v| {
                v.matches_filter(&q)
                    || v.sku
                        .as_deref()
                        .unwrap_or_default()
                        .to_lowercase()
                        .contains(&q)
            })
    }
}

fn filter_tokens(filter: &str) -> impl Iterator<Item = &str> {
    filter
        .split([',', '|', '，'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn dimension_priority(value: Option<&str>, fallback_title: &str, filter: &str) -> Option<usize> {
    let filter = filter.trim();
    if filter.is_empty() {
        return Some(0);
    }

    filter_tokens(filter)
        .enumerate()
        .find_map(|(index, token)| {
            let matches = if value.is_some() {
                contains_ci(value, token)
            } else {
                fallback_title
                    .to_lowercase()
                    .contains(&token.to_lowercase())
            };
            matches.then_some(index)
        })
}

fn size_priority(value: Option<&str>, title: &str, filter: &str) -> Option<usize> {
    if filter.trim().is_empty() {
        return Some(0);
    }
    filter_tokens(filter)
        .enumerate()
        .find_map(|(index, token)| {
            let matches = if let Some(value) = value {
                value.trim().eq_ignore_ascii_case(token)
            } else {
                title
                    .split('/')
                    .any(|part| part.trim().eq_ignore_ascii_case(token))
            };
            matches.then_some(index)
        })
}

fn contains_ci(value: Option<&str>, needle: &str) -> bool {
    value
        .map(|value| value.to_lowercase().contains(&needle.to_lowercase()))
        .unwrap_or(false)
}

fn aggregate_colors(variants: &[VariantState]) -> Vec<ColorState> {
    let mut out = Vec::<ColorState>::new();
    let mut positions = HashMap::<String, usize>::new();

    for variant in variants {
        let Some(name) = variant
            .color
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        else {
            continue;
        };
        let key = name.to_lowercase();

        if let Some(index) = positions.get(&key).copied() {
            let state = &mut out[index];
            state.variant_count += 1;
            if variant.available {
                state.available = true;
                state.available_variant_count += 1;
            }
        } else {
            positions.insert(key, out.len());
            out.push(ColorState {
                name: name.to_owned(),
                available: variant.available,
                variant_count: 1,
                available_variant_count: if variant.available { 1 } else { 0 },
            });
        }
    }

    out
}
