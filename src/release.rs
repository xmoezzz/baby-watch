//! Weekly release windows use JST regardless of the computer's local time zone.
use chrono::{DateTime, Datelike, Duration, FixedOffset, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::model::{CatalogProduct, PurchaseRule, StoreSource, VariantState, WatchItem};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReleaseSchedule {
    pub enabled: bool,
    pub hour: u32,
    pub minute: u32,
    pub prewarm_minutes: u32,
    pub window_minutes: u32,
    pub poll_seconds: u64,
}

impl Default for ReleaseSchedule {
    fn default() -> Self {
        Self {
            enabled: false,
            hour: 12,
            minute: 0,
            prewarm_minutes: 5,
            window_minutes: 15,
            poll_seconds: 5,
        }
    }
}

impl ReleaseSchedule {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.hour > 23 || self.minute > 59 {
            return Err("Set a valid Japan time (00:00–23:59)");
        }
        if self.prewarm_minutes > 60 || !(1..=120).contains(&self.window_minutes) {
            return Err("Prewarm must be 0–60 minutes and the sale window 1–120 minutes");
        }
        if !(5..=60).contains(&self.poll_seconds) {
            return Err("Release polling must be between 5 and 60 seconds");
        }
        Ok(())
    }

    /// Most recent Friday's scheduled opening (it may still be later today).
    fn friday(&self, now: DateTime<Utc>) -> Option<DateTime<FixedOffset>> {
        self.validate().ok()?;
        let jst = FixedOffset::east_opt(9 * 3600)?;
        let local = now.with_timezone(&jst);
        let days = (local.weekday().num_days_from_monday() + 7 - 4) % 7;
        let date = local.date_naive() - Duration::days(i64::from(days));
        jst.from_local_datetime(&date.and_hms_opt(self.hour, self.minute, 0)?)
            .single()
    }

    /// Returns the opening time only while an enabled prewarm/release window is active.
    pub fn active_slot(&self, now: DateTime<Utc>) -> Option<DateTime<FixedOffset>> {
        if !self.enabled {
            return None;
        }
        let recent = self.friday(now)?;
        // Also covers a prewarm window crossing Thursday midnight.
        [recent, recent + Duration::weeks(1)]
            .into_iter()
            .find(|opening| {
                now >= *opening - Duration::minutes(i64::from(self.prewarm_minutes))
                    && now < *opening + Duration::minutes(i64::from(self.window_minutes))
            })
    }

    pub fn sale_slot(&self, now: DateTime<Utc>) -> Option<i64> {
        self.active_slot(now)
            .filter(|opening| now >= *opening)
            .map(|opening| opening.timestamp())
    }

    pub fn next_opening(&self, now: DateTime<Utc>) -> Option<DateTime<FixedOffset>> {
        let recent = self.friday(now)?;
        Some(if recent >= now {
            recent
        } else {
            recent + Duration::weeks(1)
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseRule {
    pub id: u64,
    pub source: StoreSource,
    /// Exact product URL/handle, SKU/code, or title terms separated by | (AND).
    pub product_hint: String,
    pub color_priority: String,
    pub purchase: PurchaseRule,
}

impl ReleaseRule {
    pub fn new(id: u64, source: StoreSource) -> Self {
        Self {
            id,
            source,
            product_hint: String::new(),
            color_priority: String::new(),
            purchase: PurchaseRule::default(),
        }
    }

    pub fn matches(&self, source: StoreSource, product: &CatalogProduct) -> bool {
        let hint = self.product_hint.trim();
        if self.source != source || hint.is_empty() {
            return false;
        }
        if hint.starts_with("https://") || hint.starts_with("http://") {
            return crate::shopify::normalize_product_input(source, hint)
                .is_ok_and(|url| url == product.product_url);
        }
        if hint.eq_ignore_ascii_case(&product.handle)
            || product.variants.iter().any(|v| {
                v.sku
                    .as_deref()
                    .is_some_and(|sku| sku.eq_ignore_ascii_case(hint))
            })
        {
            return true;
        }
        let haystack = format!(
            "{}\n{}\n{}",
            product.title,
            product.product_code.as_deref().unwrap_or_default(),
            product
                .variants
                .iter()
                .filter_map(|v| v.sku.as_deref())
                .collect::<Vec<_>>()
                .join("\n")
        )
        .to_lowercase();
        let terms: Vec<_> = hint
            .split('|')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .collect();
        !terms.is_empty()
            && terms
                .iter()
                .all(|term| haystack.contains(&term.to_lowercase()))
    }
}

/// Explicitly selected variants override priorities; automatic selection never pins one.
pub fn resolve_variant(item: &WatchItem) -> Option<VariantState> {
    let eligible = |variant: &&VariantState| {
        variant.available
            && variant.matches_color_priority(&item.variant_filter)
            && variant.matches_size_priority(&item.purchase.size_priority)
            && variant.within_price_limit(item.purchase.max_price_yen)
    };
    if let Some(id) = item.runtime.selected_variant {
        if let Some(variant) = item
            .runtime
            .variants
            .iter()
            .filter(eligible)
            .find(|v| v.id == id)
        {
            return Some(variant.clone());
        }
    }
    item.runtime
        .variants
        .iter()
        .filter(eligible)
        .min_by_key(|v| {
            (
                v.color_priority(&item.variant_filter).unwrap_or(usize::MAX),
                v.size_priority(&item.purchase.size_priority)
                    .unwrap_or(usize::MAX),
                v.id,
            )
        })
        .cloned()
}

/// Retry failed browser launches with a cooldown, suppress successful launches until re-armed.
#[derive(Default)]
pub struct CheckoutGate {
    launched: std::collections::HashSet<u64>,
    attempted: std::collections::HashMap<u64, std::time::Instant>,
}
impl CheckoutGate {
    pub fn ready(&self, id: u64, now: std::time::Instant) -> bool {
        !self.launched.contains(&id)
            && self.attempted.get(&id).is_none_or(|last| {
                now.saturating_duration_since(*last) >= std::time::Duration::from_secs(10)
            })
    }
    pub fn attempted(&mut self, id: u64, now: std::time::Instant) {
        self.attempted.insert(id, now);
    }
    pub fn succeeded(&mut self, id: u64) {
        self.launched.insert(id);
    }
    pub fn reset(&mut self, id: u64) {
        self.launched.remove(&id);
        self.attempted.remove(&id);
    }
    pub fn clear(&mut self) {
        self.launched.clear();
        self.attempted.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }
    fn schedule() -> ReleaseSchedule {
        ReleaseSchedule {
            enabled: true,
            ..Default::default()
        }
    }
    fn variant(id: u64, color: &str, size: &str) -> VariantState {
        VariantState {
            id,
            title: format!("{color} / {size}"),
            sku: None,
            available: true,
            price_minor: 3000000,
            options: vec![],
            color: Some(color.into()),
            size: Some(size.into()),
        }
    }
    #[test]
    fn friday_boundaries_are_jst() {
        let s = schedule();
        assert!(s.active_slot(utc(2026, 10, 16, 2, 54)).is_none());
        assert!(s.active_slot(utc(2026, 10, 16, 2, 55)).is_some());
        assert!(s.sale_slot(utc(2026, 10, 16, 2, 59)).is_none());
        assert_eq!(s.sale_slot(utc(2026, 10, 16, 3, 0)), Some(1792119600));
        assert!(s.sale_slot(utc(2026, 10, 16, 3, 14)).is_some());
        assert!(s.sale_slot(utc(2026, 10, 16, 3, 15)).is_none());
        assert!(s.sale_slot(utc(2026, 10, 17, 3, 0)).is_none());
    }
    #[test]
    fn midnight_and_next_week() {
        let s = ReleaseSchedule {
            hour: 0,
            ..schedule()
        };
        assert!(s.active_slot(utc(2026, 10, 15, 14, 55)).is_some());
        assert!(s.sale_slot(utc(2026, 10, 15, 15, 0)).is_some());
        assert_eq!(
            s.next_opening(utc(2026, 10, 16, 16, 0))
                .unwrap()
                .date_naive()
                .to_string(),
            "2026-10-23"
        );
    }
    #[test]
    fn disabled_and_invalid_schedules_never_activate() {
        assert!(ReleaseSchedule::default()
            .active_slot(utc(2026, 10, 16, 3, 0))
            .is_none());
        let mut s = schedule();
        s.hour = 24;
        assert!(s.active_slot(utc(2026, 10, 16, 3, 0)).is_none());
        s.hour = 12;
        s.poll_seconds = 0;
        assert!(s.validate().is_err());
    }
    #[test]
    fn priorities_price_and_explicit_selection() {
        let mut w = WatchItem::new(
            1,
            StoreSource::JapanOnlineStore,
            String::new(),
            String::new(),
            "ピンク | 黒".into(),
        );
        w.purchase.size_priority = "M | S".into();
        w.runtime.variants = vec![
            variant(1, "黒", "S"),
            variant(2, "ピンク", "S"),
            variant(3, "ピンク", "M"),
        ];
        assert_eq!(resolve_variant(&w).unwrap().id, 3);
        w.purchase.max_price_yen = 29999;
        assert!(resolve_variant(&w).is_none());
        w.purchase.max_price_yen = 30000;
        w.runtime.selected_variant = Some(1);
        assert_eq!(resolve_variant(&w).unwrap().id, 1);
        w.runtime.selected_variant = None;
        w.runtime.variants[2].available = false;
        assert_eq!(resolve_variant(&w).unwrap().id, 2);
        // A short size token must not match an unrelated size/title substring.
        w.purchase.size_priority = "S".into();
        w.runtime.variants = vec![variant(4, "Sax", "L")];
        assert!(resolve_variant(&w).is_none());
    }
    #[test]
    fn matching_requires_region_and_all_title_terms() {
        let p = CatalogProduct {
            id: 1,
            title: "Rose ジャンパースカート".into(),
            handle: "rose-jsk".into(),
            vendor: "BABY".into(),
            product_type: String::new(),
            product_code: Some("B123".into()),
            product_url: "https://store.babyssb.co.jp/products/rose-jsk".into(),
            image_url: None,
            variants: vec![],
        };
        let mut r = ReleaseRule::new(1, StoreSource::JapanOnlineStore);
        assert!(!r.matches(r.source, &p));
        r.product_hint = "Rose | ジャンパースカート".into();
        assert!(r.matches(r.source, &p));
        assert!(!r.matches(StoreSource::ChinaLaceRococo, &p));
        r.product_hint = "Rose | ボンネット".into();
        assert!(!r.matches(r.source, &p));
        r.product_hint = p.product_url.clone() + "?variant=123";
        assert!(r.matches(r.source, &p));
        r.product_hint = "|||".into();
        assert!(!r.matches(r.source, &p));
    }
    #[test]
    fn browser_retry_and_once_per_arm() {
        let mut gate = CheckoutGate::default();
        let now = std::time::Instant::now();
        assert!(gate.ready(1, now));
        gate.attempted(1, now);
        assert!(!gate.ready(1, now + std::time::Duration::from_secs(9)));
        assert!(gate.ready(1, now + std::time::Duration::from_secs(10)));
        gate.succeeded(1);
        assert!(!gate.ready(1, now + std::time::Duration::from_secs(60)));
        gate.reset(1);
        assert!(gate.ready(1, now));
    }
}
