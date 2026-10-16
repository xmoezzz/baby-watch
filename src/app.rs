use std::collections::{HashMap, HashSet, VecDeque};

use chrono::{DateTime, Local, Utc};
use crossbeam_channel::{Receiver, Sender};
use eframe::egui::{
    self, Align, Color32, CornerRadius, FontId, Frame, Layout, Margin, RichText, Stroke, Vec2,
};

use crate::{
    browser,
    image_cache::ImageCache,
    model::{CatalogProduct, StoreSource, VariantState, WatchItem},
    monitor::{CatalogChange, MonitorCommand, MonitorEvent},
    release::{self, CheckoutGate, ReleaseRule, ReleaseSchedule},
    shopify,
    storage::{self, BrowserChoice, PersistedState, Settings},
};

const BG: Color32 = Color32::from_rgb(248, 244, 246);
const PANEL: Color32 = Color32::from_rgb(255, 252, 253);
const INK: Color32 = Color32::from_rgb(55, 45, 51);
const MUTED: Color32 = Color32::from_rgb(132, 116, 124);
const ROSE: Color32 = Color32::from_rgb(177, 105, 135);
const ROSE_SOFT: Color32 = Color32::from_rgb(242, 224, 232);
const GREEN: Color32 = Color32::from_rgb(56, 139, 101);
const RED: Color32 = Color32::from_rgb(181, 75, 82);
const BORDER: Color32 = Color32::from_rgb(231, 218, 224);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Catalog,
    Watchlist,
    Purchase,
    Release,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrandFilter {
    Baby,
    Pirates,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoginStatus {
    Unknown,
    NeedsRecheck,
    Ready,
}

#[derive(Debug, Clone)]
struct PurchaseLogEntry {
    at: DateTime<Local>,
    text: String,
}

pub struct BabyWatchApp {
    watch_items: Vec<WatchItem>,
    settings: Settings,
    command_tx: Sender<MonitorCommand>,
    event_rx: Receiver<MonitorEvent>,
    image_cache: ImageCache,

    page: Page,
    active_region: Option<StoreSource>,
    catalog: Vec<CatalogProduct>,
    catalog_loading: bool,
    catalog_last_checked: Option<DateTime<Local>>,
    catalog_error: Option<String>,
    catalog_search: String,
    catalog_stock_only: bool,
    catalog_brand: BrandFilter,

    add_input: String,
    add_filter: String,
    add_error: Option<String>,
    selected_id: Option<u64>,
    next_id: u64,
    toast: Option<(String, f64)>,

    japan_login_confirmed_at: Option<DateTime<Local>>,
    japan_login_last_confirmed_at: Option<DateTime<Local>>,
    china_login_confirmed_at: Option<DateTime<Local>>,
    china_login_last_confirmed_at: Option<DateTime<Local>>,
    armed_ids: HashSet<u64>,
    checkout_gate: CheckoutGate,
    release_rules: Vec<ReleaseRule>,
    release_armed: HashSet<u64>,
    release_bound: HashMap<u64, u64>,
    release_completed: HashSet<u64>,
    release_status: HashMap<u64, String>,
    release_slot: Option<i64>,
    release_run: Option<i64>,
    sent_schedule: Option<Option<ReleaseSchedule>>,
    purchase_log: VecDeque<PurchaseLogEntry>,
}

impl BabyWatchApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        command_tx: Sender<MonitorCommand>,
        event_rx: Receiver<MonitorEvent>,
    ) -> Self {
        crate::fonts::configure(&cc.egui_ctx);
        configure_style(&cc.egui_ctx);
        let persisted = storage::load();
        let next_id = persisted
            .watch_items
            .iter()
            .map(|x| x.id)
            .max()
            .unwrap_or(0)
            + 1;
        let previous_japan_login = persisted
            .japan_login_last_confirmed_at
            .or(persisted.login_last_confirmed_at);
        let previous_china_login = persisted.china_login_last_confirmed_at;

        let app = Self {
            watch_items: persisted.watch_items,
            settings: persisted.settings,
            command_tx,
            event_rx,
            image_cache: ImageCache::new(),
            page: Page::Catalog,
            active_region: None,
            catalog: Vec::new(),
            catalog_loading: false,
            catalog_last_checked: None,
            catalog_error: None,
            catalog_search: String::new(),
            catalog_stock_only: false,
            catalog_brand: BrandFilter::Baby,
            add_input: String::new(),
            add_filter: String::new(),
            add_error: None,
            selected_id: None,
            next_id,
            toast: None,
            japan_login_confirmed_at: None,
            japan_login_last_confirmed_at: previous_japan_login,
            china_login_confirmed_at: None,
            china_login_last_confirmed_at: previous_china_login,
            armed_ids: HashSet::new(),
            checkout_gate: CheckoutGate::default(),
            release_rules: persisted.release_rules,
            release_armed: HashSet::new(),
            release_bound: HashMap::new(),
            release_completed: HashSet::new(),
            release_status: HashMap::new(),
            release_slot: None,
            release_run: None,
            sent_schedule: None,
            purchase_log: VecDeque::new(),
        };

        app.sync_monitor();
        app
    }

    fn sync_monitor(&self) {
        let _ = self
            .command_tx
            .send(MonitorCommand::Replace(self.watch_items.clone()));
        let _ = self
            .command_tx
            .send(MonitorCommand::SetInterval(self.settings.poll_seconds));
        if let Some(source) = self.active_region {
            let _ = self
                .command_tx
                .send(MonitorCommand::SetCatalogSource(source));
        }
        let _ = self.command_tx.send(MonitorCommand::SetCatalogEnabled(
            self.settings.catalog_monitor && self.active_region.is_some(),
        ));
        let _ = self.command_tx.send(MonitorCommand::SetArmedInterval(
            self.settings.armed_poll_seconds,
        ));
        self.sync_armed_monitor();
    }

    fn sync_armed_monitor(&self) {
        let mut ids = self.armed_ids.iter().copied().collect::<Vec<_>>();
        ids.sort_unstable();
        let _ = self.command_tx.send(MonitorCommand::SetArmed(ids));
    }

    fn login_status_for(&self, source: StoreSource) -> LoginStatus {
        let (current, previous) = match source {
            StoreSource::JapanOnlineStore => (
                self.japan_login_confirmed_at.as_ref(),
                self.japan_login_last_confirmed_at.as_ref(),
            ),
            StoreSource::ChinaLaceRococo => (
                self.china_login_confirmed_at.as_ref(),
                self.china_login_last_confirmed_at.as_ref(),
            ),
        };
        if current.is_some_and(|at| Local::now().signed_duration_since(*at).num_minutes() < 30) {
            LoginStatus::Ready
        } else if current.is_some() || previous.is_some() {
            LoginStatus::NeedsRecheck
        } else {
            LoginStatus::Unknown
        }
    }

    fn login_status(&self) -> LoginStatus {
        self.active_region
            .map(|source| self.login_status_for(source))
            .unwrap_or(LoginStatus::Unknown)
    }

    fn login_ready_for(&self, source: StoreSource) -> bool {
        self.login_status_for(source) == LoginStatus::Ready
    }

    fn login_current_for(&self, source: StoreSource) -> Option<&DateTime<Local>> {
        match source {
            StoreSource::JapanOnlineStore => self.japan_login_confirmed_at.as_ref(),
            StoreSource::ChinaLaceRococo => self.china_login_confirmed_at.as_ref(),
        }
    }

    fn login_previous_for(&self, source: StoreSource) -> Option<&DateTime<Local>> {
        match source {
            StoreSource::JapanOnlineStore => self.japan_login_last_confirmed_at.as_ref(),
            StoreSource::ChinaLaceRococo => self.china_login_last_confirmed_at.as_ref(),
        }
    }

    fn confirm_login_for(&mut self, source: StoreSource) {
        let now = Local::now();
        match source {
            StoreSource::JapanOnlineStore => {
                self.japan_login_confirmed_at = Some(now);
                self.japan_login_last_confirmed_at = Some(now);
            }
            StoreSource::ChinaLaceRococo => {
                self.china_login_confirmed_at = Some(now);
                self.china_login_last_confirmed_at = Some(now);
            }
        }
    }

    fn clear_login_for(&mut self, source: StoreSource) {
        match source {
            StoreSource::JapanOnlineStore => {
                self.japan_login_confirmed_at = None;
                self.japan_login_last_confirmed_at = None;
            }
            StoreSource::ChinaLaceRococo => {
                self.china_login_confirmed_at = None;
                self.china_login_last_confirmed_at = None;
            }
        }
    }

    fn invalidate_runtime_logins(&mut self) {
        self.japan_login_confirmed_at = None;
        self.china_login_confirmed_at = None;
    }

    fn select_region(&mut self, source: StoreSource) {
        if self.active_region == Some(source) {
            return;
        }
        self.disarm_all();
        self.active_region = Some(source);
        self.page = Page::Catalog;
        self.catalog.clear();
        self.catalog_error = None;
        self.catalog_last_checked = None;
        self.catalog_search.clear();
        self.selected_id = None;
        let _ = self
            .command_tx
            .send(MonitorCommand::SetCatalogSource(source));
        let _ = self.command_tx.send(MonitorCommand::RefreshCatalog);
        let _ = self.command_tx.send(MonitorCommand::SetCatalogEnabled(
            self.settings.catalog_monitor,
        ));
        self.check_region_now(source);
        self.push_purchase_log(format!("Store changed: {}", source.store_label()));
    }

    fn check_region_now(&self, source: StoreSource) {
        for id in self
            .watch_items
            .iter()
            .filter(|item| item.source == source && item.enabled)
            .map(|item| item.id)
        {
            let _ = self.command_tx.send(MonitorCommand::CheckNow(Some(id)));
        }
    }

    fn persist(&self) {
        let state = PersistedState {
            watch_items: self.watch_items.clone(),
            release_rules: self.release_rules.clone(),
            settings: self.settings.clone(),
            login_last_confirmed_at: None,
            japan_login_last_confirmed_at: self
                .japan_login_confirmed_at
                .or(self.japan_login_last_confirmed_at),
            china_login_last_confirmed_at: self
                .china_login_confirmed_at
                .or(self.china_login_last_confirmed_at),
        };
        let _ = storage::save(&state);
    }

    fn add_watch(&mut self) {
        let Some(source) = self.active_region else {
            self.add_error = Some("Select a store first".into());
            return;
        };
        match shopify::normalize_product_input(source, &self.add_input) {
            Ok(url) => {
                self.insert_watch(
                    source,
                    self.add_input.trim().to_owned(),
                    url,
                    self.add_filter.trim().to_owned(),
                );
                self.add_input.clear();
                self.add_filter.clear();
                self.add_error = None;
            }
            Err(err) => self.add_error = Some(err.to_string()),
        }
    }

    fn insert_watch(&mut self, source: StoreSource, input: String, url: String, filter: String) {
        if let Some(existing) = self
            .watch_items
            .iter()
            .find(|w| w.source == source && w.product_url == url)
        {
            self.selected_id = Some(existing.id);
            self.page = Page::Watchlist;
            self.toast = Some(("This product is already on your watchlist".into(), 0.0));
            return;
        }

        let item = WatchItem::new(self.next_id, source, input, url, filter);
        let id = item.id;
        self.next_id += 1;
        self.watch_items.push(item);
        self.selected_id = Some(id);
        self.persist();
        self.sync_monitor();
        let _ = self.command_tx.send(MonitorCommand::CheckNow(Some(id)));
    }

    fn resolve_purchase_variant(item: &WatchItem) -> Option<VariantState> {
        release::resolve_variant(item)
    }

    fn push_purchase_log(&mut self, text: impl Into<String>) {
        self.purchase_log.push_front(PurchaseLogEntry {
            at: Local::now(),
            text: text.into(),
        });
        while self.purchase_log.len() > 80 {
            self.purchase_log.pop_back();
        }
    }

    fn launch_checkout(&mut self, id: u64, reason: &str, mark_triggered: bool) -> bool {
        let Some((source, variant, title, quantity)) = self
            .watch_items
            .iter()
            .find(|item| item.id == id)
            .and_then(|item| {
                Self::resolve_purchase_variant(item).map(|variant| {
                    let title = item
                        .runtime
                        .title
                        .clone()
                        .unwrap_or_else(|| item.input.clone());
                    (
                        item.source,
                        variant,
                        title,
                        item.purchase.quantity.clamp(1, 3),
                    )
                })
            })
        else {
            self.toast = Some((
                "No available variant matches your purchase rules".into(),
                0.0,
            ));
            return false;
        };

        if mark_triggered {
            if !self.auto_checkout_allowed(id)
                || !self.checkout_gate.ready(id, std::time::Instant::now())
            {
                return false;
            }
            self.checkout_gate.attempted(id, std::time::Instant::now());
        }
        let url = shopify::checkout_url(source, variant.id, quantity);
        match browser::open_url(self.settings.browser, &url) {
            Ok(_) => {
                if mark_triggered {
                    self.checkout_gate.succeeded(id);
                    if let Some(rule_id) = self.release_bound.get(&id) {
                        self.release_completed.insert(*rule_id);
                        self.release_status.insert(
                            *rule_id,
                            "Checkout opened; this arm will not trigger again".into(),
                        );
                    }
                    self.armed_ids.remove(&id);
                    self.sync_armed_monitor();
                }
                self.push_purchase_log(format!(
                    "{reason}: {} · {} → store checkout",
                    title,
                    variant.display_label()
                ));
                self.toast = Some((
                    format!("Checkout opened: {} · {}", title, variant.display_label()),
                    0.0,
                ));
                true
            }
            Err(error) => {
                self.push_purchase_log(format!("Failed to open checkout: {error}"));
                self.toast = Some((format!("Could not open the browser: {error}"), 0.0));
                false
            }
        }
    }

    fn arm_target(&mut self, id: u64) {
        let Some(source) = self
            .watch_items
            .iter()
            .find(|item| item.id == id)
            .map(|item| item.source)
        else {
            return;
        };
        if self.active_region != Some(source) {
            self.select_region(source);
        }
        if !self.login_ready_for(source) {
            self.page = Page::Purchase;
            self.toast = Some((
                format!(
                    "Sign in to {} and confirm the browser session first",
                    source.store_label()
                ),
                0.0,
            ));
            return;
        }

        if let Some(rule_id) = self.release_bound.get(&id).copied() {
            self.disarm_release_rule(rule_id);
        }
        if self.armed_ids.insert(id) {
            self.checkout_gate.reset(id);
            let title = self
                .watch_items
                .iter()
                .find(|item| item.id == id)
                .and_then(|item| item.runtime.title.clone())
                .unwrap_or_else(|| format!("watch #{id}"));
            self.push_purchase_log(format!("ARMED：{title}"));
            self.sync_armed_monitor();
        }

        // Always re-fetch before auto checkout; the displayed snapshot may be old.
        let _ = self.command_tx.send(MonitorCommand::CheckNow(Some(id)));
    }

    fn disarm_target(&mut self, id: u64) {
        if let Some(rule_id) = self.release_bound.remove(&id) {
            self.release_armed.remove(&rule_id);
            self.release_status.insert(rule_id, "Disarmed".into());
        }
        if self.armed_ids.remove(&id) {
            self.checkout_gate.reset(id);
            self.sync_armed_monitor();
            self.push_purchase_log(format!("DISARMED：watch #{id}"));
        }
    }

    fn disarm_all(&mut self) {
        self.disarm_release();
        if !self.armed_ids.is_empty() {
            self.armed_ids.clear();
            self.checkout_gate.clear();
            self.sync_armed_monitor();
            self.push_purchase_log("All purchase targets disarmed");
        }
    }

    fn process_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.event_rx.try_recv() {
            match event {
                MonitorEvent::CheckStarted(id) => {
                    if let Some(item) = self.watch_items.iter_mut().find(|i| i.id == id) {
                        item.runtime.checking = true;
                    }
                }
                MonitorEvent::Updated {
                    id,
                    snapshot,
                    checked_at,
                    became_available,
                } => {
                    let update_info = if let Some(item) =
                        self.watch_items.iter_mut().find(|item| item.id == id)
                    {
                        item.runtime.checking = false;
                        item.runtime.last_error = None;
                        item.runtime.last_checked = Some(checked_at);
                        item.runtime.title = Some(snapshot.title.clone());
                        item.runtime.vendor = Some(snapshot.vendor);
                        item.runtime.product_code = snapshot.product_code;
                        item.runtime.image_url = snapshot.image_url;
                        item.runtime.variants = snapshot.variants;

                        if item.runtime.selected_variant.is_some_and(|selected_id| {
                            !item
                                .runtime
                                .variants
                                .iter()
                                .any(|variant| variant.id == selected_id)
                        }) {
                            item.runtime.selected_variant = None;
                        }

                        let title = item
                            .runtime
                            .title
                            .clone()
                            .unwrap_or_else(|| "BABY product".into());
                        let buyable = Self::resolve_purchase_variant(item).is_some();
                        Some((
                            item.source,
                            title,
                            buyable,
                            item.purchase.auto_open_checkout,
                        ))
                    } else {
                        None
                    };

                    if let Some((source, title, buyable, auto_open_checkout)) = update_info {
                        if became_available {
                            self.toast = Some((
                                format!("Restocked: {title}"),
                                ctx.input(|input| input.time),
                            ));
                            if self.settings.notifications {
                                let _ = notify_rust::Notification::new()
                                    .summary("BABY Watch · Restock")
                                    .body(&format!(
                                        "{title}\nYour target color / variant is now available"
                                    ))
                                    .show();
                            }
                        }

                        let should_auto_open = buyable
                            && auto_open_checkout
                            && self.login_ready_for(source)
                            && self.armed_ids.contains(&id)
                            && self.auto_checkout_allowed(id)
                            && self.checkout_gate.ready(id, std::time::Instant::now());
                        if should_auto_open {
                            self.launch_checkout(id, "ARMED stock match", true);
                        }
                    }
                }
                MonitorEvent::Failed {
                    id,
                    message,
                    checked_at,
                } => {
                    if let Some(item) = self.watch_items.iter_mut().find(|i| i.id == id) {
                        item.runtime.checking = false;
                        item.runtime.last_checked = Some(checked_at);
                        item.runtime.last_error = Some(message);
                    }
                }
                MonitorEvent::CatalogStarted { source } => {
                    if self.active_region == Some(source) {
                        self.catalog_loading = true;
                    }
                }
                MonitorEvent::CatalogUpdated {
                    source,
                    products,
                    checked_at,
                    changes,
                } => {
                    if self.active_region == Some(source) {
                        self.catalog_loading = false;
                        self.catalog_error = None;
                        self.catalog_last_checked = Some(checked_at);
                        self.catalog = products;
                        self.handle_catalog_changes(ctx, &changes);
                    }
                }
                MonitorEvent::CatalogFailed {
                    source,
                    message,
                    checked_at,
                } => {
                    if self.active_region == Some(source) {
                        self.catalog_loading = false;
                        self.catalog_last_checked = Some(checked_at);
                        self.catalog_error = Some(message);
                    }
                }
            }
        }
    }

    fn handle_catalog_changes(&mut self, ctx: &egui::Context, changes: &[CatalogChange]) {
        if changes.is_empty() {
            return;
        }

        let new_count = changes
            .iter()
            .filter(|c| matches!(c, CatalogChange::NewProduct { .. }))
            .count();
        let restock_count = changes
            .iter()
            .filter(|c| matches!(c, CatalogChange::Restocked { .. }))
            .count();
        let summary =
            format!("Catalog changes: {new_count} new products · {restock_count} restocked colors");
        self.toast = Some((summary.clone(), ctx.input(|i| i.time)));

        if self.settings.notifications {
            let detail = changes
                .iter()
                .take(3)
                .map(|c| match c {
                    CatalogChange::NewProduct { title } => format!("New: {title}"),
                    CatalogChange::Restocked { title, color } => {
                        format!("Restocked: {title} · {color}")
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            let _ = notify_rust::Notification::new()
                .summary("BABY Watch · Catalog changes")
                .body(&detail)
                .show();
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let Some(source) = self.active_region else {
            return;
        };
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(
                    RichText::new("BABY WATCH")
                        .font(FontId::proportional(22.0))
                        .strong()
                        .color(INK),
                );
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("{} · {}", source.region_label(), source.host()))
                            .size(11.0)
                            .color(MUTED),
                    );
                    chip(ui, source.short_label());
                });
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if soft_button(ui, "Switch store").clicked() {
                    self.disarm_all();
                    self.active_region = None;
                    self.catalog.clear();
                    self.catalog_loading = false;
                    self.catalog_error = None;
                    let _ = self
                        .command_tx
                        .send(MonitorCommand::SetCatalogEnabled(false));
                }
                if soft_button(ui, "Open store").clicked() {
                    let _ =
                        browser::open_url(self.settings.browser, &shopify::storefront_url(source));
                }
                if self.catalog_loading {
                    ui.label(RichText::new("◌ syncing catalog").size(12.0).color(ROSE));
                } else {
                    let enabled = self
                        .watch_items
                        .iter()
                        .filter(|w| w.source == source && w.enabled)
                        .count();
                    ui.label(
                        RichText::new(format!(
                            "● {enabled} focused · {} catalog",
                            self.catalog.len()
                        ))
                        .size(12.0)
                        .color(GREEN),
                    );
                }
            });
        });
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.label(RichText::new("BROWSE").size(11.0).strong().color(MUTED));
        ui.add_space(6.0);

        if nav_button(ui, "Catalog", self.page == Page::Catalog).clicked() {
            self.page = Page::Catalog;
        }
        if nav_button(ui, "Watchlist", self.page == Page::Watchlist).clicked() {
            self.page = Page::Watchlist;
        }
        if nav_button(ui, "Friday releases", self.page == Page::Release).clicked() {
            self.page = Page::Release;
        }
        if nav_button(ui, "Purchase setup", self.page == Page::Purchase).clicked() {
            self.page = Page::Purchase;
        }

        ui.add_space(14.0);
        ui.separator();
        ui.add_space(12.0);
        ui.label(RichText::new("STATUS").size(11.0).strong().color(MUTED));

        let source = self.active_region.expect("region gate must run first");
        let region_total = self
            .watch_items
            .iter()
            .filter(|item| item.source == source)
            .count();
        let catalog_stock = self.catalog.iter().filter(|p| p.any_available()).count();
        let focused_ready = self
            .watch_items
            .iter()
            .filter(|item| item.source == source)
            .filter(|i| i.runtime.any_matching_available(&i.variant_filter))
            .count();
        stat(
            ui,
            "Catalog",
            self.catalog.len().to_string(),
            "All store products",
        );
        stat(
            ui,
            "In stock",
            catalog_stock.to_string(),
            "At least one variant in stock",
        );
        stat(
            ui,
            "Focused",
            format!("{focused_ready}/{region_total}"),
            "Product watches",
        );
        let login_summary = match self.login_status() {
            LoginStatus::Ready => "LOGIN READY",
            LoginStatus::NeedsRecheck => "LOGIN NEEDS RECHECK",
            LoginStatus::Unknown => "LOGIN UNKNOWN",
        };
        stat(
            ui,
            "Armed",
            self.armed_ids
                .iter()
                .filter(|id| {
                    self.watch_items
                        .iter()
                        .any(|item| item.id == **id && item.source == source)
                })
                .count()
                .to_string(),
            login_summary,
        );

        ui.add_space(10.0);
        ui.separator();
        ui.add_space(12.0);
        ui.label(RichText::new("SETTINGS").size(11.0).strong().color(MUTED));

        if ui
            .checkbox(&mut self.settings.catalog_monitor, "Monitor catalog")
            .changed()
        {
            self.persist();
            let _ = self.command_tx.send(MonitorCommand::SetCatalogEnabled(
                self.settings.catalog_monitor,
            ));
        }

        let mut seconds = self.settings.poll_seconds as f32;
        if ui
            .add(
                egui::Slider::new(&mut seconds, 30.0..=300.0)
                    .text("Poll interval (s)")
                    .logarithmic(true),
            )
            .changed()
        {
            self.settings.poll_seconds = (seconds.round() as u64).clamp(30, 300);
            self.persist();
            let _ = self
                .command_tx
                .send(MonitorCommand::SetInterval(self.settings.poll_seconds));
        }
        if ui
            .checkbox(&mut self.settings.notifications, "Desktop notifications")
            .changed()
        {
            self.persist();
        }

        let mut armed_seconds = self.settings.armed_poll_seconds as f32;
        if ui
            .add(egui::Slider::new(&mut armed_seconds, 5.0..=30.0).text("ARMED interval (s)"))
            .changed()
        {
            self.settings.armed_poll_seconds = (armed_seconds.round() as u64).clamp(5, 30);
            self.persist();
            let _ = self.command_tx.send(MonitorCommand::SetArmedInterval(
                self.settings.armed_poll_seconds,
            ));
        }

        ui.add_space(12.0);
        if ui
            .add_sized(
                [ui.available_width(), 34.0],
                egui::Button::new("↻ Refresh catalog").corner_radius(9),
            )
            .clicked()
        {
            let _ = self.command_tx.send(MonitorCommand::RefreshCatalog);
            self.check_region_now(source);
        }

        ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
            ui.label(
                RichText::new("v0.7.0 · Friday release")
                    .size(10.5)
                    .color(MUTED),
            );
            ui.label(
                RichText::new("Catalog and variant stock monitor")
                    .size(10.5)
                    .color(MUTED),
            );
        });
    }

    fn catalog_page(&mut self, ui: &mut egui::Ui) {
        Frame::new()
            .fill(PANEL)
            .stroke(Stroke::new(1.0_f32, BORDER))
            .corner_radius(14)
            .inner_margin(Margin::same(16))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new("Catalog").size(18.0).strong().color(INK));
                        let source = self.active_region.expect("region gate must run first");
                        let sub = if let Some(t) = self.catalog_last_checked {
                            format!(
                                "{} · Shopify catalog · Last synced {}",
                                source.store_label(),
                                t.format("%H:%M:%S")
                            )
                        } else {
                            format!("{} · Waiting for first sync", source.store_label())
                        };
                        ui.label(RichText::new(sub).size(11.5).color(MUTED));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add(primary_button("↻ Refresh")).clicked() {
                            let _ = self.command_tx.send(MonitorCommand::RefreshCatalog);
                        }
                    });
                });

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [ui.available_width() * 0.56, 36.0],
                        egui::TextEdit::singleline(&mut self.catalog_search)
                            .hint_text("Search title / B50JS240 / color / SKU"),
                    );
                    ui.checkbox(&mut self.catalog_stock_only, "In stock only");
                    egui::ComboBox::from_id_salt("catalog_brand")
                        .selected_text(match self.catalog_brand {
                            BrandFilter::Baby => "BABY",
                            BrandFilter::Pirates => "A/P",
                            BrandFilter::All => "All brands",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.catalog_brand, BrandFilter::Baby, "BABY");
                            ui.selectable_value(
                                &mut self.catalog_brand,
                                BrandFilter::Pirates,
                                "ALICE and the PIRATES",
                            );
                            ui.selectable_value(
                                &mut self.catalog_brand,
                                BrandFilter::All,
                                "All brands",
                            );
                        });
                });
            });

        ui.add_space(12.0);

        if let Some(error) = &self.catalog_error {
            Frame::new()
                .fill(Color32::from_rgb(255, 246, 247))
                .stroke(Stroke::new(1.0_f32, Color32::from_rgb(235, 190, 194)))
                .corner_radius(10)
                .inner_margin(Margin::same(12))
                .show(ui, |ui| {
                    ui.label(RichText::new(error).size(11.5).color(RED));
                });
            ui.add_space(10.0);
        }

        let indices = self.filtered_catalog_indices();
        let total = indices.len();
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("{total} products"))
                    .size(11.0)
                    .strong()
                    .color(MUTED),
            );
            if self.settings.catalog_monitor {
                chip(ui, "● Catalog monitoring active");
            }
        });
        ui.add_space(7.0);

        if self.catalog.is_empty() {
            Frame::new()
                .fill(PANEL)
                .stroke(Stroke::new(1.0_f32, BORDER))
                .corner_radius(14)
                .inner_margin(Margin::same(28))
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("♡").size(34.0).color(ROSE));
                        ui.label(
                            RichText::new(if self.catalog_loading {
                                "Syncing the store catalog…"
                            } else {
                                "No product data yet"
                            })
                            .size(15.0)
                            .strong()
                            .color(INK),
                        );
                    });
                });
            return;
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_rows(ui, 176.0, total, |ui, rows| {
                for row in rows {
                    let index = indices[row];
                    self.catalog_row(ui, index);
                    ui.add_space(6.0);
                }
            });
    }

    fn filtered_catalog_indices(&self) -> Vec<usize> {
        self.catalog
            .iter()
            .enumerate()
            .filter(|(_, p)| !self.catalog_stock_only || p.any_available())
            .filter(|(_, p)| match self.catalog_brand {
                BrandFilter::All => true,
                BrandFilter::Baby => {
                    let brand = format!("{} {}", p.vendor, p.title).to_lowercase();
                    brand.contains("baby") && !brand.contains("pirates")
                }
                BrandFilter::Pirates => format!("{} {}", p.vendor, p.title)
                    .to_lowercase()
                    .contains("pirates"),
            })
            .filter(|(_, p)| p.search_matches(&self.catalog_search))
            .map(|(i, _)| i)
            .collect()
    }

    fn catalog_row(&mut self, ui: &mut egui::Ui, index: usize) {
        let product = self.catalog[index].clone();
        let thumbnail = product
            .image_url
            .as_deref()
            .and_then(|url| self.image_cache.texture(url));
        let colors = product.colors();
        let available_colors = colors.iter().filter(|color| color.available).count();
        let available_variants = product.available_variant_count();
        let total_variants = product.variants.len();
        let code = product.product_code.as_deref().unwrap_or("—");
        let price = product
            .min_price_minor()
            .map(shopify::format_jpy)
            .unwrap_or_else(|| "—".into());
        let source = self.active_region.expect("region gate must run first");
        let already_watched = self
            .watch_items
            .iter()
            .any(|w| w.source == source && w.product_url == product.product_url);

        Frame::new()
            .fill(PANEL)
            .stroke(Stroke::new(1.0_f32, BORDER))
            .corner_radius(12)
            .inner_margin(Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    product_thumbnail(ui, thumbnail.as_ref(), 88.0);

                    ui.add_space(2.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&product.title).size(14.0).strong().color(INK));
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(code).size(10.5).monospace().color(MUTED));
                            ui.label(RichText::new("·").size(10.5).color(MUTED));
                            ui.label(RichText::new(&product.vendor).size(10.5).color(MUTED));
                            if !product.product_type.is_empty() {
                                ui.label(RichText::new("·").size(10.5).color(MUTED));
                                ui.label(
                                    RichText::new(&product.product_type).size(10.5).color(MUTED),
                                );
                            }
                        });
                        ui.add_space(5.0);
                        ui.horizontal_wrapped(|ui| {
                            chip(ui, &price);
                            if !colors.is_empty() {
                                chip(
                                    ui,
                                    &format!("{available_colors}/{} colors in stock", colors.len()),
                                );
                            } else {
                                chip(
                                    ui,
                                    &format!(
                                        "{available_variants}/{total_variants} variants in stock"
                                    ),
                                );
                            }
                            chip(
                                ui,
                                if product.any_available() {
                                    "● AVAILABLE"
                                } else {
                                    "○ SOLD OUT"
                                },
                            );
                        });

                        if !colors.is_empty() {
                            ui.add_space(5.0);
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new("Color").size(10.5).strong().color(MUTED));
                                for color in &colors {
                                    color_stock_chip(ui, &color.name, color.available);
                                }
                            });
                        }
                    });

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add(primary_button("Open product")).clicked() {
                            let _ = browser::open_url(self.settings.browser, &product.product_url);
                        }
                        let label = if already_watched {
                            "Watching"
                        } else {
                            "＋ Watch product"
                        };
                        if ui
                            .add_enabled(
                                !already_watched,
                                egui::Button::new(label).corner_radius(9),
                            )
                            .clicked()
                        {
                            let input = product
                                .product_code
                                .clone()
                                .unwrap_or_else(|| product.title.clone());
                            self.insert_watch(
                                source,
                                input,
                                product.product_url.clone(),
                                String::new(),
                            );
                            self.toast = Some(("Added to watchlist".into(), ui.input(|i| i.time)));
                        }
                    });
                });
            });
    }

    fn watchlist_page(&mut self, ui: &mut egui::Ui) {
        self.add_panel(ui);
        ui.add_space(18.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| self.watchlist(ui));
    }

    fn add_panel(&mut self, ui: &mut egui::Ui) {
        Frame::new()
            .fill(PANEL)
            .stroke(Stroke::new(1.0_f32, BORDER))
            .corner_radius(14)
            .inner_margin(Margin::same(16))
            .show(ui, |ui| {
                ui.label(
                    RichText::new("Add a product watch")
                        .size(16.0)
                        .strong()
                        .color(INK),
                );
                ui.label(
                    RichText::new(format!(
                        "{} · Watch a specific product, color, and size",
                        self.active_region.expect("region gate").store_label()
                    ))
                    .size(11.5)
                    .color(MUTED),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let available = ui.available_width();
                    ui.add_sized(
                        [available * 0.55, 36.0],
                        egui::TextEdit::singleline(&mut self.add_input).hint_text(
                            if self.active_region == Some(StoreSource::JapanOnlineStore) {
                                "Product URL / code"
                            } else {
                                "LaceRococo product URL"
                            },
                        ),
                    );
                    ui.add_sized(
                        [available * 0.27, 36.0],
                        egui::TextEdit::singleline(&mut self.add_filter)
                            .hint_text("Color: Pink | Black"),
                    );
                    if ui
                        .add_sized([available * 0.16, 36.0], primary_button("＋ Watch"))
                        .clicked()
                    {
                        self.add_watch();
                    }
                });
                if let Some(error) = &self.add_error {
                    ui.add_space(6.0);
                    ui.label(RichText::new(error).size(11.0).color(RED));
                }
            });
    }

    fn watchlist(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("WATCHLIST").size(11.0).strong().color(MUTED));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!(
                        "{} items",
                        self.watch_items
                            .iter()
                            .filter(|item| Some(item.source) == self.active_region)
                            .count()
                    ))
                    .size(11.0)
                    .color(MUTED),
                );
            });
        });
        ui.add_space(7.0);

        let source = self.active_region.expect("region gate must run first");
        let region_item_count = self
            .watch_items
            .iter()
            .filter(|item| item.source == source)
            .count();
        if region_item_count == 0 {
            Frame::new()
                .fill(PANEL)
                .stroke(Stroke::new(1.0_f32, BORDER))
                .corner_radius(14)
                .inner_margin(Margin::same(28))
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("♡").size(34.0).color(ROSE));
                        ui.label(
                            RichText::new("No product watches yet")
                                .size(15.0)
                                .strong()
                                .color(INK),
                        );
                        ui.label(
                            RichText::new(
                                "Add a product from Catalog, or enter a product code above.",
                            )
                            .size(11.5)
                            .color(MUTED),
                        );
                    });
                });
            return;
        }

        let mut delete_id = None;
        let mut needs_sync = false;
        let ids = self
            .watch_items
            .iter()
            .filter(|item| item.source == source)
            .map(|x| x.id)
            .collect::<Vec<_>>();

        for id in ids {
            let Some(index) = self.watch_items.iter().position(|x| x.id == id) else {
                continue;
            };
            let is_selected = self.selected_id == Some(id);
            let thumbnail = self.watch_items[index]
                .runtime
                .image_url
                .as_deref()
                .and_then(|url| self.image_cache.texture(url));
            let card = Frame::new()
                .fill(if is_selected {
                    Color32::from_rgb(255, 249, 252)
                } else {
                    PANEL
                })
                .stroke(Stroke::new(
                    if is_selected { 1.5_f32 } else { 1.0_f32 },
                    if is_selected { ROSE } else { BORDER },
                ))
                .corner_radius(14)
                .inner_margin(Margin::same(15));

            let is_armed = self.armed_ids.contains(&id);
            let mut toggle_arm = false;
            let mut open_checkout = false;

            let response = card.show(ui, |ui| {
                let item = &mut self.watch_items[index];
                let available = item.runtime.any_matching_available(&item.variant_filter);

                ui.horizontal(|ui| {
                    product_thumbnail(ui, thumbnail.as_ref(), 72.0);
                    ui.add_space(2.0);

                    let dot = if item.runtime.checking {
                        "◌"
                    } else if available {
                        "●"
                    } else if item.runtime.last_error.is_some() {
                        "!"
                    } else {
                        "○"
                    };
                    let dot_color = if available {
                        GREEN
                    } else if item.runtime.last_error.is_some() {
                        RED
                    } else {
                        MUTED
                    };
                    ui.label(RichText::new(dot).size(15.0).color(dot_color));

                    ui.vertical(|ui| {
                        let title = item.runtime.title.as_deref().unwrap_or(&item.input);
                        ui.label(RichText::new(title).size(15.0).strong().color(INK));
                        let code = item
                            .runtime
                            .product_code
                            .as_deref()
                            .unwrap_or("waiting for first check");
                        ui.label(RichText::new(code).size(10.5).color(MUTED).monospace());
                    });

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button("×").on_hover_text("Delete watch").clicked() {
                            delete_id = Some(item.id);
                        }
                        if ui
                            .checkbox(&mut item.enabled, "")
                            .on_hover_text("Enable / pause")
                            .changed()
                        {
                            needs_sync = true;
                        }
                        let status = if is_armed {
                            "ARMED"
                        } else if item.runtime.checking {
                            "CHECKING"
                        } else if available {
                            "AVAILABLE"
                        } else if item.runtime.last_error.is_some() {
                            "ERROR"
                        } else {
                            "WATCHING"
                        };
                        ui.label(
                            RichText::new(status)
                                .size(10.5)
                                .strong()
                                .color(if is_armed {
                                    ROSE
                                } else if available {
                                    GREEN
                                } else if item.runtime.last_error.is_some() {
                                    RED
                                } else {
                                    MUTED
                                }),
                        );
                    });
                });

                ui.add_space(9.0);
                ui.horizontal_wrapped(|ui| {
                    chip(
                        ui,
                        &format!(
                            "filter  {}",
                            if item.variant_filter.is_empty() {
                                "All colors / variants"
                            } else {
                                &item.variant_filter
                            }
                        ),
                    );
                    if let Some(t) = item.runtime.last_checked {
                        chip(ui, &format!("checked  {}", t.format("%H:%M:%S")));
                    }
                    if !item.runtime.variants.is_empty() {
                        let colors = item.runtime.colors();
                        if !colors.is_empty() {
                            chip(ui, &format!("{} colors", colors.len()));
                        } else {
                            chip(
                                ui,
                                &format!("{} purchase specs", item.runtime.variants.len()),
                            );
                        }
                    }
                });

                if !item.runtime.variants.is_empty() {
                    let colors = item.runtime.colors();
                    if !colors.is_empty() {
                        ui.add_space(7.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("Color").size(10.5).strong().color(MUTED));
                            for color in &colors {
                                color_stock_chip(ui, &color.name, color.available);
                            }
                        });
                    }
                }

                if let Some(error) = &item.runtime.last_error {
                    ui.add_space(8.0);
                    ui.label(RichText::new(error).size(11.0).color(RED));
                }

                if !item.runtime.variants.is_empty() {
                    ui.add_space(10.0);
                    let selectable = item
                        .runtime
                        .selectable_variants(&item.variant_filter)
                        .into_iter()
                        .cloned()
                        .collect::<Vec<_>>();

                    if selectable.is_empty() {
                        ui.label(
                            RichText::new("No colors / variants match this filter")
                                .size(11.0)
                                .color(RED),
                        );
                    } else {
                        let selected_text = item
                            .runtime
                            .selected_variant
                            .and_then(|id| item.runtime.variants.iter().find(|v| v.id == id))
                            .map(|v| {
                                format!(
                                    "{} · {}{}",
                                    v.display_label(),
                                    shopify::format_jpy(v.price_minor),
                                    if v.available {
                                        " · in stock"
                                    } else {
                                        " · sold out"
                                    }
                                )
                            })
                            .unwrap_or_else(|| "Select a color / variant".into());

                        egui::ComboBox::from_id_salt(("variant", item.id))
                            .selected_text(selected_text)
                            .width(ui.available_width())
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut item.runtime.selected_variant,
                                    None,
                                    "Choose automatically by color / size priority",
                                );
                                for v in &selectable {
                                    let text = format!(
                                        "{} · {}{}",
                                        v.display_label(),
                                        shopify::format_jpy(v.price_minor),
                                        if v.available {
                                            " · in stock"
                                        } else {
                                            " · sold out"
                                        }
                                    );
                                    ui.selectable_value(
                                        &mut item.runtime.selected_variant,
                                        Some(v.id),
                                        text,
                                    );
                                }
                            });
                    }
                }

                ui.add_space(11.0);
                Frame::new()
                    .fill(Color32::from_rgb(252, 247, 249))
                    .stroke(Stroke::new(1.0_f32, BORDER))
                    .corner_radius(10)
                    .inner_margin(Margin::symmetric(11, 9))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("Purchase rules")
                                    .size(11.0)
                                    .strong()
                                    .color(INK),
                            );
                            ui.label(
                                RichText::new(if is_armed {
                                    "● ARMED"
                                } else {
                                    "○ DISARMED"
                                })
                                .size(10.5)
                                .strong()
                                .color(if is_armed {
                                    ROSE
                                } else {
                                    MUTED
                                }),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(
                                    RichText::new(
                                        "Confirm the final order and payment on the store checkout",
                                    )
                                    .size(10.0)
                                    .color(MUTED),
                                );
                            });
                        });
                        ui.add_space(6.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("Color priority").size(10.5).color(MUTED));
                            let color_response = ui.add(
                                egui::TextEdit::singleline(&mut item.variant_filter)
                                    .hint_text("Example: Pink | Red | Black")
                                    .desired_width(220.0),
                            );
                            if color_response.changed() {
                                item.runtime.selected_variant = None;
                                needs_sync = true;
                            }

                            ui.separator();
                            ui.label(RichText::new("Size priority").size(10.5).color(MUTED));
                            let size_response = ui.add(
                                egui::TextEdit::singleline(&mut item.purchase.size_priority)
                                    .hint_text("Example: M | S")
                                    .desired_width(140.0),
                            );
                            if size_response.changed() {
                                item.runtime.selected_variant = None;
                                needs_sync = true;
                            }

                            ui.separator();
                            ui.label(RichText::new("Price ceiling").size(10.5).color(MUTED));
                            let price_response = ui.add(
                                egui::DragValue::new(&mut item.purchase.max_price_yen)
                                    .range(0..=500_000)
                                    .speed(1_000.0)
                                    .prefix("¥"),
                            );
                            if price_response.changed() {
                                needs_sync = true;
                            }
                            ui.label(RichText::new("0 = no limit").size(9.5).color(MUTED));
                        });
                        if ui
                            .checkbox(
                                &mut item.purchase.auto_open_checkout,
                                "Open store checkout automatically on an ARMED match",
                            )
                            .changed()
                        {
                            needs_sync = true;
                        }
                    });

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if soft_button(ui, "↻ Check").clicked() {
                        let _ = self
                            .command_tx
                            .send(MonitorCommand::CheckNow(Some(item.id)));
                    }
                    if soft_button(ui, "Open product").clicked() {
                        let _ = browser::open_url(self.settings.browser, &item.product_url);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let buyable = Self::resolve_purchase_variant(item).is_some();
                        let checkout_label = if buyable {
                            "Open checkout →"
                        } else {
                            "Waiting for stock"
                        };
                        if ui
                            .add_enabled(buyable, primary_button(checkout_label))
                            .clicked()
                        {
                            open_checkout = true;
                        }

                        let arm_label = if is_armed { "Disarm" } else { "Arm target" };
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new(arm_label).strong().color(if is_armed {
                                        RED
                                    } else {
                                        ROSE
                                    }),
                                )
                                .fill(if is_armed { Color32::WHITE } else { ROSE_SOFT })
                                .stroke(Stroke::new(1.0_f32, if is_armed { RED } else { ROSE }))
                                .corner_radius(9)
                                .min_size(Vec2::new(100.0, 34.0)),
                            )
                            .clicked()
                        {
                            toggle_arm = true;
                        }
                    });
                });
            });

            if response.response.clicked() {
                self.selected_id = Some(id);
            }
            if open_checkout {
                self.launch_checkout(id, "Manual checkout", false);
            }
            if toggle_arm {
                if is_armed {
                    self.disarm_target(id);
                } else {
                    self.arm_target(id);
                }
            }
            ui.add_space(9.0);
        }

        if let Some(id) = delete_id {
            self.disarm_target(id);
            self.watch_items.retain(|x| x.id != id);
            self.armed_ids.remove(&id);
            self.checkout_gate.reset(id);
            if self.selected_id == Some(id) {
                self.selected_id = None;
            }
            needs_sync = true;
        }
        if needs_sync {
            self.persist();
            self.sync_monitor();
        }
    }

    fn purchase_page(&mut self, ui: &mut egui::Ui) {
        let Some(source) = self.active_region else {
            return;
        };
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new(format!("{} · Purchase setup", source.region_label()))
                                .size(24.0)
                                .strong()
                                .color(INK),
                        );
                        ui.label(
                            RichText::new(format!(
                                "{} · Separate browser session · ARMED targets and checkout use this store",
                                source.store_label()
                            ))
                            .size(11.5)
                            .color(MUTED),
                        );
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let (text, color) = match self.login_status_for(source) {
                            LoginStatus::Ready => ("● LOGIN READY", GREEN),
                            LoginStatus::NeedsRecheck => ("◐ LOGIN NEEDS RECHECK", ROSE),
                            LoginStatus::Unknown => ("○ LOGIN UNKNOWN", RED),
                        };
                        ui.label(RichText::new(text).size(11.0).strong().color(color));
                    });
                });

                ui.add_space(16.0);
                Frame::new()
                    .fill(PANEL)
                    .stroke(Stroke::new(1.0_f32, BORDER))
                    .corner_radius(14)
                    .inner_margin(Margin::same(18))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label(
                                    RichText::new(format!("1 · {} account and purchase browser", source.region_label()))
                                        .size(15.0)
                                        .strong()
                                        .color(INK),
                                );
                                let account_hint = match source {
                                    StoreSource::JapanOnlineStore => {
                                        "The Japan store uses a separate BABY/Shopify account. Its session stays in your browser profile."
                                    }
                                    StoreSource::ChinaLaceRococo => {
                                        "LaceRococo has its own Shopify Customer Account, separate from the Japan store session."
                                    }
                                };
                                ui.label(RichText::new(account_hint).size(11.0).color(MUTED));
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if soft_button(ui, "Verify account page").clicked() {
                                    let url = shopify::account_url(source);
                                    if let Err(error) = browser::open_url(self.settings.browser, &url) {
                                        self.toast = Some((format!("Could not open the account page: {error}"), 0.0));
                                    }
                                }
                                let login_label = match source {
                                    StoreSource::JapanOnlineStore => "Sign in to Japan store",
                                    StoreSource::ChinaLaceRococo => "Sign in to China partner store",
                                };
                                if ui.add(primary_button(login_label)).clicked() {
                                    let url = shopify::account_login_url(source);
                                    match browser::open_url(self.settings.browser, &url) {
                                        Ok(()) => self.push_purchase_log(format!(
                                            "Opened the {} sign-in page for {}",
                                            self.settings.browser.label(),
                                            source.store_label()
                                        )),
                                        Err(error) => {
                                            self.toast = Some((format!("Could not open the sign-in page: {error}"), 0.0));
                                        }
                                    }
                                }
                            });
                        });

                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Purchase browser").size(10.5).strong().color(MUTED));
                            let old_browser = self.settings.browser;
                            egui::ComboBox::from_id_salt("purchase_browser_choice")
                                .selected_text(self.settings.browser.label())
                                .width(150.0)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut self.settings.browser,
                                        BrowserChoice::SystemDefault,
                                        BrowserChoice::SystemDefault.label(),
                                    );
                                    if cfg!(target_os = "macos") {
                                        ui.selectable_value(
                                            &mut self.settings.browser,
                                            BrowserChoice::Safari,
                                            BrowserChoice::Safari.label(),
                                        );
                                        ui.selectable_value(
                                            &mut self.settings.browser,
                                            BrowserChoice::Chrome,
                                            BrowserChoice::Chrome.label(),
                                        );
                                    }
                                });
                            if self.settings.browser != old_browser {
                                self.invalidate_runtime_logins();
                                self.japan_login_last_confirmed_at = None;
                                self.china_login_last_confirmed_at = None;
                                self.disarm_all();
                                self.persist();
                                self.push_purchase_log(format!(
                                    "Purchase browser changed to {}; confirm both store sessions again",
                                    self.settings.browser.label()
                                ));
                            }
                            ui.label(
                                RichText::new("Both stores use this browser, with separate cookies and sessions")
                                    .size(10.0)
                                    .color(MUTED),
                            );
                        });

                        ui.add_space(12.0);
                        Frame::new()
                            .fill(Color32::from_rgb(252, 247, 249))
                            .corner_radius(10)
                            .inner_margin(Margin::symmetric(12, 10))
                            .show(ui, |ui| {
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(RichText::new("Store sign-in steps:").size(10.5).strong().color(MUTED));
                                    chip(ui, source.region_label());
                                    ui.label(RichText::new("→").color(MUTED));
                                    chip(ui, "Open sign-in page");
                                    ui.label(RichText::new("→").color(MUTED));
                                    chip(ui, "Sign in to store");
                                    ui.label(RichText::new("→").color(MUTED));
                                    chip(ui, "Verify account");
                                    ui.label(RichText::new("→").color(MUTED));
                                    chip(ui, "Confirm session");
                                });
                            });

                        ui.add_space(12.0);
                        let login_status = self.login_status_for(source);
                        let (state_text, state_color) = match login_status {
                            LoginStatus::Ready => ("READY", GREEN),
                            LoginStatus::NeedsRecheck => ("NEEDS RECHECK", ROSE),
                            LoginStatus::Unknown => ("UNKNOWN", RED),
                        };
                        let current_confirmed = self.login_current_for(source).cloned();
                        let previous_confirmed = self.login_previous_for(source).cloned();
                        Frame::new()
                            .fill(Color32::WHITE)
                            .stroke(Stroke::new(1.0_f32, BORDER))
                            .corner_radius(10)
                            .inner_margin(Margin::symmetric(12, 10))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.vertical(|ui| {
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                RichText::new(format!("{} session status", source.region_label()))
                                                    .size(11.0)
                                                    .strong()
                                                    .color(INK),
                                            );
                                            ui.label(
                                                RichText::new(state_text)
                                                    .size(10.5)
                                                    .strong()
                                                    .color(state_color),
                                            );
                                        });
                                        match login_status {
                                            LoginStatus::Ready => {
                                                if let Some(confirmed_at) = current_confirmed.as_ref() {
                                                    ui.label(
                                                        RichText::new(format!(
                                                            "{} · Confirmed this session at {}",
                                                            self.settings.browser.label(),
                                                            confirmed_at.format("%Y-%m-%d %H:%M:%S")
                                                        ))
                                                        .size(10.2)
                                                        .color(MUTED),
                                                    );
                                                }
                                            }
                                            LoginStatus::NeedsRecheck => {
                                                if let Some(confirmed_at) = previous_confirmed.as_ref() {
                                                    ui.label(
                                                        RichText::new(format!(
                                                            "Last confirmed {}. Open this store account page to verify the session.",
                                                            confirmed_at.format("%Y-%m-%d %H:%M:%S")
                                                        ))
                                                        .size(10.2)
                                                        .color(MUTED),
                                                    );
                                                }
                                            }
                                            LoginStatus::Unknown => {
                                                ui.label(RichText::new("No sign-in confirmation for this store yet.").size(10.2).color(MUTED));
                                            }
                                        }
                                    });

                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        if login_status == LoginStatus::Ready {
                                            if ui
                                                .add(
                                                    egui::Button::new(RichText::new("Clear sign-in confirmation").color(RED))
                                                        .fill(Color32::WHITE)
                                                        .stroke(Stroke::new(1.0_f32, RED))
                                                        .corner_radius(9),
                                                )
                                                .clicked()
                                            {
                                                self.clear_login_for(source);
                                                self.disarm_all();
                                                self.persist();
                                                self.push_purchase_log(format!(
                                                    "Cleared {} sign-in confirmation; the browser remains signed in",
                                                    source.region_label()
                                                ));
                                            }
                                        } else {
                                            let label = if login_status == LoginStatus::NeedsRecheck {
                                                "Confirm still signed in"
                                            } else {
                                                "I am signed in to this store"
                                            };
                                            if ui.add(primary_button(label)).clicked() {
                                                self.confirm_login_for(source);
                                                self.persist();
                                                self.push_purchase_log(format!(
                                                    "User confirmed: {} is signed in to {}",
                                                    self.settings.browser.label(),
                                                    source.store_label()
                                                ));
                                                self.toast = Some((format!(
                                                    "{} sign-in confirmed; targets can now be armed",
                                                    source.region_label()
                                                ), 0.0));
                                            }
                                        }
                                    });
                                });
                            });

                        ui.add_space(8.0);
                        ui.label(
                            RichText::new("Sign-in is confirmed manually for 30 minutes. The app cannot detect browser sign-out. Store confirmations are separate.")
                                .size(10.0)
                                .color(MUTED),
                        );
                    });

                ui.add_space(14.0);
                Frame::new()
                    .fill(PANEL)
                    .stroke(Stroke::new(1.0_f32, BORDER))
                    .corner_radius(14)
                    .inner_margin(Margin::same(18))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label(RichText::new("2 · ARMED purchase targets").size(15.0).strong().color(INK));
                                ui.label(
                                    RichText::new(format!(
                                        "ARMED targets wait {} seconds between check rounds; the catalog uses its own schedule.",
                                        self.settings.armed_poll_seconds
                                    ))
                                    .size(11.0)
                                    .color(MUTED),
                                );
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let region_armed = self.armed_ids.iter().any(|id| {
                                    self.watch_items.iter().any(|item| item.id == *id && item.source == source)
                                });
                                if region_armed && soft_button(ui, "Disarm all").clicked()
                                {
                                    self.disarm_all();
                                }
                            });
                        });

                        ui.add_space(12.0);
                        let armed_ids = self
                            .armed_ids
                            .iter()
                            .copied()
                            .filter(|id| self.watch_items.iter().any(|item| item.id == *id && item.source == source))
                            .collect::<Vec<_>>();
                        if armed_ids.is_empty() {
                            ui.label(
                                RichText::new("No ARMED targets. Set color, size, and price rules in Watchlist, then click Arm target.")
                                    .size(11.5)
                                    .color(MUTED),
                            );
                        } else {
                            let mut disarm_id = None;
                            for id in armed_ids {
                                let Some(item) = self.watch_items.iter().find(|item| item.id == id) else {
                                    continue;
                                };
                                let title = item.runtime.title.as_deref().unwrap_or(&item.input);
                                let color_target = if item.variant_filter.trim().is_empty() {
                                    "Any color".to_owned()
                                } else {
                                    item.variant_filter.clone()
                                };
                                let size_target = if item.purchase.size_priority.trim().is_empty() {
                                    "Any size".to_owned()
                                } else {
                                    item.purchase.size_priority.clone()
                                };
                                let target = format!("{color_target} · {size_target}");
                                let price = if item.purchase.max_price_yen == 0 {
                                    "No price limit".to_owned()
                                } else {
                                    format!("≤ ¥{}", group_plain_digits(item.purchase.max_price_yen))
                                };
                                let buyable = Self::resolve_purchase_variant(item);

                                Frame::new()
                                    .fill(Color32::from_rgb(252, 247, 249))
                                    .stroke(Stroke::new(1.0_f32, BORDER))
                                    .corner_radius(10)
                                    .inner_margin(Margin::symmetric(12, 10))
                                    .show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.label(RichText::new("●").color(ROSE));
                                            ui.vertical(|ui| {
                                                ui.label(RichText::new(title).size(12.0).strong().color(INK));
                                                ui.label(
                                                    RichText::new(format!("{target} · {price}"))
                                                        .size(10.5)
                                                        .color(MUTED),
                                                );
                                                if let Some(variant) = &buyable {
                                                    ui.label(
                                                        RichText::new(format!(
                                                            "Available now: {} · {}",
                                                            variant.display_label(),
                                                            shopify::format_jpy(variant.price_minor)
                                                        ))
                                                        .size(10.5)
                                                        .color(GREEN),
                                                    );
                                                } else {
                                                    ui.label(RichText::new("Waiting for stock that matches your rules").size(10.5).color(MUTED));
                                                }
                                            });
                                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                                if soft_button(ui, "Disarm").clicked() {
                                                    disarm_id = Some(id);
                                                }
                                            });
                                        });
                                    });
                                ui.add_space(7.0);
                            }
                            if let Some(id) = disarm_id {
                                self.disarm_target(id);
                            }
                        }
                    });

                ui.add_space(14.0);
                Frame::new()
                    .fill(PANEL)
                    .stroke(Stroke::new(1.0_f32, BORDER))
                    .corner_radius(14)
                    .inner_margin(Margin::same(18))
                    .show(ui, |ui| {
                        ui.label(RichText::new("3 · Activity log").size(15.0).strong().color(INK));
                        ui.label(
                            RichText::new("Logs contain app actions, without OTPs, cookies, or payment details.")
                                .size(10.5)
                                .color(MUTED),
                        );
                        ui.add_space(9.0);
                        if self.purchase_log.is_empty() {
                            ui.label(RichText::new("No activity yet").size(11.0).color(MUTED));
                        } else {
                            for entry in self.purchase_log.iter().take(20) {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(entry.at.format("%H:%M:%S").to_string())
                                            .size(10.0)
                                            .monospace()
                                            .color(MUTED),
                                    );
                                    ui.label(RichText::new(&entry.text).size(10.8).color(INK));
                                });
                            }
                        }
                    });

                ui.add_space(10.0);
                ui.label(
                    RichText::new("ARMED matches open this store checkout. Complete the final order yourself. Store sign-in and security checks still apply.")
                        .size(10.5)
                        .color(MUTED),
                );
            });
    }

    fn auto_checkout_allowed(&self, id: u64) -> bool {
        let Some(item) = self.watch_items.iter().find(|item| item.id == id) else {
            return false;
        };
        if !item.enabled
            || self.active_region != Some(item.source)
            || item.runtime.last_error.is_some()
        {
            return false;
        }
        let Some(checked) = item.runtime.last_checked else {
            return false;
        };
        let age = Local::now().signed_duration_since(checked).num_seconds();
        if !(0..=15).contains(&age) {
            return false;
        }
        if let Some(rule_id) = self.release_bound.get(&id) {
            let Some(slot) = self.settings.release_schedule.sale_slot(Utc::now()) else {
                return false;
            };
            return self.release_armed.contains(rule_id)
                && !self.release_completed.contains(rule_id)
                && checked.timestamp() >= slot;
        }
        true
    }

    fn disarm_release(&mut self) {
        for id in self.release_bound.keys() {
            self.armed_ids.remove(id);
            self.checkout_gate.reset(*id);
        }
        self.release_bound.clear();
        self.release_armed.clear();
        self.release_completed.clear();
        self.release_slot = None;
        self.release_run = None;
        self.sync_armed_monitor();
    }

    fn disarm_release_rule(&mut self, rule_id: u64) {
        self.release_armed.remove(&rule_id);
        let ids: Vec<_> = self
            .release_bound
            .iter()
            .filter(|(_, rule)| **rule == rule_id)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.release_bound.remove(&id);
            self.armed_ids.remove(&id);
            self.checkout_gate.reset(id);
        }
        self.release_completed.remove(&rule_id);
        self.release_status
            .insert(rule_id, "Disarmed; arm again after editing".into());
        self.sync_armed_monitor();
    }

    fn tick_release(&mut self) {
        let now = Utc::now();
        let slot = self.settings.release_schedule.sale_slot(now);
        let expired = self.release_run.is_some_and(|opening| {
            now.timestamp()
                >= opening + i64::from(self.settings.release_schedule.window_minutes) * 60
        });
        if expired
            || (self.release_slot.is_some()
                && self.release_slot != slot
                && !self.release_armed.is_empty())
        {
            self.disarm_release();
            self.push_purchase_log(
                "Release window ended; release rules disarmed. Arm again for the next release.",
            );
        }
        self.release_slot = slot;
        let desired = if self.active_region == Some(StoreSource::JapanOnlineStore)
            && !self.release_armed.is_empty()
        {
            Some(self.settings.release_schedule.clone())
        } else {
            None
        };
        if self.sent_schedule.as_ref() != Some(&desired) {
            let _ = self
                .command_tx
                .send(MonitorCommand::SetReleaseSchedule(desired.clone()));
            self.sent_schedule = Some(desired);
        }
        let Some(slot) = slot else {
            return;
        };
        let Some(source) = self.active_region else {
            return;
        };
        if source != StoreSource::JapanOnlineStore
            || !self.login_ready_for(source)
            || self.catalog_error.is_some()
        {
            return;
        }
        let Some(checked) = self.catalog_last_checked else {
            return;
        };
        if checked.timestamp() < slot || now.signed_duration_since(checked).num_seconds() > 120 {
            return;
        }
        let rules: Vec<_> = self
            .release_rules
            .iter()
            .filter(|r| {
                self.release_armed.contains(&r.id)
                    && !self.release_completed.contains(&r.id)
                    && !self.release_bound.values().any(|id| *id == r.id)
            })
            .cloned()
            .collect();
        for rule in rules {
            let matches: Vec<_> = self
                .catalog
                .iter()
                .filter(|p| rule.matches(source, p))
                .cloned()
                .collect();
            if matches.len() != 1 {
                self.release_status.insert(
                    rule.id,
                    if matches.is_empty() {
                        "Waiting for a matching product".into()
                    } else {
                        format!(
                            "Matched {} products. Narrow the keywords or use a specific URL / product code.",
                            matches.len()
                        )
                    },
                );
                continue;
            }
            let product = &matches[0];
            let existing = self
                .watch_items
                .iter()
                .position(|w| w.source == source && w.product_url == product.product_url);
            let index = if let Some(index) = existing {
                if self.armed_ids.contains(&self.watch_items[index].id)
                    || self.release_bound.contains_key(&self.watch_items[index].id)
                {
                    self.release_status.insert(
                        rule.id,
                        "This product has another ARMED target. Disarm the conflicting target first.".into(),
                    );
                    continue;
                }
                index
            } else {
                let item = WatchItem::new(
                    self.next_id,
                    source,
                    rule.product_hint.clone(),
                    product.product_url.clone(),
                    rule.color_priority.clone(),
                );
                self.next_id += 1;
                self.watch_items.push(item);
                self.watch_items.len() - 1
            };
            let item = &mut self.watch_items[index];
            item.enabled = true;
            item.variant_filter = rule.color_priority.clone();
            item.purchase = rule.purchase.clone();
            item.runtime.selected_variant = None;
            item.runtime.last_checked = None;
            let id = item.id;
            self.release_bound.insert(id, rule.id);
            self.checkout_gate.reset(id);
            self.armed_ids.insert(id);
            self.release_status.insert(
                rule.id,
                format!("Matched {}; checking live stock", product.title),
            );
            self.push_purchase_log(format!(
                "Friday release match: {} → live stock check",
                product.title
            ));
            self.persist();
            self.sync_monitor();
            let _ = self.command_tx.send(MonitorCommand::CheckNow(Some(id)));
        }
    }

    fn release_page(&mut self, ui: &mut egui::Ui) {
        let Some(source) = self.active_region else {
            return;
        };
        if source != StoreSource::JapanOnlineStore {
            ui.heading("Friday releases · Japan official store");
            ui.label("This schedule follows the Japan reservation guide. Use product watches for the China channel.");
            return;
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.heading("Friday releases · Japan time (JST)");
            ui.label("Prepare a title, product code, or URL. At opening, match one product, check live variants, and open checkout.");
            ui.hyperlink_to("Official reservation guide: designated Friday 12:00 to Monday 12:00; check each announcement", "https://store.babyssb.co.jp/pages/guide_reserve");
            ui.add_space(12.0);
            let old = self.settings.release_schedule.clone();
            Frame::new().fill(PANEL).stroke(Stroke::new(1.0_f32, BORDER)).corner_radius(12).inner_margin(Margin::same(16)).show(ui, |ui| {
                let schedule = &mut self.settings.release_schedule;
                ui.checkbox(&mut schedule.enabled, "Enable Friday release schedule");
                ui.horizontal_wrapped(|ui| {
                    ui.label("Every Friday");
                    ui.add(egui::DragValue::new(&mut schedule.hour).range(0..=23).suffix("h"));
                    ui.add(egui::DragValue::new(&mut schedule.minute).range(0..=59).suffix("m JST"));
                    ui.label("Prewarm");
                    ui.add(egui::DragValue::new(&mut schedule.prewarm_minutes).range(0..=60).suffix("min"));
                    ui.label("Sale window");
                    ui.add(egui::DragValue::new(&mut schedule.window_minutes).range(1..=120).suffix("min"));
                });
                ui.horizontal(|ui| {
                    ui.label("Catalog polling during window");
                    ui.add(egui::DragValue::new(&mut schedule.poll_seconds).range(5..=60).suffix("s"));
                });
                let now = Utc::now();
                let text = if let Some(opening) = schedule.active_slot(now) {
                    if now < opening { format!("Prewarming · Opens {} JST", opening.format("%m-%d %H:%M")) }
                    else { "Release window active · Running manually armed rules".into() }
                } else if let Some(opening) = schedule.next_opening(now) {
                    let delta = opening.signed_duration_since(now);
                    format!("Next: {} JST · In {}d {}h {}m", opening.format("%Y-%m-%d %H:%M"), delta.num_days(), delta.num_hours() % 24, delta.num_minutes() % 60)
                } else { "Invalid schedule".into() };
                ui.label(RichText::new(text).color(ROSE));
                ui.label("Prewarm refreshes the catalog only. Keep the app running and computer awake. Confirm browser sign-in within 30 minutes of opening.");
                ui.label("Restarting, switching stores or browsers, editing rules, or ending the window disarms the affected rules. Arm before each release.");
            });
            if old != self.settings.release_schedule {
                self.disarm_release();
                self.persist();
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if soft_button(ui, "+ Add release target").clicked() {
                    let id = self.release_rules.iter().map(|r| r.id).max().unwrap_or(0) + 1;
                    self.release_rules.push(ReleaseRule::new(id, source));
                    self.persist();
                }
                if soft_button(ui, "Sign-in and purchase setup →").clicked() { self.page = Page::Purchase; }
                if soft_button(ui, "Disarm all release rules").clicked() { self.disarm_release(); }
            });
            if self.release_rules.is_empty() { ui.label("Add a target and enter a specific title, product code, or URL. Separate title terms with |; all terms must match."); }
            let mut remove = None;
            for index in 0..self.release_rules.len() {
                if self.release_rules[index].source != source { continue; }
                let id = self.release_rules[index].id;
                let armed = self.release_armed.contains(&id);
                let login_ready = self.login_ready_for(source);
                let schedule_ready = self.settings.release_schedule.enabled && self.settings.release_schedule.validate().is_ok();
                let mut changed = false;
                let mut toggle = false;
                ui.add_space(10.0);
                Frame::new().fill(PANEL).stroke(Stroke::new(1.0_f32, BORDER)).corner_radius(12).inner_margin(Margin::same(16)).show(ui, |ui| {
                    let rule = &mut self.release_rules[index];
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("Target #{id} · {}", if armed { "ARMED" } else { "Not armed" })).strong().color(if armed { ROSE } else { MUTED }));
                        if ui.small_button("Delete").clicked() { remove = Some(id); }
                    });
                    ui.label("Product URL / code / title terms (| means match all terms)");
                    changed |= ui.add(egui::TextEdit::singleline(&mut rule.product_hint).desired_width(f32::INFINITY).hint_text("Example: Series name | Jumper Skirt, or a specific product URL")).changed();
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Color priority");
                        changed |= ui.add(egui::TextEdit::singleline(&mut rule.color_priority).desired_width(200.0).hint_text("Pink | Black; blank for any")).changed();
                        ui.label("Size priority");
                        changed |= ui.add(egui::TextEdit::singleline(&mut rule.purchase.size_priority).desired_width(120.0).hint_text("M | S")).changed();
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Maximum JPY price (0 = no limit)");
                        changed |= ui.add(egui::DragValue::new(&mut rule.purchase.max_price_yen).range(0..=1_000_000)).changed();
                        ui.label("Quantity");
                        changed |= ui.add(egui::DragValue::new(&mut rule.purchase.quantity).range(1..=3)).changed();
                        changed |= ui.checkbox(&mut rule.purchase.auto_open_checkout, "Open checkout automatically on match").changed();
                    });
                    let count = self.catalog.iter().filter(|p| rule.matches(source, p)).count();
                    ui.label(format!("{count} catalog matches · Only a unique match can run"));
                    if let Some(status) = self.release_status.get(&id) { ui.label(RichText::new(status).color(ROSE)); }
                    if !login_ready { ui.label("Confirm Japan store sign-in in Purchase setup first (valid for 30 minutes)."); }
                    if !schedule_ready { ui.label("Enable and configure the release schedule first."); }
                    let valid_hint = rule.product_hint.split('|').any(|term| !term.trim().is_empty());
                    if ui.add_enabled(armed || (login_ready && schedule_ready && valid_hint), primary_button(if armed { "Disarm this release" } else { "Arm this release" })).clicked() { toggle = true; }
                });
                if changed {
                    self.disarm_release_rule(id);
                    self.persist();
                } else if toggle {
                    if armed { self.disarm_release_rule(id); }
                    else {
                        self.release_completed.remove(&id);
                        self.release_armed.insert(id);
                        let now = Utc::now();
                        let schedule = &self.settings.release_schedule;
                        self.release_run = schedule.active_slot(now).or_else(|| schedule.next_opening(now)).map(|opening| opening.timestamp());
                        self.release_status.insert(id, "Armed; waiting for the release window and a unique product match".into());
                        self.push_purchase_log(format!("Friday release target #{id} armed"));
                    }
                }
            }
            if let Some(id) = remove {
                self.disarm_release_rule(id);
                self.release_rules.retain(|rule| rule.id != id);
                self.release_status.remove(&id);
                self.persist();
            }
        });
    }

    fn region_gate(&mut self, ctx: &egui::Context) {
        let mut chosen = None;
        egui::CentralPanel::default()
            .frame(Frame::new().fill(BG).inner_margin(Margin::same(32)))
            .show(ctx, |ui| {
                ui.add_space(32.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new("BABY WATCH")
                            .font(FontId::proportional(30.0))
                            .strong()
                            .color(INK),
                    );
                    ui.label(
                        RichText::new("Choose a store · Catalog, stock, account session, and checkout are scoped to your selection")
                            .size(12.5)
                            .color(MUTED),
                    );
                });
                ui.add_space(26.0);

                ui.columns(2, |columns| {
                    Frame::new()
                        .fill(PANEL)
                        .stroke(Stroke::new(1.0_f32, BORDER))
                        .corner_radius(16)
                        .inner_margin(Margin::same(22))
                        .show(&mut columns[0], |ui| {
                            ui.label(RichText::new("JP  Japan").size(11.0).strong().color(ROSE));
                            ui.add_space(5.0);
                            ui.label(
                                RichText::new("BABY Japan Official Online Store")
                                    .size(19.0)
                                    .strong()
                                    .color(INK),
                            );
                            ui.label(RichText::new("store.babyssb.co.jp").size(11.0).monospace().color(MUTED));
                            ui.add_space(12.0);
                            ui.label(RichText::new("• Japan official Shopify catalog and stock").size(11.5).color(INK));
                            ui.label(RichText::new("• Separate Japan account and browser session").size(11.5).color(INK));
                            ui.label(RichText::new("• JPY prices and Japan store checkout").size(11.5).color(INK));
                            ui.add_space(18.0);
                            if ui
                                .add_sized(
                                    [ui.available_width(), 42.0],
                                    primary_button("Monitor Japan store →"),
                                )
                                .clicked()
                            {
                                chosen = Some(StoreSource::JapanOnlineStore);
                            }
                        });

                    Frame::new()
                        .fill(PANEL)
                        .stroke(Stroke::new(1.0_f32, BORDER))
                        .corner_radius(16)
                        .inner_margin(Margin::same(22))
                        .show(&mut columns[1], |ui| {
                            ui.label(RichText::new("CN  Mainland China").size(11.0).strong().color(ROSE));
                            ui.add_space(5.0);
                            ui.label(
                                RichText::new("LaceRococo · China Partner Store")
                                    .size(19.0)
                                    .strong()
                                    .color(INK),
                            );
                            ui.label(RichText::new("www.lacerococo.com").size(11.0).monospace().color(MUTED));
                            ui.add_space(12.0);
                            ui.label(RichText::new("• China-facing store operated by Yakumo").size(11.5).color(INK));
                            ui.label(RichText::new("• Separate Shopify customer account and session").size(11.5).color(INK));
                            ui.label(RichText::new("• BABY / A/P products from this store only").size(11.5).color(INK));
                            ui.add_space(18.0);
                            if ui
                                .add_sized(
                                    [ui.available_width(), 42.0],
                                    primary_button("Monitor China partner store →"),
                                )
                                .clicked()
                            {
                                chosen = Some(StoreSource::ChinaLaceRococo);
                            }
                        });
                });

                ui.add_space(18.0);
                Frame::new()
                    .fill(Color32::from_rgb(255, 248, 249))
                    .stroke(Stroke::new(1.0_f32, Color32::from_rgb(236, 206, 210)))
                    .corner_radius(11)
                    .inner_margin(Margin::symmetric(14, 11))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new("Store separation")
                                .size(11.0)
                                .strong()
                                .color(RED),
                        );
                        ui.label(
                            RichText::new("LaceRococo is a separate Yakumo partner store with its own account session. BABY has identified babyzg.is / babyssb.is as impersonation sites; this app does not connect to them.")
                                .size(10.7)
                                .color(MUTED),
                        );
                    });
            });

        if let Some(source) = chosen {
            self.select_region(source);
        }
    }

    fn toast(&mut self, ctx: &egui::Context) {
        let Some((message, mut started)) = self.toast.clone() else {
            return;
        };
        if started == 0.0 {
            started = ctx.input(|i| i.time);
            self.toast = Some((message.clone(), started));
        }
        let age = ctx.input(|i| i.time) - started;
        if age > 4.5 {
            self.toast = None;
            return;
        }

        egui::Area::new("toast".into())
            .anchor(egui::Align2::RIGHT_BOTTOM, Vec2::new(-24.0, -24.0))
            .show(ctx, |ui| {
                Frame::new()
                    .fill(Color32::from_rgb(49, 45, 47))
                    .corner_radius(11)
                    .inner_margin(Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.label(RichText::new(message).size(12.5).color(Color32::WHITE));
                    });
            });
    }
}

