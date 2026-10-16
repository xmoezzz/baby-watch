use std::{
    collections::{HashMap, HashSet},
    thread,
    time::{Duration, Instant},
};

use chrono::Local;
use crossbeam_channel::{Receiver, Sender};

use crate::{
    model::{CatalogProduct, StoreSource, WatchItem},
    release::ReleaseSchedule,
    shopify,
};

#[derive(Debug, Clone)]
pub enum MonitorCommand {
    Replace(Vec<WatchItem>),
    SetInterval(u64),
    SetArmedInterval(u64),
    SetArmed(Vec<u64>),
    SetCatalogEnabled(bool),
    SetCatalogSource(StoreSource),
    CheckNow(Option<u64>),
    RefreshCatalog,
    SetReleaseSchedule(Option<ReleaseSchedule>),
    Stop,
}

#[derive(Debug, Clone)]
pub enum CatalogChange {
    NewProduct { title: String },
    Restocked { title: String, color: String },
}

#[derive(Debug, Clone)]
pub enum MonitorEvent {
    CheckStarted(u64),
    Updated {
        id: u64,
        snapshot: shopify::ProductSnapshot,
        checked_at: chrono::DateTime<Local>,
        became_available: bool,
    },
    Failed {
        id: u64,
        message: String,
        checked_at: chrono::DateTime<Local>,
    },
    CatalogStarted {
        source: StoreSource,
    },
    CatalogUpdated {
        source: StoreSource,
        products: Vec<CatalogProduct>,
        checked_at: chrono::DateTime<Local>,
        changes: Vec<CatalogChange>,
    },
    CatalogFailed {
        source: StoreSource,
        message: String,
        checked_at: chrono::DateTime<Local>,
    },
}

/// Independent workers ensure a full catalog scan or an ordinary slow target
/// cannot block the ARMED worker. Commands are broadcast, then drained before I/O.
pub fn spawn(
    command_rx: Receiver<MonitorCommand>,
    event_tx: Sender<MonitorEvent>,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("baby-monitor-dispatch".into())
        .spawn(move || {
            let mut senders = Vec::new();
            for lane in [Lane::Catalog, Lane::Focused, Lane::Armed] {
                let (tx, rx) = crossbeam_channel::unbounded();
                let events = event_tx.clone();
                thread::Builder::new()
                    .name(format!("baby-{lane:?}"))
                    .spawn(move || run_lane(lane, rx, events))
                    .expect("failed to spawn monitor worker");
                senders.push(tx);
            }
            while let Ok(command) = command_rx.recv() {
                let stop = matches!(command, MonitorCommand::Stop);
                for tx in &senders {
                    let _ = tx.send(command.clone());
                }
                if stop {
                    break;
                }
            }
        })
        .expect("failed to spawn monitor dispatcher")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lane {
    Catalog,
    Focused,
    Armed,
}

fn run_lane(lane: Lane, rx: Receiver<MonitorCommand>, events: Sender<MonitorEvent>) {
    let client = reqwest::blocking::Client::builder()
        .user_agent("BABY-Watch/0.7.0 stock-monitor")
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(if lane == Lane::Catalog {
            20
        } else {
            8
        }))
        .build()
        .expect("failed to create HTTP client");
    let mut targets = Vec::<WatchItem>::new();
    let mut armed = HashSet::<u64>::new();
    let mut source = StoreSource::JapanOnlineStore;
    let mut enabled = false;
    let mut interval = 30;
    let mut armed_interval = 8;
    let mut release: Option<ReleaseSchedule> = None;
    let mut next = Instant::now() + Duration::from_secs(2);
    let mut previous_available = HashMap::new();
    let mut previous_catalogs = HashMap::new();
    let mut was_warm = false;
    let mut prior_sale = None;
    let mut failures = 0_u32;
    loop {
        let wait = next
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(500));
        let first = match rx.recv_timeout(wait) {
            Ok(c) => Some(c),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => None,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
        };
        let mut pending = HashSet::new();
        let mut refresh = false;
        for command in first.into_iter().chain(rx.try_iter()) {
            match command {
                MonitorCommand::Replace(items) => targets = items,
                MonitorCommand::SetInterval(seconds) => {
                    interval = seconds.clamp(30, 3600);
                    next = next.min(Instant::now() + Duration::from_secs(interval));
                }
                MonitorCommand::SetArmedInterval(seconds) => armed_interval = seconds.clamp(5, 60),
                MonitorCommand::SetArmed(ids) => {
                    let new_ids = ids.into_iter().collect();
                    if armed != new_ids && lane == Lane::Armed {
                        next = Instant::now();
                    }
                    armed = new_ids;
                }
                MonitorCommand::SetCatalogEnabled(on) => {
                    if on && !enabled && lane == Lane::Catalog {
                        next = Instant::now();
                    }
                    enabled = on;
                }
                MonitorCommand::SetCatalogSource(new_source) => {
                    if source != new_source {
                        next = Instant::now();
                    }
                    source = new_source;
                }
                MonitorCommand::SetReleaseSchedule(schedule) => release = schedule,
                MonitorCommand::CheckNow(id) => {
                    if let Some(id) = id {
                        pending.insert(id);
                    } else {
                        next = Instant::now();
                    }
                }
                MonitorCommand::RefreshCatalog => refresh = true,
                MonitorCommand::Stop => return,
            }
        }
        let now = chrono::Utc::now();
        let warm = release
            .as_ref()
            .is_some_and(|s| s.active_slot(now).is_some());
        let sale = release.as_ref().and_then(|s| s.sale_slot(now));
        if lane == Lane::Catalog && ((warm && !was_warm) || (sale.is_some() && sale != prior_sale))
        {
            next = Instant::now();
        }
        was_warm = warm;
        prior_sale = sale;
        let seconds = if lane == Lane::Armed {
            armed_interval
        } else if warm && lane == Lane::Catalog {
            release.as_ref().unwrap().poll_seconds
        } else {
            interval
        };
        if lane == Lane::Catalog {
            if refresh || ((enabled || warm) && Instant::now() >= next) {
                let success = check_catalog(&client, source, &events, &mut previous_catalogs);
                failures = if success {
                    0
                } else {
                    failures.saturating_add(1)
                };
                // Bounded backoff, including HTTP 429/5xx; never retry in a tight loop.
                next = Instant::now()
                    + Duration::from_secs(seconds.saturating_mul(1 << failures.min(4)).min(300));
            } else if !enabled && !warm {
                next = Instant::now() + Duration::from_secs(1);
            }
            continue;
        }
        let due = Instant::now() >= next;
        // A stable order respects the order targets were added, rather than HashSet iteration order.
        for target in targets.iter().filter(|t| t.enabled && t.source == source) {
            let in_lane = armed.contains(&target.id) == (lane == Lane::Armed);
            if in_lane && (due || pending.contains(&target.id)) {
                check_targets(
                    &client,
                    std::slice::from_ref(target),
                    Some(target.id),
                    Some(source),
                    &events,
                    &mut previous_available,
                );
            }
        }
        if due {
            next = Instant::now() + Duration::from_secs(seconds);
        }
    }
}

