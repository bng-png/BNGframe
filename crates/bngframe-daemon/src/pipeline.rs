use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use bngframe_capture::{capture_for_rewards, reward_ui_analysis};
use bngframe_core::config::Config;
use bngframe_core::db::{Database, ItemRow};
use bngframe_core::ocr::{
    display_name_for_item, fuzzy_match_items, ocr_reward_screen_engine,
};
use bngframe_core::pricing::{
    resolve_lotus_reward_label, resolve_lotus_reward_path, PricingService,
};
use bngframe_core::reward_mem::{
    is_confident_reward_path, is_plain_forma_path, HarvestDepth, RewardMemScanner,
};
use bngframe_core::state::{AppState, RewardSlot, RewardSnapshot};
use bngframe_overlay::OverlayManager;
use chrono::Utc;
use tracing::{debug, info, warn};
use uuid::Uuid;

pub struct RewardPipeline {
    pub cfg: Arc<tokio::sync::RwLock<Config>>,
    pub state: Arc<AppState>,
    pub db: Arc<tokio::sync::Mutex<Database>>,
    pub pricing: Arc<PricingService>,
    pub overlay: Arc<OverlayManager>,
    pub mem: Arc<RewardMemScanner>,
    pub capture_path: std::path::PathBuf,
    pub mem_had_matches: Arc<AtomicBool>,
    pub prefetch_fired: Arc<AtomicBool>,
    pub cancel_prefetch: Arc<AtomicBool>,
    pub reward_window: Arc<AtomicBool>,
    /// Prefetch finished Opening baseline freeze (Got should wait briefly).
    pub baseline_ready: Arc<AtomicBool>,
    pub run_lock: Arc<tokio::sync::Mutex<()>>,
}

impl RewardPipeline {
    /// Poll process memory while the reward SWF is open (before Got rewards).
    pub async fn prefetch_memory(self: &Arc<Self>) {
        if self
            .reward_window
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            // Already in Opening…Got window.
            return;
        }
        self.cancel_prefetch.store(false, Ordering::SeqCst);
        self.prefetch_fired.store(false, Ordering::SeqCst);
        self.mem_had_matches.store(false, Ordering::SeqCst);
        self.baseline_ready.store(false, Ordering::SeqCst);
        self.mem.clear_last_selected();

        let consent = self.cfg.read().await.reward_memory_consent;
        if !consent {
            warn!("reward_memory_consent=false — skip memory prefetch");
            self.baseline_ready.store(true, Ordering::SeqCst);
            self.reward_window.store(false, Ordering::SeqCst);
            return;
        }

        let mem = self.mem.clone();
        match tokio::task::spawn_blocking(move || mem.refresh_baseline_depth(HarvestDepth::Fast))
            .await
        {
            Ok(Ok(n)) => info!("Prefetch: baseline frozen ({n} StoreItems paths)"),
            Ok(Err(e)) => warn!("Prefetch: baseline failed: {e:#}"),
            Err(e) => warn!("Prefetch: baseline join: {e}"),
        }
        self.baseline_ready.store(true, Ordering::SeqCst);

        info!("Prefetch memory loop on reward-screen opening (no overlay until EE)");
        let deadline = Instant::now() + Duration::from_secs(18);
        let mut deep_once = false;
        while Instant::now() < deadline {
            if self.cancel_prefetch.load(Ordering::SeqCst) {
                break;
            }

            let mem = self.mem.clone();
            let t_scan = Instant::now();
            let scan = match tokio::task::spawn_blocking(move || {
                mem.scan_depth(HarvestDepth::Fast, None, &[])
            })
            .await
            {
                Ok(Ok(s)) => {
                    debug!("prefetch scan {:?}", t_scan.elapsed());
                    s
                }
                Ok(Err(e)) => {
                    warn!("prefetch memory scan: {e:#}");
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue;
                }
                Err(e) => {
                    warn!("prefetch memory join: {e}");
                    break;
                }
            };

            // Accumulate only — local slot comes from EE `gets reward`.
            // Early memory overlay was showing stale/other players' leftovers (Xaku).
            if !scan.selected.is_empty() {
                let n = self.mem.last_selected().len();
                let confident = self
                    .mem
                    .last_selected()
                    .iter()
                    .filter(|p| is_confident_reward_path(p))
                    .count();
                info!(
                    "Prefetch: banked {} path(s) (confident={confident}) in {:?}",
                    n,
                    t_scan.elapsed()
                );
                if !deep_once && confident < 3 {
                    deep_once = true;
                    let mem = self.mem.clone();
                    let cache_dir = self.cfg.read().await.cache_dir.clone();
                    tokio::task::spawn_blocking(move || {
                        let _ = mem.scan_depth(HarvestDepth::Deep, Some(&cache_dir), &[]);
                    });
                }
            }

            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        if !self.cancel_prefetch.load(Ordering::SeqCst) {
            self.reward_window.store(false, Ordering::SeqCst);
        }
    }