impl eframe::App for BabyWatchApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_events(ctx);
        self.tick_release();
        self.image_cache.process_ready(ctx);

        if self.active_region.is_none() {
            self.region_gate(ctx);
            self.toast(ctx);
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
            return;
        }

        egui::TopBottomPanel::top("top")
            .frame(
                Frame::new()
                    .fill(PANEL)
                    .inner_margin(Margin::symmetric(22, 13))
                    .stroke(Stroke::new(1.0_f32, BORDER)),
            )
            .show(ctx, |ui| self.top_bar(ui));

        egui::SidePanel::left("side")
            .resizable(false)
            .exact_width(215.0)
            .frame(
                Frame::new()
                    .fill(Color32::from_rgb(244, 238, 241))
                    .inner_margin(Margin::same(18)),
            )
            .show(ctx, |ui| self.sidebar(ui));

        egui::CentralPanel::default()
            .frame(Frame::new().fill(BG).inner_margin(Margin::same(22)))
            .show(ctx, |ui| match self.page {
                Page::Catalog => self.catalog_page(ui),
                Page::Watchlist => self.watchlist_page(ui),
                Page::Purchase => self.purchase_page(ui),
                Page::Release => self.release_page(ui),
            });

        self.toast(ctx);
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.persist();
        let _ = self.command_tx.send(MonitorCommand::Stop);
    }
}