fn check_targets(
    client: &reqwest::blocking::Client,
    targets: &[WatchItem],
    only_id: Option<u64>,
    only_source: Option<StoreSource>,
    event_tx: &Sender<MonitorEvent>,
    previous_available: &mut HashMap<u64, bool>,
) {
    for target in targets.iter().filter(|t| t.enabled) {
        if let Some(id) = only_id {
            if id != target.id {
                continue;
            }
        }
        if let Some(source) = only_source {
            if target.source != source {
                continue;
            }
        }

        let _ = event_tx.send(MonitorEvent::CheckStarted(target.id));
        let now = Local::now();
        match shopify::fetch_product(client, target.source, &target.product_url) {
            Ok(snapshot) => {
                let available = snapshot
                    .variants
                    .iter()
                    .any(|v| v.available && v.matches_filter(&target.variant_filter));
                let previous = previous_available
                    .insert(target.id, available)
                    .unwrap_or(false);
                let became_available = available && !previous;
                let _ = event_tx.send(MonitorEvent::Updated {
                    id: target.id,
                    snapshot,
                    checked_at: now,
                    became_available,
                });
            }
            Err(err) => {
                let _ = event_tx.send(MonitorEvent::Failed {
                    id: target.id,
                    message: err.to_string(),
                    checked_at: now,
                });
            }
        }
    }
}

fn check_catalog(
    client: &reqwest::blocking::Client,
    source: StoreSource,
    event_tx: &Sender<MonitorEvent>,
    previous_catalogs: &mut HashMap<StoreSource, HashMap<u64, HashMap<u64, bool>>>,
) -> bool {
    let _ = event_tx.send(MonitorEvent::CatalogStarted { source });
    let now = Local::now();

    match shopify::fetch_catalog(client, source) {
        Ok(products) => {
            let current = catalog_availability(&products);
            let changes = previous_catalogs
                .get(&source)
                .map(|previous| diff_catalog(previous, &products))
                .unwrap_or_default();
            previous_catalogs.insert(source, current);

            let _ = event_tx.send(MonitorEvent::CatalogUpdated {
                source,
                products,
                checked_at: now,
                changes,
            });
            true
        }
        Err(err) => {
            let _ = event_tx.send(MonitorEvent::CatalogFailed {
                source,
                message: err.to_string(),
                checked_at: now,
            });
            false
        }
    }
}

fn catalog_availability(products: &[CatalogProduct]) -> HashMap<u64, HashMap<u64, bool>> {
    products
        .iter()
        .map(|p| {
            let variants = p
                .variants
                .iter()
                .map(|v| (v.id, v.available))
                .collect::<HashMap<_, _>>();
            (p.id, variants)
        })
        .collect()
}

fn diff_catalog(
    previous: &HashMap<u64, HashMap<u64, bool>>,
    products: &[CatalogProduct],
) -> Vec<CatalogChange> {
    let mut changes = Vec::new();

    for product in products {
        let Some(old_variants) = previous.get(&product.id) else {
            changes.push(CatalogChange::NewProduct {
                title: product.title.clone(),
            });
            continue;
        };

        let mut restocked_colors = Vec::<String>::new();
        for variant in &product.variants {
            if !variant.available {
                continue;
            }
            let was_available = old_variants.get(&variant.id).copied().unwrap_or(false);
            if !was_available {
                let color = variant.color_or_display();
                if !restocked_colors.iter().any(|seen| seen == &color) {
                    restocked_colors.push(color);
                }
            }
        }

        for color in restocked_colors {
            changes.push(CatalogChange::Restocked {
                title: product.title.clone(),
                color,
            });
        }
    }

    changes
}
