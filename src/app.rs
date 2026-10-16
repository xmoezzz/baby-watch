use std::collections::{HashMap, HashSet, VecDeque};

use chrono::{DateTime, Local, Utc};
use crossbeam_channel::{Receiver, Sender};
use eframe::egui::{
    self, Color32, CornerRadius, FontId, Frame, Layout, Margin, RichText, Stroke, Vec2,
};

use crate::{
    browser,
    image_cache::ImageCache,
    model::{CatalogProduct, PurchaseRule, StoreSource, VariantState, WatchItem},
    monitor::{CatalogChange, MonitorCommand, MonitorEvent},
    release::{self, CheckoutGate, ReleaseRule, ReleaseSchedule},
    shopify,
    storage::{self, BrowserChoice, PersistedState, Settings},
};

const BG: Color32 = Color32::from_rgb(247, 245, 243);
const PANEL: Color32 = Color32::WHITE;
const INK: Color32 = Color32::from_rgb(55, 46, 58);
const MUTED: Color32 = Color32::from_rgb(132, 123, 134);
const ROSE: Color32 = Color32::from_rgb(151, 100, 125);
const ROSE_SOFT: Color32 = Color32::from_rgb(243, 232, 237);
const GREEN: Color32 = Color32::from_rgb(56, 139, 101);
const RED: Color32 = Color32::from_rgb(181, 75, 82);
const BORDER: Color32 = Color32::from_rgb(232, 226, 230);

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
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("BABY Watch").size(22.0).strong().color(INK));
            ui.add_space(12.0);
            chip(ui, source.region_label());
            ui.label(RichText::new(source.host()).size(12.0).color(MUTED));
            if self.catalog_loading {
                ui.spinner();
            }
        });
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        let source = self.active_region.expect("store selected");
        egui::ScrollArea::vertical()
            .id_salt("sidebar_scroll")
            .show(ui, |ui| {
                eyebrow(ui, "YOUR COLLECTION");
                ui.add_space(12.0);
                for (page, label) in [
                    (Page::Catalog, "Catalog"),
                    (Page::Watchlist, "Watchlist"),
                    (Page::Release, "Friday releases"),
                    (Page::Purchase, "Purchase setup"),
                ] {
                    if nav_button(ui, label, self.page == page).clicked() {
                        self.page = page;
                    }
                    ui.add_space(4.0);
                }
                ui.add_space(26.0);
                eyebrow(ui, "AT A GLANCE");
                ui.add_space(10.0);
                let watched = self
                    .watch_items
                    .iter()
                    .filter(|w| w.source == source)
                    .count();
                let armed = self
                    .armed_ids
                    .iter()
                    .filter(|id| {
                        self.watch_items
                            .iter()
                            .any(|w| w.id == **id && w.source == source)
                    })
                    .count();
                sidebar_count(ui, "Products", self.catalog.len());
                sidebar_count(ui, "Watching", watched);
                sidebar_count(ui, "Armed", armed);
                ui.add_space(22.0);
                let (label, color) = match self.login_status() {
                    LoginStatus::Ready => ("Session confirmed", GREEN),
                    LoginStatus::NeedsRecheck => ("Check your session", ROSE),
                    LoginStatus::Unknown => ("Sign-in needed", MUTED),
                };
                ui.label(RichText::new(format!("● {label}")).size(12.0).color(color));
                ui.add_space(22.0);
                ui.collapsing("Preferences", |ui| {
                    let old = self.settings.clone();
                    ui.checkbox(&mut self.settings.catalog_monitor, "Monitor catalog");
                    ui.checkbox(&mut self.settings.notifications, "Notifications");
                    small_label(ui, "Catalog interval");
                    ui.add(
                        egui::Slider::new(&mut self.settings.poll_seconds, 30..=300).suffix(" s"),
                    );
                    small_label(ui, "Armed interval");
                    ui.add(
                        egui::Slider::new(&mut self.settings.armed_poll_seconds, 5..=30)
                            .suffix(" s"),
                    );
                    if old.catalog_monitor != self.settings.catalog_monitor
                        || old.notifications != self.settings.notifications
                        || old.poll_seconds != self.settings.poll_seconds
                        || old.armed_poll_seconds != self.settings.armed_poll_seconds
                    {
                        self.persist();
                        self.sync_monitor();
                    }
                });
                ui.add_space(18.0);
                if soft_button(ui, "Open store").clicked() {
                    let _ =
                        browser::open_url(self.settings.browser, &shopify::storefront_url(source));
                }
                if soft_button(ui, "Switch store").clicked() {
                    self.disarm_all();
                    self.active_region = None;
                    self.catalog.clear();
                    self.catalog_loading = false;
                    let _ = self
                        .command_tx
                        .send(MonitorCommand::SetCatalogEnabled(false));
                }
                ui.add_space(24.0);
                small_label(ui, "BABY Watch  /  v0.7.1");
            });
    }

    fn catalog_page(&mut self, ui: &mut egui::Ui) {
        section_title(
            ui,
            "The collection",
            "Browse pieces, follow your favorites, and watch for a restock.",
        );
        card().show(ui, |ui| {
            form_field(
                ui,
                "Search the collection",
                &mut self.catalog_search,
                "Product name, code, color, or SKU",
            );
            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                for (brand, label) in [
                    (BrandFilter::Baby, "BABY"),
                    (BrandFilter::Pirates, "ALICE and the PIRATES"),
                    (BrandFilter::All, "All brands"),
                ] {
                    if ui
                        .selectable_label(self.catalog_brand == brand, label)
                        .clicked()
                    {
                        self.catalog_brand = brand;
                    }
                }
                ui.checkbox(&mut self.catalog_stock_only, "In stock only");
                if soft_button(ui, "Refresh").clicked() {
                    let _ = self.command_tx.send(MonitorCommand::RefreshCatalog);
                }
            });
        });
        ui.add_space(16.0);
        if let Some(error) = &self.catalog_error {
            notice(ui, error, RED);
        }
        let indices = self.filtered_catalog_indices();
        ui.horizontal_wrapped(|ui| {
            small_label(ui, &format!("{} pieces", indices.len()));
            if let Some(at) = self.catalog_last_checked {
                small_label(ui, &format!("Updated {}", at.format("%H:%M:%S")));
            }
        });
        ui.add_space(12.0);
        if self.catalog.is_empty() {
            empty_state(
                ui,
                if self.catalog_loading {
                    "Curating your collection…"
                } else {
                    "Your collection will appear here"
                },
                "Refresh the catalog to load products from the selected store.",
            );
            return;
        }
        if indices.is_empty() {
            empty_state(
                ui,
                "No pieces match this search",
                "Try a different name, product code, or brand filter.",
            );
            return;
        }
        // Rows use their actual content height. No fixed-height virtualization
        // or inherited column justification can make neighboring cards overlap.
        egui::ScrollArea::vertical()
            .id_salt("catalog_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                for index in indices {
                    self.catalog_row(ui, index);
                    ui.add_space(14.0);
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
        let visible = ui.is_rect_visible(egui::Rect::from_min_size(
            ui.next_widget_position(),
            Vec2::new(ui.available_width(), 270.0),
        ));
        let thumbnail = if visible {
            product
                .image_url
                .as_deref()
                .and_then(|url| self.image_cache.texture(url))
        } else {
            None
        };
        let source = self.active_region.expect("store selected");
        let watched = self
            .watch_items
            .iter()
            .any(|w| w.source == source && w.product_url == product.product_url);
        card().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            let width = ui.available_width();
            ui.horizontal_top(|ui| {
                product_thumbnail(ui, thumbnail.as_ref(), 94.0);
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.set_width((width - 120.0).max(100.0));
                    ui.add(
                        egui::Label::new(
                            RichText::new(&product.title).size(15.0).strong().color(INK),
                        )
                        .wrap(),
                    );
                    ui.add_space(6.0);
                    small_label(
                        ui,
                        product.product_code.as_deref().unwrap_or(&product.vendor),
                    );
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(
                            product
                                .min_price_minor()
                                .map(shopify::format_jpy)
                                .unwrap_or_else(|| "—".into()),
                        )
                        .size(17.0)
                        .color(ROSE),
                    );
                });
            });
            ui.add_space(12.0);
            ui.horizontal_wrapped(|ui| {
                let colors = product.colors();
                if colors.is_empty() {
                    chip(
                        ui,
                        if product.any_available() {
                            "In stock"
                        } else {
                            "Sold out"
                        },
                    );
                } else {
                    for color in colors.iter().take(6) {
                        color_stock_chip(ui, &color.name, color.available);
                    }
                    if colors.len() > 6 {
                        small_label(ui, &format!("+{} colors", colors.len() - 6));
                    }
                }
            });
            ui.add_space(12.0);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(
                        !watched,
                        primary_button(if watched { "Watching" } else { "+ Watch" }),
                    )
                    .clicked()
                {
                    self.insert_watch(
                        source,
                        product
                            .product_code
                            .clone()
                            .unwrap_or_else(|| product.title.clone()),
                        product.product_url.clone(),
                        String::new(),
                    );
                }
                if soft_button(ui, "View product").clicked() {
                    let _ = browser::open_url(self.settings.browser, &product.product_url);
                }
            });
        });
    }

    fn watchlist_page(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .id_salt("watchlist_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                section_title(
                    ui,
                    "Your watchlist",
                    "A little closer to the pieces you love.",
                );
                card().show(ui, |ui| {
                    ui.label(RichText::new("Add a piece").size(16.0).strong());
                    ui.add_space(10.0);
                    form_field(
                        ui,
                        "Product URL or code",
                        &mut self.add_input,
                        "Paste a store product link or Japan product code",
                    );
                    ui.add_space(8.0);
                    form_field(
                        ui,
                        "Color preference (optional)",
                        &mut self.add_filter,
                        "Pink | Black",
                    );
                    ui.add_space(10.0);
                    if ui.add(primary_button("Add to watchlist")).clicked() {
                        self.add_watch();
                    }
                    if let Some(error) = &self.add_error {
                        notice(ui, error, RED);
                    }
                });
                ui.add_space(16.0);
                self.watchlist(ui);
            });
    }

    fn watchlist(&mut self, ui: &mut egui::Ui) {
        let Some(source) = self.active_region else {
            return;
        };
        let ids: Vec<_> = self
            .watch_items
            .iter()
            .filter(|w| w.source == source)
            .map(|w| w.id)
            .collect();
        if ids.is_empty() {
            empty_state(
                ui,
                "Nothing on your watchlist yet",
                "Choose a piece in Catalog, or add a product above.",
            );
            return;
        }
        let mut sync = false;
        let mut delete = None;
        for id in ids {
            let Some(index) = self.watch_items.iter().position(|w| w.id == id) else {
                continue;
            };
            let armed = self.armed_ids.contains(&id);
            let mut toggle = false;
            let mut checkout = false;
            let mut changed = false;
            card().show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                let item = &mut self.watch_items[index];
                ui.add(
                    egui::Label::new(
                        RichText::new(item.runtime.title.as_deref().unwrap_or(&item.input))
                            .size(18.0)
                            .strong(),
                    )
                    .wrap(),
                );
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    chip(
                        ui,
                        if armed {
                            "Armed"
                        } else if item.runtime.checking {
                            "Checking…"
                        } else {
                            "Watching"
                        },
                    );
                    if let Some(at) = item.runtime.last_checked {
                        small_label(ui, &format!("Checked {}", at.format("%H:%M:%S")));
                    }
                    changed |= ui.checkbox(&mut item.enabled, "Enabled").changed();
                });
                ui.add_space(16.0);
                changed |= purchase_fields(ui, &mut item.variant_filter, &mut item.purchase);
                ui.add_space(12.0);
                small_label(ui, "Variant selection");
                let label = item
                    .runtime
                    .selected_variant
                    .and_then(|id| item.runtime.variants.iter().find(|v| v.id == id))
                    .map(|v| v.display_label())
                    .unwrap_or_else(|| "Automatic · follow your priorities".into());
                egui::ComboBox::from_id_salt(("variant", id))
                    .width(ui.available_width().min(480.0))
                    .wrap_mode(egui::TextWrapMode::Truncate)
                    .selected_text(label)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut item.runtime.selected_variant,
                            None,
                            "Automatic · follow your priorities",
                        );
                        for variant in &item.runtime.variants {
                            if variant.matches_filter(&item.variant_filter) {
                                ui.selectable_value(
                                    &mut item.runtime.selected_variant,
                                    Some(variant.id),
                                    format!(
                                        "{} · {}{}",
                                        variant.display_label(),
                                        shopify::format_jpy(variant.price_minor),
                                        if variant.available {
                                            ""
                                        } else {
                                            " · sold out"
                                        }
                                    ),
                                );
                            }
                        }
                    });
                if let Some(error) = &item.runtime.last_error {
                    notice(ui, error, RED);
                }
                ui.add_space(16.0);
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add(primary_button(if armed { "Disarm" } else { "Arm target" }))
                        .clicked()
                    {
                        toggle = true;
                    }
                    if ui
                        .add_enabled(
                            Self::resolve_purchase_variant(item).is_some(),
                            secondary_button("Open checkout"),
                        )
                        .clicked()
                    {
                        checkout = true;
                    }
                    if soft_button(ui, "Check now").clicked() {
                        let _ = self.command_tx.send(MonitorCommand::CheckNow(Some(id)));
                    }
                    if soft_button(ui, "View product").clicked() {
                        let _ = browser::open_url(self.settings.browser, &item.product_url);
                    }
                    if soft_button(ui, "Remove").clicked() {
                        delete = Some(id);
                    }
                });
            });
            if changed {
                sync = true;
            }
            if toggle {
                if armed {
                    self.disarm_target(id);
                } else {
                    self.arm_target(id);
                }
            }
            if checkout {
                self.launch_checkout(id, "Manual checkout", false);
            }
            ui.add_space(16.0);
        }
        if let Some(id) = delete {
            self.disarm_target(id);
            self.watch_items.retain(|w| w.id != id);
            sync = true;
        }
        if sync {
            self.persist();
            self.sync_monitor();
        }
    }

    fn purchase_page(&mut self, ui: &mut egui::Ui) {
        let Some(source) = self.active_region else {
            return;
        };
        egui::ScrollArea::vertical().id_salt("purchase_scroll").auto_shrink([false,false]).show(ui, |ui| {
            section_title(ui,"Ready when you are", "Connect your browser session before arming a piece.");
            card().show(ui, |ui| {
                eyebrow(ui,"01 / YOUR ACCOUNT");
                ui.add_space(10.0);
                ui.label(RichText::new(source.store_label()).size(18.0).strong());
                ui.add_space(8.0);
                ui.add(egui::Label::new("Sign in in your purchase browser, open your account page, then confirm the session here.").wrap());
                ui.add_space(16.0);
                small_label(ui,"Purchase browser");
                let old_browser=self.settings.browser;
                egui::ComboBox::from_id_salt("purchase_browser").selected_text(self.settings.browser.label()).width(220.0).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.settings.browser,BrowserChoice::SystemDefault,"System default");
                    if cfg!(target_os="macos") {
                        ui.selectable_value(&mut self.settings.browser,BrowserChoice::Chrome,"Google Chrome");
                        ui.selectable_value(&mut self.settings.browser,BrowserChoice::Safari,"Safari");
                    }
                });
                if old_browser!=self.settings.browser { self.invalidate_runtime_logins(); self.disarm_all(); self.persist(); }
                ui.add_space(14.0);
                ui.horizontal_wrapped(|ui| {
                    if ui.add(primary_button("Open sign-in page")).clicked() {
                        if let Err(e)=browser::open_url(self.settings.browser,&shopify::account_login_url(source)) { self.toast=Some((e,0.0)); }
                    }
                    if soft_button(ui,"Open account page").clicked() { let _=browser::open_url(self.settings.browser,&shopify::account_url(source)); }
                });
                ui.add_space(16.0);
                if self.login_ready_for(source) {
                    notice(ui,"Session confirmed for this store",GREEN);
                    if let Some(at)=self.login_current_for(source) { small_label(ui,&format!("Confirmed {}",at.format("%H:%M:%S"))); }
                    if soft_button(ui,"Clear confirmation").clicked() { self.clear_login_for(source); self.disarm_all(); self.persist(); }
                } else {
                    if self.login_previous_for(source).is_some() { small_label(ui,"Your previous confirmation needs a fresh check."); }
                    if ui.add(secondary_button("I am signed in to this store")).clicked() { self.confirm_login_for(source); self.persist(); }
                }
                ui.add_space(10.0);
                small_label(ui,"Confirmation lasts 30 minutes. Your password and browser cookies stay in your browser.");
            });
            ui.add_space(16.0);
            card().show(ui, |ui| {
                eyebrow(ui,"02 / ARMED PIECES");
                ui.add_space(12.0);
                let targets:Vec<_>=self.watch_items.iter().filter(|w|w.source==source && self.armed_ids.contains(&w.id)).map(|w|(w.id,w.runtime.title.clone().unwrap_or_else(||w.input.clone()))).collect();
                if targets.is_empty() { ui.label("No targets armed yet. Add your preferences in Watchlist or Friday releases."); }
                for (id,title) in targets {
                    ui.add(egui::Label::new(RichText::new(title).strong()).wrap());
                    if soft_button(ui,"Disarm").clicked() { self.disarm_target(id); }
                    ui.add_space(8.0);
                }
                ui.add_space(8.0);
                small_label(ui,"A stock match opens the store checkout. You review and complete the final order there.");
            });
            ui.add_space(16.0);
            card().show(ui, |ui| {
                eyebrow(ui,"03 / RECENT ACTIVITY");
                ui.add_space(12.0);
                if self.purchase_log.is_empty() { small_label(ui,"Nothing to report yet."); }
                for entry in self.purchase_log.iter().take(20) {
                    small_label(ui,&entry.at.format("%H:%M:%S").to_string());
                    ui.add(egui::Label::new(&entry.text).wrap());
                    ui.add_space(10.0);
                }
            });
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
        egui::ScrollArea::vertical().id_salt("release_scroll").auto_shrink([false,false]).show(ui, |ui| {
            section_title(ui,"Friday, thoughtfully prepared", "Your release schedule and wish list, in Japan time.");
            if source!=StoreSource::JapanOnlineStore { notice(ui,"Friday scheduling is available for the Japan store. Use Watchlist for this store.",ROSE); return; }
            let old=self.settings.release_schedule.clone();
            card().show(ui, |ui| {
                eyebrow(ui,"THE NEXT OPENING");
                ui.add_space(12.0);
                let now=Utc::now();
                let schedule=&mut self.settings.release_schedule;
                let opening=schedule.active_slot(now).or_else(||schedule.next_opening(now));
                if let Some(opening)=opening {
                    ui.label(RichText::new(opening.format("Friday, %d %B · %H:%M JST").to_string()).size(24.0).strong().color(INK));
                    let seconds=opening.signed_duration_since(now).num_seconds();
                    small_label(ui, &if seconds>0 { format!("{}d {}h {}m to go",seconds/86400,(seconds/3600)%24,(seconds/60)%60) } else { "Release window is open".into() });
                }
                ui.add_space(16.0);
                ui.checkbox(&mut schedule.enabled,"Enable this release schedule");
                ui.add_space(12.0);
                ui.columns(3, |columns| {
                    small_label(&mut columns[0],"Friday opening · JST");
                    columns[0].horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut schedule.hour).range(0..=23).prefix("Hour "));
                        ui.add(egui::DragValue::new(&mut schedule.minute).range(0..=59).prefix("Min "));
                    });
                    small_label(&mut columns[1],"Prewarm");
                    columns[1].add(egui::DragValue::new(&mut schedule.prewarm_minutes).range(0..=60).suffix(" min"));
                    small_label(&mut columns[2],"After opening");
                    columns[2].add(egui::DragValue::new(&mut schedule.window_minutes).range(1..=120).suffix(" min"));
                });
                ui.add_space(12.0);
                small_label(ui,"Catalog interval during the window");
                ui.add(egui::DragValue::new(&mut schedule.poll_seconds).range(5..=60).suffix(" seconds"));
                ui.add_space(12.0);
                small_label(ui,"Prewarm refreshes the catalog only. Keep the app running and your computer awake.");
                ui.hyperlink_to("View the official reservation guide", "https://store.babyssb.co.jp/pages/guide_reserve");
            });
            if old!=self.settings.release_schedule { self.disarm_release(); self.persist(); }
            ui.add_space(18.0);
            ui.horizontal_wrapped(|ui| {
                if ui.add(primary_button("+ Add a release target")).clicked() {
                    let id=self.release_rules.iter().map(|r|r.id).max().unwrap_or(0)+1;
                    self.release_rules.push(ReleaseRule::new(id,source)); self.persist();
                }
                if soft_button(ui,"Sign-in setup").clicked() { self.page=Page::Purchase; }
                if !self.release_armed.is_empty() && soft_button(ui,"Disarm all").clicked() { self.disarm_release(); }
            });
            ui.add_space(16.0);
            if self.release_rules.is_empty() { empty_state(ui,"What is on your wish list?", "Add a product URL, code, or a few specific title terms to prepare your first target."); }
            let mut remove=None;
            for index in 0..self.release_rules.len() {
                if self.release_rules[index].source!=source { continue; }
                let id=self.release_rules[index].id;
                let armed=self.release_armed.contains(&id);
                let ready=self.login_ready_for(source) && self.settings.release_schedule.enabled && self.settings.release_schedule.validate().is_ok();
                let mut changed=false; let mut toggle=false;
                card().show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    eyebrow(ui,&format!("WISH LIST / {:02}", index+1));
                    ui.add_space(10.0);
                    let rule=&mut self.release_rules[index];
                    changed |= form_field(ui,"Product URL, code, or title terms",&mut rule.product_hint,"Series name | Jumper Skirt");
                    small_label(ui,"Use | between title terms that must all match.");
                    ui.add_space(12.0);
                    changed |= purchase_fields(ui,&mut rule.color_priority,&mut rule.purchase);
                    ui.add_space(12.0);
                    let matches=self.catalog.iter().filter(|p|rule.matches(source,p)).count();
                    small_label(ui,&format!("{matches} matching products · A unique match is required"));
                    if let Some(status)=self.release_status.get(&id) { notice(ui,status,ROSE); }
                    if !ready { small_label(ui,"Enable the schedule and confirm sign-in before arming."); }
                    let hint=rule.product_hint.split('|').any(|s|!s.trim().is_empty());
                    ui.add_space(12.0);
                    ui.horizontal_wrapped(|ui| {
                        if ui.add_enabled(armed || (ready&&hint),primary_button(if armed { "Disarm release" } else { "Arm this release" })).clicked() { toggle=true; }
                        if soft_button(ui,"Remove").clicked() { remove=Some(id); }
                    });
                });
                if changed { self.disarm_release_rule(id); self.persist(); }
                else if toggle {
                    if armed { self.disarm_release_rule(id); }
                    else {
                        self.release_completed.remove(&id); self.release_armed.insert(id);
                        let now=Utc::now(); let schedule=&self.settings.release_schedule;
                        self.release_run=schedule.active_slot(now).or_else(||schedule.next_opening(now)).map(|t|t.timestamp());
                        self.release_status.insert(id,"Armed · Waiting for opening and a unique match".into());
                        self.push_purchase_log(format!("Friday release target #{id} armed"));
                    }
                }
                ui.add_space(16.0);
            }
            if let Some(id)=remove { self.disarm_release_rule(id); self.release_rules.retain(|r|r.id!=id); self.release_status.remove(&id); self.persist(); }
        });
    }

    fn region_gate(&mut self, ctx: &egui::Context) {
        let mut selected = None;
        egui::CentralPanel::default().frame(Frame::new().fill(BG).stroke(Stroke::NONE).inner_margin(Margin::same(32))).show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    ui.set_max_width(640.0);
                    eyebrow(ui,"A LITTLE CLOSER TO YOUR FAVORITES");
                    ui.add_space(18.0);
                    ui.label(RichText::new("BABY Watch").size(44.0).strong().color(INK));
                    ui.add_space(10.0);
                    ui.add(egui::Label::new(RichText::new("A quiet place to browse, watch, and prepare for your next release.").size(15.0).color(MUTED)).wrap());
                    ui.add_space(32.0);
                    card().show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        eyebrow(ui,"JAPAN / OFFICIAL STORE"); ui.add_space(12.0);
                        ui.label(RichText::new("BABY & ALICE and the PIRATES").size(22.0).strong());
                        ui.add_space(8.0);
                        small_label(ui,"The official Japan collection, live stock, and Friday releases.");
                        ui.add_space(18.0);
                        if ui.add(primary_button("Explore the Japan store →")).clicked() { selected=Some(StoreSource::JapanOnlineStore); }
                    });
                    ui.add_space(18.0);
                    ui.collapsing("Looking for the China partner store?", |ui| {
                        ui.add(egui::Label::new("LaceRococo uses a separate catalog and account session.").wrap());
                        if soft_button(ui,"Open LaceRococo").clicked() { selected=Some(StoreSource::ChinaLaceRococo); }
                    });
                    ui.add_space(30.0);
                    small_label(ui,"Your preferences stay on this computer.");
                });
            });
        });
        if let Some(source) = selected {
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
                    .fill(ROSE)
                    .corner_radius(11)
                    .inner_margin(Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.set_max_width(340.0);
                        ui.add(
                            egui::Label::new(
                                RichText::new(message).size(12.5).color(Color32::WHITE),
                            )
                            .wrap(),
                        );
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
                    .stroke(Stroke::NONE),
            )
            .show(ctx, |ui| self.top_bar(ui));

        egui::SidePanel::left("side")
            .resizable(false)
            .exact_width(220.0)
            .frame(
                Frame::new()
                    .fill(Color32::from_rgb(252, 250, 249))
                    .inner_margin(Margin::same(18)),
            )
            .show(ctx, |ui| self.sidebar(ui));

        egui::CentralPanel::default()
            .frame(Frame::new().fill(BG).inner_margin(Margin::same(28)))
            .show(ctx, |ui| match self.page {
                Page::Catalog => self.catalog_page(ui),
                Page::Watchlist => self.watchlist_page(ui),
                Page::Purchase => self.purchase_page(ui),
                Page::Release => self.release_page(ui),
            });

        self.toast(ctx);
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [247.0 / 255.0, 245.0 / 255.0, 243.0 / 255.0, 1.0]
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.persist();
        let _ = self.command_tx.send(MonitorCommand::Stop);
    }
}