fn configure_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.visuals = egui::Visuals::light();
    style.visuals.panel_fill = BG;
    style.visuals.window_fill = PANEL;
    style.visuals.extreme_bg_color = Color32::WHITE;
    style.visuals.widgets.inactive.bg_fill = Color32::WHITE;
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(250, 239, 244);
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, ROSE);
    style.visuals.widgets.active.bg_fill = ROSE_SOFT;
    style.visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, ROSE);
    style.spacing.item_spacing = Vec2::new(8.0, 7.0);
    style.spacing.button_padding = Vec2::new(10.0, 7.0);
    ctx.set_style(style);
}

fn primary_button(text: &str) -> egui::Button<'_> {
    egui::Button::new(RichText::new(text).color(Color32::WHITE).strong())
        .fill(ROSE)
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(9))
        .min_size(Vec2::new(88.0, 34.0))
}

fn soft_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(text).color(INK))
            .fill(Color32::WHITE)
            .stroke(Stroke::new(1.0_f32, BORDER))
            .corner_radius(9),
    )
}

fn nav_button(ui: &mut egui::Ui, text: &str, selected: bool) -> egui::Response {
    ui.add_sized(
        [ui.available_width(), 36.0],
        egui::Button::new(
            RichText::new(text)
                .color(if selected { ROSE } else { INK })
                .strong(),
        )
        .fill(if selected {
            ROSE_SOFT
        } else {
            Color32::TRANSPARENT
        })
        .stroke(Stroke::NONE)
        .corner_radius(9),
    )
}

