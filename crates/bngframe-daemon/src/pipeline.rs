use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use bngframe_capture::{
    capture_for_rewards, reward_ui_score, warframe_is_on_active_workspace, CapturedImage,
};
use bngframe_core::config::Config;
use bngframe_core::db::{Database, ItemRow};
use bngframe_core::ocr::{
    display_name_for_item, fuzzy_match_item, fuzzy_match_items, ocr_reward_screen_engine,
    reward_text_likely, OcrResult,
};
use bngframe_core::pricing::{
    resolve_lotus_reward_label, resolve_lotus_reward_path, PricingService,
};
use bngframe_core::state::{AppState, RewardSlot, RewardSnapshot};
use bngframe_overlay::OverlayManager;
use chrono::Utc;
use tracing::{info, warn};
use uuid::Uuid;

pub struct RewardPipeline {
    pub cfg: Arc<tokio::sync::RwLock<Config>>,
    pub state: Arc<AppState>,
    pub db: Arc<tokio::sync::Mutex<Database>>,
    pub pricing: Arc<PricingService>,
    pub overlay: Arc<OverlayManager>,
    pub capture_path: PathBuf,
    /// Frame grabbed on RewardScreenOpening (before Got rewards).
    pub prefetch: Arc<tokio::sync::Mutex<Option<(Instant, CapturedImage)>>>,
    /// Visual OCR already fired from a good prefetch (before Got rewards).
    pub prefetch_ocr_fired: Arc<AtomicBool>,
    /// Last visual OCR found catalog matches — Got rewards can skip heavy OCR.
    pub visual_had_matches: Arc<AtomicBool>,
    /// Set by Got-rewards pipeline so the prefetch loop can exit.
    pub cancel_prefetch: Arc<AtomicBool>,
    /// Serialize reward pipeline runs (prefetch OCR vs Got rewards).
    pub run_lock: Arc<tokio::sync::Mutex<()>>,
}