    /// Background baseline refresh while not in a reward window.
    pub async fn baseline_refresher(self: Arc<Self>) {
        loop {
            tokio::time::sleep(Duration::from_secs(6)).await;
            if self.reward_window.load(Ordering::SeqCst) {
                continue;
            }
            let consent = self.cfg.read().await.reward_memory_consent;
            if !consent {
                continue;
            }
            let mem = self.mem.clone();
            match tokio::task::spawn_blocking(move || mem.refresh_baseline()).await {
                Ok(Ok(n)) => {
                    tracing::debug!("reward_mem baseline ok ({n} paths)");
                }
                Ok(Err(e)) => {
                    tracing::debug!("reward_mem baseline skip: {e:#}");
                }
                Err(_) => {}
            }
        }
    }

    pub async fn run(&self, source: &str) -> Result<RewardSnapshot> {
        self.run_with_paths_party(source, &[], 4).await
    }

    pub async fn run_with_paths(
        &self,
        source: &str,
        reward_paths: &[String],
    ) -> Result<RewardSnapshot> {
        self.run_with_paths_party(source, reward_paths, 4).await
    }

    pub async fn run_with_paths_party(
        &self,
        source: &str,
        reward_paths: &[String],
        party_size: usize,
    ) -> Result<RewardSnapshot> {
        let party_size = party_size.clamp(1, 4);
        let t0 = Instant::now();
        info!(
            "Running reward pipeline (source={source}, paths={}, party={party_size})",
            reward_paths.len()
        );

        let consent = self.cfg.read().await.reward_memory_consent;
        if !consent {
            warn!("reward_memory_consent=false — reward pipeline no-op");
            let empty = RewardSnapshot {
                id: Uuid::new_v4().to_string(),
                detected_at: Utc::now(),
                slots: vec![RewardSlot {
                    name: "—".into(),
                    matched_url_name: None,
                    platinum: None,
                    volume: None,
                    ducats: None,
                    owned: None,
                    mastered: None,
                    rank: Some(1),
                }],
                best_index: None,
                source: format!("{source}:no_consent"),
            };
            return Ok(empty);
        }

        if source == "eelog" {
            self.cancel_prefetch.store(true, Ordering::SeqCst);
            self.prefetch_fired.store(true, Ordering::SeqCst);
        }

        let prefer_ru = self.cfg.read().await.prefer_russian_names();
        let cache_dir = self.cfg.read().await.cache_dir.clone();

        let pricing = self.pricing.clone();
        let catalog_task = tokio::spawn(async move { pricing.ensure_items_cached().await });
        match catalog_task.await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => warn!("catalog ensure failed: {e}"),
            Err(e) => warn!("catalog task join failed: {e}"),
        }

        let (catalog, lotus) = {
            let db = self.db.lock().await;
            let catalog = db.all_items()?;
            let mut lotus = std::collections::HashMap::new();
            for kind in ["resource", "weapon", "warframe", "weapon_en", "relic", "custom"] {
                if let Ok(m) = db.lotus_name_map(kind) {
                    lotus.extend(m);
                }
            }
            (catalog, lotus)
        };

        let ee_paths = if source == "eelog" {
            reward_paths.to_vec()
        } else {
            Vec::new()
        };

