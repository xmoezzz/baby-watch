use serde::Deserialize;
use thiserror::Error;
use url::Url;

use crate::model::{CatalogProduct, StoreSource, VariantOption, VariantState};

const CATALOG_PAGE_SIZE: usize = 250;
const CATALOG_MAX_PAGES: usize = 20;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("The product URL does not belong to the selected store")]
    UnsupportedUrl,
    #[error("Unrecognized product code or URL")]
    InvalidInput,
    #[error("Paste a specific LaceRococo product URL for the China channel")]
    ChinaUrlRequired,
    #[error("Network request failed: {0}")]
    Network(#[from] reqwest::Error),
    #[error("Product endpoint returned HTTP {0}")]
    Http(reqwest::StatusCode),
    #[error("Could not parse product data: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Catalog pagination exceeded the page limit")]
    CatalogPagination,
}

#[derive(Debug, Clone)]
pub struct ProductSnapshot {
    pub title: String,
    pub vendor: String,
    pub product_code: Option<String>,
    pub image_url: Option<String>,
    pub variants: Vec<VariantState>,
}

#[derive(Debug, Deserialize)]
struct ShopifyProduct {
    title: String,
    vendor: Option<String>,
    #[serde(default)]
    featured_image: Option<String>,
    #[serde(default)]
    images: Vec<String>,
    #[serde(default)]
    options: Vec<ShopifyCatalogOption>,
    variants: Vec<ShopifyVariant>,
}