impl RewardPipeline {
    /// Start capture as soon as the reward SWF opens (before Got rewards).
    /// Keeps refreshing until Got rewards cancels the loop or ~18s elapse.
    pub async fn prefetch_capture(self: &Arc<Self>) {
        {
            let slot = self.prefetch.lock().await;
            if let Some((t, _)) = slot.as_ref() {
                if t.elapsed() < Duration::from_secs(2) {
                    // Another opening event — already prefetching.
                    return;
                }
            }
        }
        self.cancel_prefetch.store(false, Ordering::SeqCst);
        self.prefetch_ocr_fired.store(false, Ordering::SeqCst);
        self.visual_had_matches.store(false, Ordering::SeqCst);
        let monitor = self.cfg.read().await.monitor.clone();
        let path = self.capture_path.clone();
        let (ocr_lang, cache_dir) = {
            let cfg = self.cfg.read().await;
            (cfg.effective_ocr_lang(), cfg.cache_dir.clone())
        };
        info!("Prefetch capture loop on reward-screen opening");
        let deadline = Instant::now() + Duration::from_secs(18);
        let mut best: Option<(i32, CapturedImage)> = None;
        let mut good_streak = 0u32;
        while Instant::now() < deadline {
            if self.cancel_prefetch.load(Ordering::SeqCst) {
                break;
            }
            // Visual OCR matched — stop. If it's still running (or just missed),
            // keep capturing better frames for the Got-rewards path; the worker
            // clears prefetch_ocr_fired when the pass finds nothing.
            if self.visual_had_matches.load(Ordering::SeqCst) {
                break;
            }
            let path = path.clone();
            let monitor_owned = monitor.clone();
            let frame = match tokio::task::spawn_blocking(move || {
                capture_for_rewards(monitor_owned.as_deref(), &path)
            })
            .await
            {
                Ok(Ok(img)) => img,
                Ok(Err(e)) => {
                    warn!("prefetch capture failed: {e}");
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    continue;
                }
                Err(e) => {
                    warn!("prefetch join failed: {e}");
                    break;
                }
            };
            let score = reward_ui_score(&frame);
            info!("Prefetch frame score={score}");
            let better = best.as_ref().map(|(s, _)| score > *s).unwrap_or(true);
            if better {
                best = Some((score, frame.clone()));
                let mut slot = self.prefetch.lock().await;
                *slot = Some((Instant::now(), frame.clone()));
            }
            if score >= 20 {
                good_streak += 1;
            } else {
                good_streak = 0;
            }
            // Wine often delays Got rewards ~10s while the UI is already up.
            // Always probe for readable reward text — blue ability FX can score
            // 100+ on a combat frame and must not skip the probe.
            if good_streak >= 2
                && score >= 20
                && !self.prefetch_ocr_fired.load(Ordering::SeqCst)
                && !self.cancel_prefetch.load(Ordering::SeqCst)
            {
                let probe_img = frame.clone();
                let lang = ocr_lang.clone();
                let cache = cache_dir.clone();
                let text_ok = tokio::task::spawn_blocking(move || {
                    reward_text_likely(&probe_img, &lang, &cache)
                })
                .await
                .unwrap_or(false);
                if text_ok
                    && self
                        .prefetch_ocr_fired
                        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                {
                    info!("Prefetch: starting visual OCR (score={score}, text_probe=ok)");
                    let img = frame.clone();
                    let this = Arc::clone(self);
                    tokio::spawn(async move {
                        match this
                            .run_with_paths_and_image("visual", &[], Some(img))
                            .await
                        {
                            Ok(_) => {
                                if !this.visual_had_matches.load(Ordering::SeqCst) {
                                    // Empty/garbage frame — drop it and retry later.
                                    this.prefetch_ocr_fired.store(false, Ordering::SeqCst);
                                    let mut slot = this.prefetch.lock().await;
                                    slot.take();
                                    warn!("visual OCR found nothing — discarded frame, keep capturing");
                                }
                            }
                            Err(e) => {
                                warn!("visual OCR pipeline: {e}");
                                this.prefetch_ocr_fired.store(false, Ordering::SeqCst);
                            }
                        }
                    });
                } else if !text_ok {
                    info!("Prefetch: score={score} but no reward text yet — keep capturing");
                    // Don't burn the streak forever on a glowing combat frame.
                    if score >= 80 {
                        good_streak = 0;
                    }
                }
            }
            if score >= 20 {
                tokio::time::sleep(Duration::from_millis(400)).await;
            } else {
                tokio::time::sleep(Duration::from_millis(280)).await;
            }
        }
    }

    pub async fn run(&self, source: &str) -> Result<RewardSnapshot> {
        self.run_with_paths(source, &[]).await
    }

    pub async fn run_with_paths(
        &self,
        source: &str,
        reward_paths: &[String],
    ) -> Result<RewardSnapshot> {
        self.run_with_paths_and_image(source, reward_paths, None)
            .await
    }