        let mut mem_paths = if source == "memory" {
            reward_paths.to_vec()
        } else {
            self.mem.last_selected()
        };
        // Prefetch may still hold leftovers; keep only confident primes for squad fill.
        if source == "eelog" {
            mem_paths.retain(|p| is_confident_reward_path(p));
        }

        let reward_id = Uuid::new_v4().to_string();
        let mut early_announced = false;

        // Local slot is EE truth — announce it immediately, before any Deep scan.
        if source == "eelog" && !ee_paths.is_empty() {
            let (local_items, local_labels) =
                resolve_paths_to_items(&ee_paths, &catalog, &lotus, prefer_ru);
            if !local_items.is_empty() || !local_labels.is_empty() {
                let mut early_slots = Vec::new();
                for item in &local_items {
                    early_slots.push(self.slot_from_item(item, prefer_ru).await);
                }
                for (label, url) in &local_labels {
                    early_slots.push(RewardSlot {
                        name: label.clone(),
                        matched_url_name: url.clone(),
                        platinum: None,
                        volume: None,
                        ducats: None,
                        owned: None,
                        mastered: None,
                        rank: Some(1),
                    });
                }
                assign_rank_badges(&mut early_slots);
                let best_index = early_slots
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, s)| s.rank.unwrap_or(99))
                    .map(|(i, _)| i);
                let early = RewardSnapshot {
                    id: reward_id.clone(),
                    detected_at: Utc::now(),
                    slots: early_slots,
                    best_index,
                    source: format!("{source}:local"),
                };
                self.state.push_reward(early.clone()).await;
                if let Err(e) = self.overlay.show_rewards(&early).await {
                    warn!("local EE reward notify: {e}");
                } else {
                    early_announced = true;
                    self.state.emit(bngframe_core::AppEvent::OverlayShown {
                        reward_id: early.id.clone(),
                    });
                    info!(
                        "Reward notify (EE local) in {:?} — {:?}",
                        t0.elapsed(),
                        ee_paths
                    );
                }
            }
        }

        let _guard = self.run_lock.lock().await;

        // Wait for Opening baseline if prefetch is in flight (same EE flush).
        if source == "eelog" && !self.baseline_ready.load(Ordering::SeqCst) {
            let wait_deadline = Instant::now() + Duration::from_millis(1500);
            while !self.baseline_ready.load(Ordering::SeqCst) && Instant::now() < wait_deadline {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }

        let confident_so_far = {
            let mut s = std::collections::HashSet::new();
            for p in mem_paths.iter().chain(ee_paths.iter()) {
                if is_confident_reward_path(p) || is_plain_forma_path(p) {
                    s.insert(p.clone());
                }
            }
            s.len()
        };
        let need_rescan =
            source == "manual" || (source == "eelog" && party_size > confident_so_far);
        if need_rescan && source != "memory" {
            let mem = self.mem.clone();
            let cache = cache_dir.clone();
            let hints = ee_paths.clone();
            let max_slots = party_size;
            // Deep matches Fast baseline coverage class; Exhaustive flooded "new".
            let t_deep = Instant::now();
            match tokio::task::spawn_blocking(move || {
                mem.scan_depth_capped(HarvestDepth::Deep, Some(&cache), &hints, max_slots)
            })
            .await
            {
                Ok(Ok(scan)) => {
                    info!(
                        "Got mem Deep in {:?} all={} new={} selected={} (before={confident_so_far}/{party_size})",
                        t_deep.elapsed(),
                        scan.all_paths.len(),
                        scan.new_paths.len(),
                        scan.selected.len()
                    );
                    for p in scan.selected {
                        if !mem_paths.iter().any(|x| x == &p) {
                            mem_paths.push(p);
                        }
                    }
                    mem_paths.retain(|p| {
                        is_confident_reward_path(p)
                            || ee_paths.iter().any(|e| e == p)
                            || is_plain_forma_path(p)
                    });
                }
                Ok(Err(e)) => warn!("memory Deep scan failed: {e:#}"),
                Err(e) => warn!("memory Deep join failed: {e}"),
            }
        } else if source == "eelog" {
            info!(
                "Got rewards: reuse prefetch mem ({} path(s), ≈{confident_so_far}/{party_size})",
                mem_paths.len()
            );
        }

        // EE local first (stable order), then memory squad, then OCR.
        let mut slots = Vec::new();
        let mut matched_count = 0usize;
        let mut seen_urls = std::collections::HashSet::new();

        let (local_items, local_labels) =
            resolve_paths_to_items(&ee_paths, &catalog, &lotus, prefer_ru);
        for item in &local_items {
            if !seen_urls.insert(item.url_name.clone()) {
                continue;
            }
            matched_count += 1;
            slots.push(self.slot_from_item(item, prefer_ru).await);
        }
        for (label, url) in &local_labels {
            matched_count += 1;
            slots.push(RewardSlot {
                name: label.clone(),
                matched_url_name: url.clone(),
                platinum: None,
                volume: None,
                ducats: None,
                owned: None,
                mastered: None,
                rank: None,
            });
        }

        let mem_only: Vec<String> = mem_paths
            .iter()
            .filter(|p| !ee_paths.iter().any(|e| e == *p))
            .cloned()
            .collect();
        let (mem_items, mem_labels) =
            resolve_paths_to_items(&mem_only, &catalog, &lotus, prefer_ru);
        for item in &mem_items {
            if slots.len() >= party_size {
                break;
            }
            if !seen_urls.insert(item.url_name.clone()) {
                continue;
            }
            matched_count += 1;
            slots.push(self.slot_from_item(item, prefer_ru).await);
        }
        for (label, url) in &mem_labels {
            if slots.len() >= party_size {
                break;
            }
            matched_count += 1;
            slots.push(RewardSlot {
                name: label.clone(),
                matched_url_name: url.clone(),
                platinum: None,
                volume: None,
                ducats: None,
                owned: None,
                mastered: None,
                rank: None,
            });
        }

        info!(
            "After EE+mem: {}/{} slots (ee={} mem_extra={})",
            slots.len(),
            party_size,
            ee_paths.len(),
            mem_only.len()
        );

        // Proton rarely exposes other players' StoreItems — OCR fills remaining cards.
        let mut ocr_added = false;
        if slots.len() < party_size && source == "eelog" {
            let (ocr_lang, prefer_rapid, monitor) = {
                let cfg = self.cfg.read().await;
                (
                    cfg.effective_ocr_lang(),
                    cfg.use_rapidocr(),
                    cfg.monitor.clone(),
                )
            };
            info!(
                "Slots {}/{} — OCR for remaining squad cards",
                slots.len(),
                party_size
            );
            let capture_path = self.capture_path.clone();
            let monitor_owned = monitor.clone();
            let cache = cache_dir.clone();
            let t_ocr = Instant::now();
            let ocr_result = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<String>> {
                let img = capture_for_rewards(monitor_owned.as_deref(), &capture_path)?;
                let analysis = reward_ui_analysis(&img);
                if analysis.score < 12 {
                    anyhow::bail!("capture score too low ({})", analysis.score);
                }
                let hint = match analysis.peaks {
                    2..=4 => Some(analysis.peaks as u32),
                    _ => None,
                };
                let ocr =
                    ocr_reward_screen_engine(&img, &ocr_lang, &cache, prefer_rapid, hint)?;
                Ok(ocr.slot_texts)
            })
            .await;
            match ocr_result {
                Ok(Ok(texts)) => {
                    info!("OCR texts in {:?}: {:?}", t_ocr.elapsed(), texts);
                    for text in texts {
                        if slots.len() >= party_size {
                            break;
                        }
                        if text.trim().is_empty() {
                            continue;
                        }
                        for item in fuzzy_match_items(&text, &catalog) {
                            if seen_urls.contains(&item.url_name) {
                                continue;
                            }
                            info!("OCR matched: {text:?} → {}", item.url_name);
                            seen_urls.insert(item.url_name.clone());
                            matched_count += 1;
                            ocr_added = true;
                            slots.push(self.slot_from_item(item, prefer_ru).await);
                            break;
                        }
                    }
                }
                Ok(Err(e)) => warn!("OCR fill failed: {e:#}"),
                Err(e) => warn!("OCR join failed: {e}"),
            }
        }

        if source == "memory" && matched_count > 0 {
            self.mem_had_matches.store(true, Ordering::SeqCst);
        }

        if slots.is_empty() {
            slots.push(RewardSlot {
                name: "—".into(),
                matched_url_name: None,
                platinum: None,
                volume: None,
                ducats: None,
                owned: None,
                mastered: None,
                rank: Some(1),
            });
        }

        assign_rank_badges(&mut slots);
        let best_index = if matched_count > 0 {
            slots
                .iter()
                .enumerate()
                .min_by_key(|(_, s)| s.rank.unwrap_or(99))
                .map(|(i, _)| i)
        } else {
            None
        };

        let reward = RewardSnapshot {
            id: reward_id,
            detected_at: Utc::now(),
            slots,
            best_index,
            source: source.into(),
        };

        if source == "eelog" {
            self.reward_window.store(false, Ordering::SeqCst);
        }

        if matched_count == 0 {
            warn!("No catalog matches (source={source}); skipping overlay");
            if source != "memory" {
                self.state.push_reward(reward.clone()).await;
            }
            return Ok(reward);
        }

        self.state.push_reward(reward.clone()).await;

        if !early_announced || ocr_added || reward.slots.len() > 1 {
            self.overlay.show_rewards(&reward).await?;
            self.state.emit(bngframe_core::AppEvent::OverlayShown {
                reward_id: reward.id.clone(),
            });
            info!(
                "Reward notify in {:?} ({} slots, ocr_added={ocr_added})",
                t0.elapsed(),
                reward.slots.len()
            );
        } else if let Err(e) = self.overlay.update_rewards(&reward).await {
            warn!("silent reward update: {e}");
        } else {
            info!(
                "Reward done — kept early notify ({:?}, {} slots)",
                t0.elapsed(),
                reward.slots.len()
            );
        }

        Ok(reward)
    }

    async fn slot_from_item(&self, item: &ItemRow, prefer_ru: bool) -> RewardSlot {
        let mut platinum = None;
        let mut volume = None;
        if item.url_name != "forma" {
            if let Some(p) = self.pricing.price_cached_only(&item.url_name).await {
                if p.platinum > 0.0 {
                    platinum = Some(p.platinum);
                    volume = Some(p.volume);
                }
            }
        }
        RewardSlot {
            name: display_name_for_item(item, prefer_ru),
            matched_url_name: Some(item.url_name.clone()),
            platinum,
            volume,
            ducats: item.ducats,
            owned: None,
            mastered: None,
            rank: None,
        }
    }
}