fn configure_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Light);
    let mut style = (*ctx.style()).clone();
    style.visuals = egui::Visuals::light();
    style.wrap_mode = Some(egui::TextWrapMode::Wrap);
    style.visuals.override_text_color = Some(INK);
    style.visuals.panel_fill = BG;
    style.visuals.window_fill = PANEL;
    style.visuals.window_stroke = Stroke::new(1.0_f32, BORDER);
    style.visuals.window_shadow = egui::epaint::Shadow::NONE;
    style.visuals.popup_shadow = egui::epaint::Shadow::NONE;
    style.visuals.window_corner_radius = CornerRadius::same(14);
    style.visuals.menu_corner_radius = CornerRadius::same(10);
    style.visuals.extreme_bg_color = Color32::from_rgb(250, 248, 249);
    style.visuals.faint_bg_color = ROSE_SOFT;
    style.visuals.selection.bg_fill = ROSE_SOFT;
    style.visuals.selection.stroke = Stroke::new(1.0_f32, ROSE);
    style.visuals.hyperlink_color = ROSE;
    for widget in [
        &mut style.visuals.widgets.noninteractive,
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
        &mut style.visuals.widgets.open,
    ] {
        widget.bg_fill = Color32::from_rgb(250, 248, 249);
        widget.weak_bg_fill = Color32::from_rgb(250, 248, 249);
        widget.bg_stroke = Stroke::new(1.0_f32, BORDER);
        widget.fg_stroke = Stroke::new(1.0_f32, INK);
        widget.corner_radius = CornerRadius::same(8);
        widget.expansion = 0.0;
    }
    style.visuals.widgets.hovered.bg_fill = ROSE_SOFT;
    style.visuals.widgets.hovered.weak_bg_fill = ROSE_SOFT;
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, ROSE);
    style.visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, ROSE);
    style.visuals.widgets.active.fg_stroke = Stroke::new(1.0_f32, ROSE);
    style.spacing.item_spacing = Vec2::new(10.0, 8.0);
    style.spacing.button_padding = Vec2::new(14.0, 9.0);
    style.spacing.interact_size = Vec2::new(32.0, 34.0);
    style.spacing.slider_width = 110.0;
    style
        .text_styles
        .insert(egui::TextStyle::Body, FontId::proportional(13.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, FontId::proportional(13.0));
    style
        .text_styles
        .insert(egui::TextStyle::Small, FontId::proportional(11.0));
    ctx.set_style(style);
}