fn chip(ui: &mut egui::Ui, text: &str) {
    Frame::new()
        .fill(Color32::from_rgb(246, 240, 243))
        .corner_radius(7)
        .inner_margin(Margin::symmetric(7, 4))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(10.5).color(MUTED));
        });
}

fn color_stock_chip(ui: &mut egui::Ui, color_name: &str, available: bool) {
    let text = if available {
        format!("● {color_name}")
    } else {
        format!("○ {color_name}")
    };
    Frame::new()
        .fill(if available {
            Color32::from_rgb(239, 248, 244)
        } else {
            Color32::from_rgb(247, 243, 245)
        })
        .stroke(Stroke::new(
            1.0_f32,
            if available {
                Color32::from_rgb(190, 224, 208)
            } else {
                BORDER
            },
        ))
        .corner_radius(7)
        .inner_margin(Margin::symmetric(7, 4))
        .show(ui, |ui| {
            ui.label(
                RichText::new(text)
                    .size(10.5)
                    .color(if available { GREEN } else { MUTED }),
            );
        });
}

fn product_thumbnail(ui: &mut egui::Ui, texture: Option<&egui::TextureHandle>, edge: f32) {
    Frame::new()
        .fill(Color32::WHITE)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .corner_radius(10)
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            ui.allocate_ui_with_layout(
                Vec2::splat(edge),
                Layout::centered_and_justified(egui::Direction::LeftToRight),
                |ui| {
                    if let Some(texture) = texture {
                        let source = texture.size_vec2();
                        let scale = if source.x > 0.0 && source.y > 0.0 {
                            (edge / source.x).min(edge / source.y)
                        } else {
                            1.0
                        };
                        let size = source * scale;
                        ui.add(egui::Image::new((texture.id(), size)));
                    } else {
                        ui.label(RichText::new("♡").size(24.0).color(ROSE_SOFT));
                    }
                },
            );
        });
}

fn group_plain_digits(value: u64) -> String {
    let raw = value.to_string();
    let mut reversed = String::with_capacity(raw.len() + raw.len() / 3);
    for (index, ch) in raw.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            reversed.push(',');
        }
        reversed.push(ch);
    }
    reversed.chars().rev().collect()
}

fn stat(ui: &mut egui::Ui, label: &str, value: String, hint: &str) {
    Frame::new()
        .fill(Color32::from_rgb(250, 246, 248))
        .stroke(Stroke::new(1.0_f32, BORDER))
        .corner_radius(10)
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new(label).size(10.5).color(MUTED));
                    ui.label(RichText::new(value).size(18.0).strong().color(INK));
                });
                ui.with_layout(Layout::right_to_left(Align::BOTTOM), |ui| {
                    ui.label(RichText::new(hint).size(9.5).color(MUTED));
                });
            });
        });
    ui.add_space(7.0);
}