fn resolve_paths_to_items<'a>(
    paths: &[String],
    catalog: &'a [ItemRow],
    lotus: &std::collections::HashMap<String, String>,
    prefer_ru: bool,
) -> (Vec<&'a ItemRow>, Vec<(String, Option<String>)>) {
    let mut items = Vec::new();
    let mut labels = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for path in paths {
        if let Some(item) = resolve_lotus_reward_path(path, catalog) {
            if seen.insert(item.url_name.clone()) {
                info!("path matched: {path} → {}", item.url_name);
                items.push(item);
            }
            continue;
        }
        if let Some(label) = resolve_lotus_reward_label(path, lotus, prefer_ru) {
            info!("path label (no WFM): {path} → {label}");
            labels.push((label, None));
        } else {
            warn!("path unmatched: {path}");
        }
    }
    (items, labels)
}

fn assign_rank_badges(slots: &mut [RewardSlot]) {
    let mut order: Vec<usize> = (0..slots.len()).collect();
    order.sort_by(|&a, &b| {
        let pa = slots[a].platinum.unwrap_or(-1.0);
        let pb = slots[b].platinum.unwrap_or(-1.0);
        pb.partial_cmp(&pa)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                slots[b]
                    .ducats
                    .unwrap_or(0)
                    .cmp(&slots[a].ducats.unwrap_or(0))
            })
    });
    for (rank, &idx) in order.iter().enumerate() {
        slots[idx].rank = Some((rank + 1) as u8);
    }
}