#[derive(Debug, Deserialize)]
struct ShopifyVariant {
    id: u64,
    title: String,
    available: bool,
    price: serde_json::Value,
    sku: Option<String>,
    #[serde(default)]
    option1: Option<String>,
    #[serde(default)]
    option2: Option<String>,
    #[serde(default)]
    option3: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CatalogResponse {
    products: Vec<ShopifyCatalogProduct>,
}

#[derive(Debug, Deserialize)]
struct ShopifyCatalogProduct {
    id: u64,
    title: String,
    handle: String,
    vendor: Option<String>,
    #[serde(default)]
    product_type: String,
    #[serde(default)]
    options: Vec<ShopifyCatalogOption>,
    #[serde(default)]
    variants: Vec<ShopifyCatalogVariant>,
    #[serde(default)]
    images: Vec<ShopifyImage>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ShopifyCatalogOption {
    Name(String),
    Named { name: String },
}

impl ShopifyCatalogOption {
    fn into_name(self) -> String {
        match self {
            Self::Name(name) | Self::Named { name } => name,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ShopifyCatalogVariant {
    id: u64,
    title: String,
    available: bool,
    price: serde_json::Value,
    sku: Option<String>,
    #[serde(default)]
    option1: Option<String>,
    #[serde(default)]
    option2: Option<String>,
    #[serde(default)]
    option3: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ShopifyImage {
    src: String,
}

pub fn storefront_url(source: StoreSource) -> String {
    format!("https://{}/", source.host())
}

pub fn normalize_product_input(source: StoreSource, input: &str) -> Result<String, StoreError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(StoreError::InvalidInput);
    }

    if input.starts_with("http://") || input.starts_with("https://") {
        let mut url = Url::parse(input).map_err(|_| StoreError::InvalidInput)?;
        let host = url.host_str().unwrap_or_default();
        let host_ok = match source {
            StoreSource::JapanOnlineStore => host.eq_ignore_ascii_case("store.babyssb.co.jp"),
            StoreSource::ChinaLaceRococo => {
                host.eq_ignore_ascii_case("www.lacerococo.com")
                    || host.eq_ignore_ascii_case("lacerococo.com")
            }
        };
        if !host_ok {
            return Err(StoreError::UnsupportedUrl);
        }
        let path = url.path().trim_end_matches('/').to_string();
        if !path.contains("/products/") {
            return Err(StoreError::InvalidInput);
        }
        url.set_scheme("https").ok();
        url.set_host(Some(source.host()))
            .map_err(|_| StoreError::InvalidInput)?;
        url.set_query(None);
        url.set_fragment(None);
        return Ok(url.to_string().trim_end_matches('/').to_string());
    }

    match source {
        StoreSource::JapanOnlineStore => {
            let code = input.to_lowercase();
            let plausible_code =
                code.len() >= 6 && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
            if plausible_code {
                Ok(format!("https://{}/products/{code}", source.host()))
            } else {
                Err(StoreError::InvalidInput)
            }
        }
        StoreSource::ChinaLaceRococo => Err(StoreError::ChinaUrlRequired),
    }
}

pub fn product_json_url(source: StoreSource, product_url: &str) -> Result<String, StoreError> {
    let url = Url::parse(product_url).map_err(|_| StoreError::InvalidInput)?;
    let host = url.host_str().unwrap_or_default();
    let host_ok = match source {
        StoreSource::JapanOnlineStore => host.eq_ignore_ascii_case("store.babyssb.co.jp"),
        StoreSource::ChinaLaceRococo => {
            host.eq_ignore_ascii_case("www.lacerococo.com")
                || host.eq_ignore_ascii_case("lacerococo.com")
        }
    };
    if !host_ok {
        return Err(StoreError::UnsupportedUrl);
    }

    let mut path = url.path().trim_end_matches('/').to_string();
    if !path.contains("/products/") {
        return Err(StoreError::InvalidInput);
    }
    path.push_str(".js");
    Ok(format!("https://{}{}", source.host(), path))
}

pub fn checkout_url(source: StoreSource, variant_id: u64, quantity: u32) -> String {
    format!("https://{}/cart/{variant_id}:{quantity}", source.host())
}

pub fn account_login_url(source: StoreSource) -> String {
    match source {
        StoreSource::JapanOnlineStore => {
            "https://store.babyssb.co.jp/account/login".to_owned()
        }
        StoreSource::ChinaLaceRococo => {
            "https://www.lacerococo.com/customer_authentication/redirect?locale=en&region_country=CN".to_owned()
        }
    }
}

pub fn account_url(source: StoreSource) -> String {
    format!("https://{}/account", source.host())
}

pub fn fetch_product(
    client: &reqwest::blocking::Client,
    source: StoreSource,
    product_url: &str,
) -> Result<ProductSnapshot, StoreError> {
    let json_url = product_json_url(source, product_url)?;
    let response = client.get(json_url).send()?;
    if !response.status().is_success() {
        return Err(StoreError::Http(response.status()));
    }

    let body = response.text()?;
    let product: ShopifyProduct = serde_json::from_str(&body)?;
    let option_names: Vec<_> = product
        .options
        .into_iter()
        .map(ShopifyCatalogOption::into_name)
        .collect();
    let partner_color = (source == StoreSource::ChinaLaceRococo)
        .then(|| infer_partner_color_from_title(&product.title))
        .flatten();

    let variants = product
        .variants
        .into_iter()
        .map(|v| {
            let (options, color, size) = resolve_variant_options(
                &option_names,
                v.option1.as_deref(),
                v.option2.as_deref(),
                v.option3.as_deref(),
                &v.title,
            );
            let color = color.or_else(|| partner_color.clone());
            VariantState {
                id: v.id,
                title: v.title,
                sku: v.sku,
                available: v.available,
                price_minor: parse_ajax_price_minor(&v.price),
                options,
                color,
                size,
            }
        })
        .collect::<Vec<_>>();

    let product_code = variants
        .iter()
        .filter_map(|v| v.sku.clone())
        .find(|s| !s.trim().is_empty())
        .or_else(|| extract_handle(product_url).map(|h| h.to_uppercase()));

    let image_url = product
        .featured_image
        .or_else(|| product.images.into_iter().next());

    Ok(ProductSnapshot {
        title: product.title,
        vendor: product.vendor.unwrap_or_else(|| match source {
            StoreSource::JapanOnlineStore => "BABY, THE STARS SHINE BRIGHT".into(),
            StoreSource::ChinaLaceRococo => "LaceRococo".into(),
        }),
        product_code,
        image_url,
        variants,
    })
}

pub fn fetch_catalog(
    client: &reqwest::blocking::Client,
    source: StoreSource,
) -> Result<Vec<CatalogProduct>, StoreError> {
    let mut out = Vec::new();

    for page in 1..=CATALOG_MAX_PAGES {
        let url = format!(
            "https://{}/collections/all/products.json?limit={CATALOG_PAGE_SIZE}&page={page}",
            source.host()
        );
        let response = client.get(url).send()?;
        if !response.status().is_success() {
            return Err(StoreError::Http(response.status()));
        }

        let payload: CatalogResponse = response.json()?;
        let page_len = payload.products.len();

        for p in payload.products {
            if source == StoreSource::ChinaLaceRococo && !is_baby_partner_product(&p) {
                continue;
            }

            let partner_color = (source == StoreSource::ChinaLaceRococo)
                .then(|| infer_partner_color_from_title(&p.title))
                .flatten();
            let option_names = p
                .options
                .into_iter()
                .map(ShopifyCatalogOption::into_name)
                .collect::<Vec<_>>();
            let variants = p
                .variants
                .into_iter()
                .map(|v| {
                    let (options, color, size) = resolve_variant_options(
                        &option_names,
                        v.option1.as_deref(),
                        v.option2.as_deref(),
                        v.option3.as_deref(),
                        &v.title,
                    );
                    let color = color.or_else(|| partner_color.clone());
                    VariantState {
                        id: v.id,
                        title: v.title,
                        sku: v.sku,
                        available: v.available,
                        price_minor: parse_catalog_price_minor(&v.price),
                        options,
                        color,
                        size,
                    }
                })
                .collect::<Vec<_>>();

            let product_code = variants
                .iter()
                .filter_map(|v| v.sku.clone())
                .find(|s| !s.trim().is_empty());
            let handle = p.handle;
            let product_url = format!("https://{}/products/{handle}", source.host());
            let image_url = p.images.into_iter().next().map(|i| i.src);

            out.push(CatalogProduct {
                id: p.id,
                title: p.title,
                product_url,
                handle,
                vendor: p.vendor.unwrap_or_default(),
                product_type: p.product_type,
                product_code,
                image_url,
                variants,
            });
        }

        if page_len < CATALOG_PAGE_SIZE {
            return Ok(out);
        }
    }

    Err(StoreError::CatalogPagination)
}

fn is_baby_partner_product(product: &ShopifyCatalogProduct) -> bool {
    let haystack = format!(
        "{} {} {}",
        product.title,
        product.vendor.as_deref().unwrap_or_default(),
        product.product_type
    )
    .to_lowercase();
    haystack.contains("baby")
        || haystack.contains("alice and the pirates")
        || haystack.contains("alice & the pirates")
}

fn infer_partner_color_from_title(title: &str) -> Option<String> {
    let normalized = title.trim();
    if normalized.is_empty() {
        return None;
    }

    // LaceRococo frequently represents one color as one Shopify product and
    // leaves the variant as "Default Title". Preserve the visible product
    // color in the normalized model so the GUI still exposes color stock.
    let tokens = normalized
        .split(|c: char| c.is_whitespace() || matches!(c, '/' | '／' | '・' | '|' | '｜'))
        .filter(|token| !token.trim().is_empty())
        .collect::<Vec<_>>();

    for token in tokens.iter().rev() {
        let upper = token.to_ascii_uppercase();
        if token.ends_with('色')
            || [
                "SAX", "PINK", "RED", "BLACK", "WHITE", "IVORY", "BROWN", "NAVY", "BLUE", "GREEN",
                "PURPLE", "LAVENDER",
            ]
            .iter()
            .any(|name| upper == *name || upper.starts_with(&format!("{name}色")))
        {
            return Some((*token).to_owned());
        }
    }
    None
}

fn resolve_variant_options(
    option_names: &[String],
    option1: Option<&str>,
    option2: Option<&str>,
    option3: Option<&str>,
    variant_title: &str,
) -> (Vec<VariantOption>, Option<String>, Option<String>) {
    let values = [option1, option2, option3];
    let color_index = option_names
        .iter()
        .position(|name| is_color_option(name))
        .or_else(|| infer_single_color_option(option_names));
    let size_index = option_names.iter().position(|name| is_size_option(name));

    let mut options = Vec::new();
    for (index, name) in option_names.iter().take(3).enumerate() {
        if let Some(value) = values[index].map(str::trim).filter(|v| !v.is_empty()) {
            options.push(VariantOption {
                name: name.clone(),
                value: value.to_owned(),
            });
        }
    }

    let color = color_index
        .and_then(|index| values.get(index).copied().flatten())
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "Default Title")
        .map(str::to_owned)
        .or_else(|| fallback_color(option_names, variant_title));

    let size = size_index
        .and_then(|index| values.get(index).copied().flatten())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);