fn card() -> Frame {
    Frame::new()
        .fill(PANEL)
        .stroke(Stroke::NONE)
        .corner_radius(16)
        .inner_margin(Margin::same(20))
}
fn eyebrow(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(10.0).strong().color(MUTED));
}
fn small_label(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).size(12.0).color(MUTED)).wrap());
}
fn section_title(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    ui.add(egui::Label::new(RichText::new(title).size(29.0).strong().color(INK)).wrap());
    ui.add_space(5.0);
    small_label(ui, subtitle);
    ui.add_space(22.0);
}
fn notice(ui: &mut egui::Ui, text: &str, color: Color32) {
    ui.add_space(8.0);
    ui.add(egui::Label::new(RichText::new(text).size(12.0).color(color)).wrap());
    ui.add_space(4.0);
}
fn empty_state(ui: &mut egui::Ui, title: &str, description: &str) {
    card().show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.add_space(22.0);
        ui.add(egui::Label::new(RichText::new(title).size(20.0).strong()).wrap());
        ui.add_space(8.0);
        small_label(ui, description);
        ui.add_space(22.0);
    });
}
fn form_field(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str) -> bool {
    small_label(ui, label);
    ui.add(
        egui::TextEdit::singleline(value)
            .desired_width(ui.available_width())
            .hint_text(hint)
            .margin(Vec2::new(10.0, 10.0)),
    )
    .changed()
}
fn purchase_fields(ui: &mut egui::Ui, color: &mut String, rule: &mut PurchaseRule) -> bool {
    let mut changed = false;
    ui.columns(2, |columns| {
        changed |= form_field(&mut columns[0], "Color priority", color, "Pink | Black");
        changed |= form_field(
            &mut columns[1],
            "Size priority",
            &mut rule.size_priority,
            "M | S",
        );
    });
    ui.add_space(10.0);
    ui.columns(2, |columns| {
        small_label(&mut columns[0], "Maximum price · JPY (0 = any)");
        changed |= columns[0]
            .add(
                egui::DragValue::new(&mut rule.max_price_yen)
                    .range(0..=1_000_000)
                    .speed(100),
            )
            .changed();
        small_label(&mut columns[1], "Quantity");
        changed |= columns[1]
            .add(egui::DragValue::new(&mut rule.quantity).range(1..=3))
            .changed();
    });
    ui.add_space(10.0);
    changed |= ui
        .checkbox(
            &mut rule.auto_open_checkout,
            "Open checkout automatically on a match",
        )
        .changed();
    changed
}
fn primary_button(text: &str) -> egui::Button<'_> {
    egui::Button::new(RichText::new(text).color(Color32::WHITE))
        .fill(ROSE)
        .stroke(Stroke::NONE)
        .corner_radius(9)
        .wrap()
        .min_size(Vec2::new(90.0, 38.0))
}
fn secondary_button(text: &str) -> egui::Button<'_> {
    egui::Button::new(RichText::new(text).color(INK))
        .fill(Color32::from_rgb(246, 242, 244))
        .stroke(Stroke::NONE)
        .corner_radius(9)
        .wrap()
        .min_size(Vec2::new(70.0, 38.0))
}
fn soft_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(secondary_button(text))
}
fn nav_button(ui: &mut egui::Ui, text: &str, selected: bool) -> egui::Response {
    ui.add_sized(
        [ui.available_width(), 42.0],
        egui::Button::new(
            RichText::new(text)
                .color(if selected { ROSE } else { MUTED })
                .size(13.0),
        )
        .fill(if selected {
            ROSE_SOFT
        } else {
            Color32::TRANSPARENT
        })
        .stroke(Stroke::NONE)
        .corner_radius(10),
    )
}
fn sidebar_count(ui: &mut egui::Ui, label: &str, count: usize) {
    ui.horizontal(|ui| {
        small_label(ui, label);
        let value = count.to_string();
        let width = ui.fonts(|fonts| {
            fonts
                .layout_no_wrap(value.clone(), FontId::proportional(13.0), INK)
                .size()
                .x
        });
        ui.add_space((ui.available_width() - width - 10.0).max(0.0));
        ui.label(RichText::new(value).color(INK).strong());
    });
}
fn chip(ui: &mut egui::Ui, text: &str) {
    ui.add(
        egui::Button::new(RichText::new(text).size(11.0).color(ROSE))
            .fill(ROSE_SOFT)
            .stroke(Stroke::NONE)
            .corner_radius(6)
            .wrap_mode(egui::TextWrapMode::Extend)
            .sense(egui::Sense::hover()),
    );
}
fn color_stock_chip(ui: &mut egui::Ui, name: &str, available: bool) {
    // A single measured widget wraps as a unit. A nested Frame with wrapped text
    // can be squeezed to the final few pixels of a row and render one letter per line.
    let name = if name.chars().count() > 38 {
        format!("{}…", name.chars().take(37).collect::<String>())
    } else {
        name.to_owned()
    };
    ui.add(
        egui::Button::new(
            RichText::new(format!("{} {name}", if available { "●" } else { "○" }))
                .size(11.0)
                .color(if available { GREEN } else { MUTED }),
        )
        .fill(if available {
            Color32::from_rgb(237, 245, 239)
        } else {
            Color32::from_rgb(246, 243, 244)
        })
        .stroke(Stroke::NONE)
        .corner_radius(6)
        .wrap_mode(egui::TextWrapMode::Extend)
        .sense(egui::Sense::hover()),
    );
}
fn product_thumbnail(ui: &mut egui::Ui, texture: Option<&egui::TextureHandle>, edge: f32) {
    Frame::new()
        .fill(Color32::from_rgb(249, 247, 246))
        .stroke(Stroke::NONE)
        .corner_radius(10)
        .show(ui, |ui| {
            ui.allocate_ui_with_layout(
                Vec2::splat(edge),
                Layout::centered_and_justified(egui::Direction::LeftToRight),
                |ui| {
                    if let Some(texture) = texture {
                        let source = texture.size_vec2();
                        let scale = (edge / source.x.max(1.0)).min(edge / source.y.max(1.0));
                        ui.add(egui::Image::new((texture.id(), source * scale)));
                    } else {
                        ui.label(RichText::new("♡").size(30.0).color(ROSE));
                    }
                },
            );
        });
}