    pub async fn run_with_paths_and_image(
        &self,
        source: &str,
        reward_paths: &[String],
        forced_image: Option<CapturedImage>,
    ) -> Result<RewardSnapshot> {
        let t0 = Instant::now();
        info!(
            "Running reward pipeline (source={source}, ee_paths={})",
            reward_paths.len()
        );
        if source == "eelog" {
            self.cancel_prefetch.store(true, Ordering::SeqCst);
            self.prefetch_ocr_fired.store(true, Ordering::SeqCst);
        }
        // Visual OCR already showed squad rewards — Got rewards only needs EE merge.
        let skip_ocr = source == "eelog" && self.visual_had_matches.load(Ordering::SeqCst);
        if skip_ocr {
            info!("Got rewards: visual already matched — EE notify only (skip capture/OCR)");
        }

        let (monitor, ocr_lang, cache_dir, prefer_ru, prefer_rapid) = {
            let cfg = self.cfg.read().await;
            (
                cfg.monitor.clone(),
                cfg.effective_ocr_lang(),
                cfg.cache_dir.clone(),
                cfg.prefer_russian_names(),
                cfg.use_rapidocr(),
            )
        };

        // Prefer forced image (visual path), else fresh prefetch, else burst.
        let prefetched = if skip_ocr {
            None
        } else if let Some(img) = forced_image {
            let score = reward_ui_score(&img);
            info!("Using forced capture (score={score})");
            Some(img)
        } else {
            let mut slot = self.prefetch.lock().await;
            match slot.as_ref() {
                Some((t, img)) if t.elapsed() < Duration::from_secs(20) => {
                    let score = reward_ui_score(img);
                    if score >= 12 {
                        info!(
                            "Using prefetched reward capture ({:?} old, score={score})",
                            t.elapsed()
                        );
                        // Clone so visual OCR / Got can both use a fresh-ish frame.
                        let img = img.clone();
                        if source == "eelog" {
                            slot.take();
                        }
                        Some(img)
                    } else {
                        info!(
                            "Prefetch score too low ({score}) — capturing again ({:?} old)",
                            t.elapsed()
                        );
                        if source == "eelog" {
                            slot.take();
                        }
                        None
                    }
                }
                Some(_) => {
                    info!("Prefetch too old — capturing again");
                    if source == "eelog" {
                        slot.take();
                    }
                    None
                }
                None => None,
            }
        };
        let capture_task = if skip_ocr {
            None
        } else {
            let monitor = monitor.clone();
            let this_path = self.capture_path.clone();
            let source = source.to_string();
            let already = prefetched;
            Some(tokio::spawn(async move {
                if let Some(img) = already {
                    return Some(img);
                }
                if source == "eelog" || source == "visual" {
                    capture_reward_burst(&this_path, monitor.as_deref()).await
                } else {
                    match tokio::task::spawn_blocking(move || {
                        capture_for_rewards(monitor.as_deref(), &this_path)
                    })
                    .await
                    {
                        Ok(Ok(img)) => Some(img),
                        Ok(Err(e)) => {
                            warn!("screen capture failed: {e}");
                            None
                        }
                        Err(e) => {
                            warn!("capture join failed: {e}");
                            None
                        }
                    }
                }
            }))
        };

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

        let (ee_items, ee_labels) = resolve_ee_rewards(reward_paths, &catalog, &lotus, prefer_ru);
        let reward_id = Uuid::new_v4().to_string();

        // Squad rewards already read from the screen by the earlier visual pass.
        // Must be captured *before* pushing the EE snapshot, otherwise the lookup
        // finds this run's own EE-only snapshot and the OCR slots are dropped.
        let visual_urls: Vec<String> = if skip_ocr {
            let q = self.state.last_rewards.read().await;
            q.iter()
                .find(|r| r.source.starts_with("visual"))
                .map(|r| {
                    r.slots
                        .iter()
                        .filter_map(|s| s.matched_url_name.clone())
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let visual_items: Vec<&ItemRow> = visual_urls
            .iter()
            .filter(|url| !ee_items.iter().any(|e| e.url_name == **url))
            .filter_map(|url| catalog.iter().find(|i| i.url_name == *url))
            .collect();

        // Fast path: EE notify WITHOUT the OCR lock so Wine-delayed Got rewards
        // still alert while a visual OCR pass holds run_lock.
        let mut early_announced = false;
        if !ee_items.is_empty() || !ee_labels.is_empty() {
            let mut early_slots = Vec::new();
            for item in &ee_items {
                early_slots.push(self.slot_from_item(item, prefer_ru).await);
            }
            for (label, url) in &ee_labels {
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
            for item in &visual_items {
                if early_slots.len() >= 4 {
                    break;
                }
                early_slots.push(self.slot_from_item(item, prefer_ru).await);
            }
            rank_slots(&mut early_slots);
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
                source: format!("{source}:early"),
            };
            self.state.push_reward(early.clone()).await;
            if let Err(e) = self.overlay.show_rewards(&early).await {
                warn!("early EE notify: {e}");
            } else {
                early_announced = true;
                self.state.emit(bngframe_core::AppEvent::OverlayShown {
                    reward_id: early.id.clone(),
                });
                info!(
                    "EE notify (cached prices) in {:?} — {} item(s)",
                    t0.elapsed(),
                    ee_items.len() + ee_labels.len()
                );
            }
        }

        let _guard = self.run_lock.lock().await;

        let capture = if skip_ocr {
            None
        } else if let Some(task) = capture_task {
            match task.await {
                Ok(img) => img,
                Err(e) => {
                    warn!("capture task join failed: {e}");
                    None
                }
            }
        } else {
            None
        };
        if !skip_ocr {
            info!("Capture phase done in {:?}", t0.elapsed());
        }

        let mut ocr = if skip_ocr {
            OcrResult {
                raw_lines: vec![],
                slot_texts: vec![],
            }
        } else {
            match capture {
                Some(img) => {
                    let ocr_lang = ocr_lang.clone();
                    let cache_dir = cache_dir.clone();
                    match tokio::task::spawn_blocking(move || {
                        ocr_reward_screen_engine(&img, &ocr_lang, &cache_dir, prefer_rapid)
                    })
                    .await
                    {
                        Ok(Ok(o)) => o,
                        Ok(Err(e)) => {
                            warn!("OCR failed: {e}");
                            OcrResult {
                                raw_lines: vec![],
                                slot_texts: vec![],
                            }
                        }
                        Err(e) => {
                            warn!("OCR task join failed: {e}");
                            OcrResult {
                                raw_lines: vec![],
                                slot_texts: vec![],
                            }
                        }
                    }
                }
                None => OcrResult {
                    raw_lines: vec![],
                    slot_texts: vec![],
                },
            }
        };

        let ocr_empty = ocr.slot_texts.iter().all(|t| t.trim().is_empty());
        // Retry once when OCR found nothing — but not after a successful visual pass.
        if ocr_empty && source == "eelog" && !skip_ocr {
            info!("OCR empty — retrying capture+OCR once");
            tokio::time::sleep(Duration::from_millis(320)).await;
            let capture_path = self.capture_path.clone();
            let monitor_owned = monitor.clone();
            let ocr_lang = ocr_lang.clone();
            let cache_dir = cache_dir.clone();
            match tokio::task::spawn_blocking(move || {
                let img = capture_for_rewards(monitor_owned.as_deref(), &capture_path)?;
                ocr_reward_screen_engine(&img, &ocr_lang, &cache_dir, prefer_rapid)
            })
            .await
            {
                Ok(Ok(o)) => {
                    info!("OCR retry done in {:?}", t0.elapsed());
                    ocr = o;
                }
                Ok(Err(e)) => warn!("OCR retry failed: {e}"),
                Err(e) => warn!("OCR retry join failed: {e}"),
            }
        }
        if !skip_ocr {
            info!("OCR done in {:?}", t0.elapsed());
        }

        let mut ocr_matches: Vec<&ItemRow> = Vec::new();
        for text in &ocr.slot_texts {
            if text.trim().is_empty() {
                continue;
            }
            for item in fuzzy_match_items(text, &catalog) {
                if ocr_matches.iter().any(|m| m.url_name == item.url_name) {
                    continue;
                }
                if ee_items.iter().any(|e| e.url_name == item.url_name) {
                    continue;
                }
                if ee_labels
                    .iter()
                    .any(|(label, _)| label_matches_item(label, item, prefer_ru))
                {
                    continue;
                }
                info!("OCR matched: {text:?} → {}", item.url_name);
                ocr_matches.push(item);
            }
        }
        if source == "visual" && !ocr_matches.is_empty() {
            self.visual_had_matches.store(true, Ordering::SeqCst);
        }
        // Fresh OCR findings only — visual slots merged below were already announced.
        let ocr_added = !ocr_matches.is_empty();

        // Carry the visual pass results into this snapshot so the EE run keeps
        // showing the whole squad instead of just the local reward.
        for item in &visual_items {
            if ocr_matches.iter().any(|m| m.url_name == item.url_name) {
                continue;
            }
            ocr_matches.push(item);
        }

        let mut slots = Vec::new();
        let mut matched_count = 0usize;

        for item in &ee_items {
            matched_count += 1;
            slots.push(self.slot_from_item(item, prefer_ru).await);
        }
        for (label, url) in &ee_labels {
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
        for item in &ocr_matches {
            if slots.len() >= 4 {
                break;
            }
            matched_count += 1;
            slots.push(self.slot_from_item(item, prefer_ru).await);
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

        rank_slots(&mut slots);
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

        self.state.push_reward(reward.clone()).await;

        if matched_count == 0 {
            warn!(
                "No catalog matches (source={source}); skipping overlay. OCR={:?} ee_paths={reward_paths:?}",
                ocr.slot_texts
            );
            return Ok(reward);
        }

        // Re-notify when OCR found extra squad rewards, or when there was no EE alert.
        if !early_announced || ocr_added {
            self.overlay.show_rewards(&reward).await?;
            self.state.emit(bngframe_core::AppEvent::OverlayShown {
                reward_id: reward.id.clone(),
            });
            info!(
                "OCR catch-up notify in {:?} ({} slots, ocr_added={ocr_added})",
                t0.elapsed(),
                reward.slots.len()
            );
        } else if let Err(e) = self.overlay.update_rewards(&reward).await {
            warn!("silent OCR update: {e}");
        } else {
            info!(
                "OCR done, no new slots — kept EE notify ({:?}, {} slots)",
                t0.elapsed(),
                reward.slots.len()
            );
        }

        // Live WFM prices in background — overlay only, no third notify.
        let urls: Vec<String> = reward
            .slots
            .iter()
            .filter_map(|s| s.matched_url_name.clone())
            .collect();
        if !urls.is_empty() {
            let pricing = self.pricing.clone();
            let overlay = self.overlay.clone();
            let state = self.state.clone();
            let mut reward_bg = reward.clone();
            tokio::spawn(async move {
                for slot in &mut reward_bg.slots {
                    let Some(ref url) = slot.matched_url_name else {
                        continue;
                    };
                    if let Ok(p) = pricing.price_for_refresh(url).await {
                        if p.platinum > 0.0 {
                            slot.platinum = Some(p.platinum);
                            slot.volume = Some(p.volume);
                        }
                    }
                }
                rank_slots(&mut reward_bg.slots);
                reward_bg.best_index = reward_bg
                    .slots
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, s)| s.rank.unwrap_or(99))
                    .map(|(i, _)| i);
                state.push_reward(reward_bg.clone()).await;
                if let Err(e) = overlay.update_rewards(&reward_bg).await {
                    warn!("price-refresh overlay: {e}");
                }
                info!("Overlay live prices refreshed (silent) for {}", reward_bg.id);
            });
        }

        Ok(reward)
    }

    async fn slot_from_item(&self, item: &ItemRow, prefer_ru: bool) -> RewardSlot {
        let mut platinum = None;
        let mut volume = None;
        // Forma is not traded on WFM — skip the orders call.
        if item.url_name != "forma" {
            match self.pricing.price_for_refresh(&item.url_name).await {
                Ok(p) if p.platinum > 0.0 => {
                    platinum = Some(p.platinum);
                    volume = Some(p.volume);
                }
                Ok(_) => {
                    // Empty book / 404 — fall back to last warm cache if any.
                    if let Some(p) = self.pricing.price_cached_only(&item.url_name).await {
                        if p.platinum > 0.0 {
                            platinum = Some(p.platinum);
                            volume = Some(p.volume);
                        }
                    }
                }
                Err(e) => {
                    warn!("live price for {}: {e}", item.url_name);
                    if let Some(p) = self.pricing.price_cached_only(&item.url_name).await {
                        if p.platinum > 0.0 {
                            platinum = Some(p.platinum);
                            volume = Some(p.volume);
                        }
                    }
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

/// Grab a couple of frames while the relic UI paints; stop as soon as one looks good.
async fn capture_reward_burst(
    capture_path: &std::path::Path,
    monitor: Option<&str>,
) -> Option<CapturedImage> {
    if !warframe_is_on_active_workspace() {
        warn!("Warframe not on active workspace at burst start — capturing anyway");
    }
    // Keep this short: each grim is ~0.8–1.4s. Two attempts beat four.
    // Absolute times from Got-rewards; early-exit when UI score is good.
    const DELAYS_MS: &[u64] = &[150, 400];
    const GOOD_ENOUGH: i32 = 20;
    let mut best: Option<(i32, CapturedImage)> = None;
    for (i, &delay) in DELAYS_MS.iter().enumerate() {
        if i == 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        } else {
            let prev = DELAYS_MS[i - 1];
            tokio::time::sleep(Duration::from_millis(delay.saturating_sub(prev))).await;
        }
        // Always write the working capture to last_capture — avoids extra disk churn.
        let path = capture_path.to_path_buf();
        let monitor_owned = monitor.map(|s| s.to_string());
        let frame = match tokio::task::spawn_blocking(move || {
            capture_for_rewards(monitor_owned.as_deref(), &path)
        })
        .await
        {
            Ok(Ok(img)) => img,
            Ok(Err(e)) => {
                warn!("burst capture {i} failed: {e}");
                continue;
            }
            Err(e) => {
                warn!("burst capture {i} join failed: {e}");
                continue;
            }
        };
        let score = reward_ui_score(&frame);
        info!("Burst frame {i} score={score}");
        let better = best.as_ref().map(|(s, _)| score > *s).unwrap_or(true);
        if better {
            best = Some((score, frame));
        }
        if score >= GOOD_ENOUGH {
            info!("Burst early-stop on frame {i} (score={score})");
            break;
        }
    }
    match best {
        Some((score, img)) if score > 0 => {
            info!("Selected burst frame score={score}");
            Some(img)
        }
        Some((score, img)) => {
            warn!("Burst frames never looked like relic UI (best score={score})");
            Some(img)
        }
        None => None,
    }
}
fn resolve_ee_rewards<'a>(
    reward_paths: &[String],
    catalog: &'a [ItemRow],
    lotus: &std::collections::HashMap<String, String>,
    prefer_ru: bool,
) -> (Vec<&'a ItemRow>, Vec<(String, Option<String>)>) {
    let mut ee_items = Vec::new();
    let mut ee_labels = Vec::new();
    for path in reward_paths {
        if let Some(item) = resolve_lotus_reward_path(path, catalog) {
            info!("EE path matched: {path} → {}", item.url_name);
            ee_items.push(item);
        } else if let Some(label) = resolve_lotus_reward_label(path, lotus, prefer_ru) {
            info!("EE path label (no WFM): {path} → {label}");
            ee_labels.push((label, None));
        } else {
            warn!("EE path unmatched: {path}");
        }
    }
    (ee_items, ee_labels)
}

fn label_matches_item(label: &str, item: &ItemRow, prefer_ru: bool) -> bool {
    let disp = display_name_for_item(item, prefer_ru).to_lowercase();
    let lab = label.to_lowercase();
    if disp == lab {
        return true;
    }
    // "Ортос Прайм: Клинок" vs catalog row
    fuzzy_match_item(label, std::slice::from_ref(item)).is_some()
}

fn rank_slots(slots: &mut [RewardSlot]) {
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