    (options, color, size)
}

fn infer_single_color_option(option_names: &[String]) -> Option<usize> {
    if option_names.len() != 1 {
        return None;
    }
    let name = option_names[0].trim();
    if name.is_empty() || is_size_option(name) || is_default_title_option(name) {
        None
    } else {
        Some(0)
    }
}

fn fallback_color(option_names: &[String], variant_title: &str) -> Option<String> {
    if !option_names.is_empty() {
        return None;
    }
    let title = variant_title.trim();
    if title.is_empty() || title == "Default Title" || title.contains(" / ") {
        None
    } else {
        Some(title.to_owned())
    }
}

fn is_color_option(name: &str) -> bool {
    let normalized = name.trim().to_lowercase();
    normalized == "color"
        || normalized == "colour"
        || normalized == "カラー"
        || normalized == "色"
        || normalized.contains("color")
        || normalized.contains("colour")
        || normalized.contains("カラー")
}

fn is_size_option(name: &str) -> bool {
    let normalized = name.trim().to_lowercase();
    normalized == "size"
        || normalized == "サイズ"
        || normalized.contains("size")
        || normalized.contains("サイズ")
}

fn is_default_title_option(name: &str) -> bool {
    let normalized = name.trim().to_lowercase();
    normalized == "title" || normalized == "default" || normalized == "default title"
}

fn parse_ajax_price_minor(value: &serde_json::Value) -> i64 {
    match value {
        serde_json::Value::Number(n) => n.as_i64().unwrap_or_default(),
        serde_json::Value::String(s) => {
            if s.contains('.') {
                s.parse::<f64>()
                    .map(|v| (v * 100.0).round() as i64)
                    .unwrap_or_default()
            } else {
                s.parse::<i64>().unwrap_or_default()
            }
        }
        _ => 0,
    }
}

fn parse_catalog_price_minor(value: &serde_json::Value) -> i64 {
    match value {
        serde_json::Value::Number(n) => n
            .as_f64()
            .map(|v| (v * 100.0).round() as i64)
            .unwrap_or_default(),
        serde_json::Value::String(s) => s
            .parse::<f64>()
            .map(|v| (v * 100.0).round() as i64)
            .unwrap_or_default(),
        _ => 0,
    }
}

fn extract_handle(product_url: &str) -> Option<String> {
    let url = Url::parse(product_url).ok()?;
    let parts = url.path_segments()?.collect::<Vec<_>>();
    let product_pos = parts.iter().position(|p| *p == "products")?;
    parts.get(product_pos + 1).map(|s| s.to_string())
}

pub fn format_jpy(minor: i64) -> String {
    let yen = minor / 100;
    format!("¥{}", group_digits(yen))
}

fn group_digits(value: i64) -> String {
    let raw = value.abs().to_string();
    let mut out = String::new();
    for (i, ch) in raw.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    let mut result = out.chars().rev().collect::<String>();
    if value < 0 {
        result.insert(0, '-');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_current_and_legacy_ajax_option_shapes() {
        for options in [
            r#"["Color","Size"]"#,
            r#"[{"name":"Color","position":1,"values":["Pink"]},{"name":"Size","position":2,"values":["M"]}]"#,
        ] {
            let json = format!(
                r#"{{"title":"Test","vendor":"BABY","options":{options},"variants":[{{"id":1,"title":"Pink / M","available":true,"price":3168000,"sku":"B123","option1":"Pink","option2":"M"}}]}}"#
            );
            let product: ShopifyProduct = serde_json::from_str(&json).unwrap();
            let names: Vec<_> = product
                .options
                .into_iter()
                .map(ShopifyCatalogOption::into_name)
                .collect();
            let (_, color, size) =
                resolve_variant_options(&names, Some("Pink"), Some("M"), None, "Pink / M");
            assert_eq!(color.as_deref(), Some("Pink"));
            assert_eq!(size.as_deref(), Some("M"));
            assert_eq!(parse_ajax_price_minor(&product.variants[0].price), 3168000);
        }
    }

    #[test]
    #[ignore = "read-only live storefront check; requires network"]
    fn live_japan_product_schema() {
        let client = reqwest::blocking::Client::builder()
            .user_agent("BABY-Watch/0.7.0 stock-monitor")
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .unwrap();
        let catalog: CatalogResponse = client
            .get("https://store.babyssb.co.jp/collections/all/products.json?limit=1")
            .send()
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .unwrap();
        let product = &catalog.products[0];
        let snapshot = fetch_product(
            &client,
            StoreSource::JapanOnlineStore,
            &format!("https://store.babyssb.co.jp/products/{}", product.handle),
        )
        .unwrap();
        assert!(!snapshot.variants.is_empty());
        let variant = &snapshot.variants[0];
        let original = product
            .variants
            .iter()
            .find(|v| v.id == variant.id)
            .unwrap();
        assert_eq!(
            variant.price_minor,
            parse_catalog_price_minor(&original.price)
        );
        assert!(variant.price_minor > 0);
    }

    #[test]
    fn normalizes_product_code() {
        assert_eq!(
            normalize_product_input(StoreSource::JapanOnlineStore, "B49BG835").unwrap(),
            "https://store.babyssb.co.jp/products/b49bg835"
        );
    }

    #[test]
    fn extracts_color_and_size_from_shopify_options() {
        let names = vec!["Color".to_string(), "Size".to_string()];
        let (_, color, size) = resolve_variant_options(
            &names,
            Some("ピンク Pink"),
            Some("M"),
            None,
            "ピンク Pink / M",
        );
        assert_eq!(color.as_deref(), Some("ピンク Pink"));
        assert_eq!(size.as_deref(), Some("M"));
    }

    #[test]
    fn does_not_treat_size_only_as_color() {
        let names = vec!["Size".to_string()];
        let (_, color, size) = resolve_variant_options(&names, Some("L"), None, None, "L");
        assert_eq!(color, None);
        assert_eq!(size.as_deref(), Some("L"));
    }

    #[test]
    fn groups_yen() {
        assert_eq!(format_jpy(3168000), "¥31,680");
    }
}
