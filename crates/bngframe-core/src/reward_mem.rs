//! Relic reward slots via read-only Warframe process memory (Proton).
//!
//! Diff-based string harvest: baseline StoreItems set vs live scan after
//! RewardScreenOpening. Squad paths are not in EE.log — only the local
//! `gets reward` line is. Deep needle scan + optional cluster around the
//! EE local path.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::inventory::find_warframe_pid;
use crate::pricing::is_plausible_reward_path;

/// ASCII needles — **StoreItems only**. `/Lotus/Types/Recipes/` is the inventory
/// catalog and floods diff with hundreds of false "new" Prime parts.
const PATH_NEEDLES: &[&[u8]] = &[
    b"/Lotus/StoreItems/Types/Recipes/",
    b"/Lotus/StoreItems/Types/Items/MiscItems/Forma",
    b"/Lotus/StoreItems/Types/Items/MiscItems/Endo",
];

#[derive(Debug, Clone, Copy)]
pub enum HarvestDepth {
    /// Rolling baseline / Opening freeze — visit all regions, shallow.
    Baseline,
    /// Prefetch polls — visit all regions, ~0.8s.
    Fast,
    /// Got enrichment when Fast is short — deeper per-region reads.
    Deep,
    /// Manual / last resort.
    Exhaustive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewardMemScan {
    pub pid: u32,
    pub all_paths: Vec<String>,
    pub new_paths: Vec<String>,
    /// Filtered candidates for squad slots (≤4).
    pub selected: Vec<String>,
    pub dump_path: Option<PathBuf>,
    pub regions_ok: u32,
    pub regions_fail: u32,
}

#[derive(Debug, Clone)]
struct PathHit {
    addr: u64,
    path: String,
}

pub struct RewardMemScanner {
    baseline: Mutex<HashSet<String>>,
    last_selected: Mutex<Vec<String>>,
}

impl Default for RewardMemScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl RewardMemScanner {
    pub fn new() -> Self {
        Self {
            baseline: Mutex::new(HashSet::new()),
            last_selected: Mutex::new(Vec::new()),
        }
    }

    pub fn last_selected(&self) -> Vec<String> {
        self.last_selected
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn clear_last_selected(&self) {
        if let Ok(mut g) = self.last_selected.lock() {
            g.clear();
        }
    }

    /// Refresh baseline StoreItems set (call while reward UI is not open, or
    /// immediately on Opening *before* GetVoidProjectionRewards returns).
    pub fn refresh_baseline(&self) -> Result<usize> {
        self.refresh_baseline_depth(HarvestDepth::Baseline)
    }

    pub fn refresh_baseline_depth(&self, depth: HarvestDepth) -> Result<usize> {
        let pid = find_warframe_pid()?;
        let (hits, _, _) = harvest_path_hits(pid, depth, None)?;
        let paths: HashSet<String> = hits.into_iter().map(|h| h.path).collect();
        let n = paths.len();
        *self.baseline.lock().unwrap_or_else(|e| e.into_inner()) = paths;
        debug!("reward_mem baseline refreshed ({n} paths, pid={pid}, depth={depth:?})");
        Ok(n)
    }

    pub fn baseline_len(&self) -> usize {
        self.baseline
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    pub fn scan(&self, cache_dir: Option<&Path>) -> Result<RewardMemScan> {
        self.scan_depth(HarvestDepth::Deep, cache_dir, &[])
    }

    /// Deep harvest + diff. `ee_hints` (local EE paths) pull nearby StoreItems
    /// neighbors into `selected` when the plain diff is thin.
    pub fn scan_depth(
        &self,
        depth: HarvestDepth,
        cache_dir: Option<&Path>,
        ee_hints: &[String],
    ) -> Result<RewardMemScan> {
        self.scan_depth_capped(depth, cache_dir, ee_hints, 4)
    }

    pub fn scan_depth_capped(
        &self,
        depth: HarvestDepth,
        cache_dir: Option<&Path>,
        ee_hints: &[String],
        max_slots: usize,
    ) -> Result<RewardMemScan> {
        let max_slots = max_slots.clamp(1, 4);
        let pid = find_warframe_pid().context("Warframe process not found")?;
        let baseline_snap: HashSet<String> = self
            .baseline
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        // Never early-stop on Got/Deep: stopping after a handful of "new" chrome
        // strings left all≈18 and missed squad StoreItems deeper in heaps.
        let (hits, regions_ok, regions_fail) = harvest_path_hits(pid, depth, None)?;

        let all_set: HashSet<String> = hits.iter().map(|h| h.path.clone()).collect();
        let mut new_paths: Vec<String> = all_set.difference(&baseline_snap).cloned().collect();
        new_paths.sort();

        let mut all_paths: Vec<String> = all_set.iter().cloned().collect();
        all_paths.sort();

        let mut selected = Vec::new();
        let noisy = new_paths.len() > (max_slots * 4).max(12);

        // EE neighborhood first — never trust a fat "new" catalog dump (Ambassador etc.).
        if !ee_hints.is_empty() {
            let clustered = harvest_ee_neighborhood(pid, ee_hints, max_slots)?;
            if !clustered.is_empty() {
                info!(
                    "reward_mem EE-neighborhood → {} path(s): {clustered:?}",
                    clustered.len()
                );
                selected = clustered;
            }
        }

        if !noisy {
            let from_diff = select_reward_paths(&new_paths, max_slots);
            for p in from_diff {
                if selected.len() >= max_slots {
                    break;
                }
                if !selected.iter().any(|s| s == &p) {
                    selected.push(p);
                }
            }
        } else {
            warn!(
                "reward_mem: noisy diff (new={}) — ignoring global select (catalog flood)",
                new_paths.len()
            );
        }

        let nearby = collect_nearby_store_items(pid, &hits, ee_hints, &new_paths)?;
        if !nearby.is_empty() {
            let mut extra: Vec<String> = nearby
                .into_iter()
                .filter(|p| is_confident_reward_path(p) || is_plain_forma(p))
                .filter(|p| !selected.iter().any(|s| s == p))
                .collect();
            // Only keep neighbors that are truly new (or the EE path itself).
            extra.retain(|p| !baseline_snap.contains(p) || ee_hints.iter().any(|e| e == p));
            // If the baseline was shallow, almost everything looks new — require
            // tight confidence and reject known chrome.
            for p in extra {
                if selected.len() >= max_slots {
                    break;
                }
                if !is_confident_reward_path(&p) {
                    continue;
                }
                if !selected.iter().any(|s| s == &p) {
                    info!("reward_mem nearby slot: {p}");
                    selected.push(p);
                }
            }
        }

        if !noisy && selected.len() < max_slots.saturating_sub(ee_hints.len().max(1)) {
            let clustered = cluster_around_hints(&hits, ee_hints, max_slots);
            if clustered.len() > selected.len() {
                info!(
                    "reward_mem cluster around EE hints → {} path(s)",
                    clustered.len()
                );
                selected = clustered;
            }
        }

        // Drop the local EE path from mem-selected — pipeline merges EE first.
        if !ee_hints.is_empty() {
            selected.retain(|p| !ee_hints.iter().any(|e| e == p));
        }
        // Final safety: never return more noise than party room.
        selected.retain(|p| is_confident_reward_path(p) || is_plain_forma(p));
        if selected.len() > max_slots {
            selected.truncate(max_slots);
        }

        if !selected.is_empty() {
            if let Ok(mut g) = self.last_selected.lock() {
                for p in &selected {
                    if !g.iter().any(|x| x == p) {
                        g.push(p.clone());
                    }
                }
                // Prefer confident paths; drop weak fillers if over capacity.
                g.retain(|p| is_confident_reward_path(p) || is_plain_forma(p));
                // Keep confident first.
                g.sort_by_key(|p| if is_confident_reward_path(p) { 0 } else { 1 });
                g.dedup();
                if g.len() > 4 {
                    g.truncate(4);
                }
            }
        }

        let dump_path = if let Some(dir) = cache_dir {
            // Prefetch dumps every poll are expensive; only dump on Got (hints) or growth.
            let should_dump = !ee_hints.is_empty()
                || (!selected.is_empty() && !new_paths.is_empty());
            if should_dump {
                match write_spike_dump(dir, pid, &all_paths, &new_paths, &selected) {
                    Ok(p) => Some(p),
                    Err(e) => {
                        warn!("reward_mem dump failed: {e:#}");
                        None
                    }
                }
            } else {
                None
            }
        } else {
            None
        };

        info!(
            "reward_mem scan pid={pid} depth={depth:?} all={} new={} selected={} max={max_slots} regions_ok={regions_ok} fail={regions_fail}",
            all_paths.len(),
            new_paths.len(),
            selected.len()
        );
        if !selected.is_empty() {
            info!("reward_mem selected: {selected:?}");
        }

        Ok(RewardMemScan {
            pid,
            all_paths,
            new_paths,
            selected,
            dump_path,
            regions_ok,
            regions_fail,
        })
    }

    pub fn debug_scan(&self, cache_dir: &Path) -> Result<RewardMemScan> {
        if self.baseline_len() == 0 {
            let _ = self.refresh_baseline_depth(HarvestDepth::Deep);
        }
        self.scan_depth(HarvestDepth::Deep, Some(cache_dir), &[])
    }
}

struct EarlyStop<'a> {
    #[allow(dead_code)]
    baseline: &'a HashSet<String>,
    #[allow(dead_code)]
    want_new: usize,
}

fn harvest_path_hits(
    pid: u32,
    depth: HarvestDepth,
    early: Option<EarlyStop<'_>>,
) -> Result<(Vec<PathHit>, u32, u32)> {
    let maps = std::fs::read_to_string(format!("/proc/{pid}/maps"))
        .with_context(|| format!("read /proc/{pid}/maps"))?;
    let mem_path = format!("/proc/{pid}/mem");
    // Probe open once so we fail fast with a clear ptrace error.
    let _probe = File::open(&mem_path).with_context(|| {
        format!(
            "open {mem_path} (need ptrace: sysctl kernel.yama.ptrace_scope=0 or setcap cap_sys_ptrace)"
        )
    })?;
    drop(_probe);

    let per_region: u64 = match depth {
        HarvestDepth::Baseline | HarvestDepth::Fast => 8 * 1024 * 1024,
        HarvestDepth::Deep => 24 * 1024 * 1024,
        HarvestDepth::Exhaustive => 64 * 1024 * 1024,
    };

    let mut regions: Vec<(u64, u64)> = Vec::new();
    for line in maps.lines() {
        if !line.contains("rw-p") {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }
        let pathname = parts.get(5).copied().unwrap_or("");
        if pathname.ends_with(".so")
            || pathname.contains("steam-runtime")
            || pathname.ends_with(".dll")
            || pathname.ends_with(".exe")
        {
            continue;
        }
        let range: Vec<&str> = parts[0].split('-').collect();
        if range.len() != 2 {
            continue;
        }
        let start = u64::from_str_radix(range[0], 16).unwrap_or(0);
        let end = u64::from_str_radix(range[1], 16).unwrap_or(0);
        if end <= start || end - start < 64 {
            continue;
        }
        regions.push((start, end));
    }
    // Smaller regions first — live UI buffers show up here; huge heaps are catalogs.
    regions.sort_by_key(|(start, end)| end - start);

    let thread_count = std::thread::available_parallelism()
        .map(|n| n.get().clamp(2, 6))
        .unwrap_or(4);
    // Round-robin across threads so each gets a mix of small UI heaps and large catalogs.
    let mut region_chunks: Vec<Vec<(u64, u64)>> = vec![Vec::new(); thread_count];
    for (i, r) in regions.into_iter().enumerate() {
        region_chunks[i % thread_count].push(r);
    }

    let (baseline, want_new) = match early {
        Some(e) => (Some(e.baseline.clone()), e.want_new),
        None => (None, usize::MAX),
    };
    let new_live = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    let mut handles = Vec::new();
    for chunk_regions in region_chunks {
        let mem_path = mem_path.clone();
        let baseline = baseline.clone();
        let new_live = new_live.clone();
        let stop = stop.clone();
        handles.push(std::thread::spawn(move || {
            let mut local: HashMap<String, u64> = HashMap::new();
            let mut ok = 0u32;
            let mut fail = 0u32;
            let mut bytes = 0u64;
            let mut mem = match File::open(&mem_path) {
                Ok(f) => f,
                Err(_) => return (local, 0, chunk_regions.len() as u32, 0u64),
            };
            for (start, end) in chunk_regions {
                if stop.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                let want = (end - start).min(per_region);
                let mut buf = vec![0u8; want as usize];
                if mem.seek(SeekFrom::Start(start)).is_err() {
                    fail += 1;
                    continue;
                }
                if mem.read_exact(&mut buf).is_err() {
                    fail += 1;
                    continue;
                }
                ok += 1;
                bytes += want;
                let mut batch = HashMap::new();
                scan_buf_for_store_items(&buf, start, &mut batch);
                for (path, addr) in batch {
                    let is_new_live = baseline
                        .as_ref()
                        .is_some_and(|b| !b.contains(&path) && is_live_reward_candidate(&path));
                    if local.insert(path, addr).is_none() && is_new_live {
                        let n = new_live.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                        if n >= want_new {
                            stop.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                }
            }
            (local, ok, fail, bytes)
        }));
    }

    let mut found: HashMap<String, u64> = HashMap::new();
    let mut regions_ok = 0u32;
    let mut regions_fail = 0u32;
    let mut bytes_read = 0u64;
    for h in handles {
        match h.join() {
            Ok((local, ok, fail, bytes)) => {
                regions_ok += ok;
                regions_fail += fail;
                bytes_read += bytes;
                for (p, addr) in local {
                    found.entry(p).or_insert(addr);
                }
            }
            Err(_) => {}
        }
    }

    // Fix early-stop counting: recompute unique new-live for logging accuracy.
    let new_live_n = if let Some(ref base) = baseline {
        found
            .keys()
            .filter(|p| !base.contains(*p) && is_live_reward_candidate(p))
            .count()
    } else {
        0
    };

    debug!(
        "reward_mem harvest depth={depth:?} bytes={bytes_read} regions_ok={regions_ok} fail={regions_fail} unique={} new_live≈{new_live_n} early={}",
        found.len(),
        stop.load(std::sync::atomic::Ordering::Relaxed)
    );

    if regions_ok == 0 && regions_fail > 0 {
        bail!(
            "could not read any rw-p regions for pid {pid} (ptrace blocked? regions_fail={regions_fail})"
        );
    }

    let mut hits: Vec<PathHit> = found
        .into_iter()
        .map(|(path, addr)| PathHit { addr, path })
        .collect();
    hits.sort_by_key(|h| h.addr);
    Ok((hits, regions_ok, regions_fail))
}

fn scan_buf_for_store_items(buf: &[u8], base: u64, found: &mut HashMap<String, u64>) {
    for needle in PATH_NEEDLES {
        let mut from = 0usize;
        while let Some(rel) = find_bytes(&buf[from..], needle) {
            let off = from + rel;
            if let Some(path) = extract_lotus_path(buf, off) {
                let path = normalize_lotus_path(&path);
                if path_looks_complete(&path) && is_plausible_reward_path(&path) {
                    found.entry(path).or_insert(base + off as u64);
                }
            }
            from = off + needle.len();
            if from >= buf.len() {
                break;
            }
        }
    }
}

/// Re-read ±512KB around EE / newly appeared paths for co-located StoreItems.
/// Always hunts EE leaves — catalog copies of the local path are often far from the UI table.
fn collect_nearby_store_items(
    pid: u32,
    hits: &[PathHit],
    ee_hints: &[String],
    new_paths: &[String],
) -> Result<Vec<String>> {
    let mut centers = Vec::new();
    for h in hits {
        let interesting = ee_hints.iter().any(|e| e == &h.path)
            || new_paths.iter().any(|n| n == &h.path)
            || is_confident_reward_path(&h.path);
        if interesting {
            centers.push(h.addr);
        }
    }
    for leaf_addr in find_leaf_addrs(pid, ee_hints)? {
        centers.push(leaf_addr);
    }
    if centers.is_empty() {
        return Ok(Vec::new());
    }
    centers.sort_unstable();
    centers.dedup_by(|a, b| a.abs_diff(*b) < 4096);

    let mem_path = format!("/proc/{pid}/mem");
    let mut mem = File::open(&mem_path)?;
    let mut found = HashSet::new();
    const RADIUS: u64 = 32 * 1024;
    for addr in centers {
        let start = addr.saturating_sub(RADIUS);
        let len = (RADIUS * 2) as usize;
        let mut buf = vec![0u8; len];
        if mem.seek(SeekFrom::Start(start)).is_err() {
            continue;
        }
        if mem.read_exact(&mut buf).is_err() {
            continue;
        }
        let mut local = HashMap::new();
        scan_buf_for_store_items(&buf, start, &mut local);
        found.extend(local.into_keys());
    }
    Ok(found.into_iter().collect())
}

/// Focused scan: find EE leaf hits, score windows by how many *other* confident
/// reward paths sit in a tight radius (UI table), not a 1MB catalog sweep.
fn harvest_ee_neighborhood(pid: u32, ee_hints: &[String], max: usize) -> Result<Vec<String>> {
    if ee_hints.is_empty() {
        return Ok(Vec::new());
    }
    let addrs = find_leaf_addrs(pid, ee_hints)?;
    if addrs.is_empty() {
        debug!("reward_mem: no EE leaf addresses in memory");
        return Ok(Vec::new());
    }
    info!("reward_mem: {} EE leaf hit(s) for neighborhood scan", addrs.len());
    let mut mem = File::open(format!("/proc/{pid}/mem"))?;
    let mut best: Vec<(i32, String)> = Vec::new();
    // Tight windows first — wide windows pull static recipe catalogs.
    const RADII: &[u64] = &[8 * 1024, 32 * 1024, 128 * 1024];
    for &radius in RADII {
        let mut scored: Vec<(i32, String)> = Vec::new();
        let mut seen = HashSet::new();
        for addr in &addrs {
            let start = addr.saturating_sub(radius);
            let len = (radius * 2) as usize;
            let mut buf = vec![0u8; len];
            if mem.seek(SeekFrom::Start(start)).is_err() {
                continue;
            }
            if mem.read_exact(&mut buf).is_err() {
                continue;
            }
            let mut local = HashMap::new();
            scan_buf_for_store_items(&buf, start, &mut local);
            let mut neighbors = 0i32;
            let mut batch = Vec::new();
            for (path, _) in local {
                if ee_hints.iter().any(|e| e == &path) {
                    continue;
                }
                if !is_confident_reward_path(&path) {
                    continue;
                }
                neighbors += 1;
                batch.push(path);
            }
            // A real squad table has 1–3 other cards near the local path, not dozens.
            if !(1..=6).contains(&neighbors) {
                continue;
            }
            for path in batch {
                if !seen.insert(path.clone()) {
                    continue;
                }
                scored.push((score_path(&path) + neighbors * 10, path));
            }
        }
        if scored.len() > best.len() {
            best = scored;
        }
        if best.len() >= max.saturating_sub(1) {
            break;
        }
    }
    best.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    Ok(dedupe_take(&best, max.saturating_sub(1).max(1)))
}

/// Find addresses of EE path leaf names (e.g. `CalibanPrimeChassisBlueprint`) in mem.
fn find_leaf_addrs(pid: u32, ee_hints: &[String]) -> Result<Vec<u64>> {
    if ee_hints.is_empty() {
        return Ok(Vec::new());
    }
    let mut needles: Vec<Vec<u8>> = Vec::new();
    for p in ee_hints {
        let leaf = p.rsplit('/').next().unwrap_or(p);
        if leaf.len() >= 8 {
            needles.push(leaf.as_bytes().to_vec());
        }
    }
    needles.sort();
    needles.dedup();
    if needles.is_empty() {
        return Ok(Vec::new());
    }

    let maps = std::fs::read_to_string(format!("/proc/{pid}/maps"))?;
    let mut mem = File::open(format!("/proc/{pid}/mem"))?;
    let mut addrs = Vec::new();
    // Prefer mid-size anon heaps — full maps walk at 8MB is enough for leaf hunt.
    let mut regions: Vec<(u64, u64)> = Vec::new();
    for line in maps.lines() {
        if !line.contains("rw-p") {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }
        let pathname = parts.get(5).copied().unwrap_or("");
        if pathname.ends_with(".so")
            || pathname.contains("steam-runtime")
            || pathname.ends_with(".dll")
            || pathname.ends_with(".exe")
        {
            continue;
        }
        let range: Vec<&str> = parts[0].split('-').collect();
        if range.len() != 2 {
            continue;
        }
        let start = u64::from_str_radix(range[0], 16).unwrap_or(0);
        let end = u64::from_str_radix(range[1], 16).unwrap_or(0);
        if end <= start {
            continue;
        }
        regions.push((start, end));
    }
    regions.sort_by_key(|(s, e)| e - s);

    for (start, end) in regions {
        if addrs.len() >= 24 {
            break;
        }
        let want = (end - start).min(8 * 1024 * 1024);
        let mut buf = vec![0u8; want as usize];
        if mem.seek(SeekFrom::Start(start)).is_err() {
            continue;
        }
        if mem.read_exact(&mut buf).is_err() {
            continue;
        }
        for n in &needles {
            let mut from = 0usize;
            let mut hits_here = 0u32;
            while let Some(rel) = find_bytes(&buf[from..], n) {
                addrs.push(start + (from + rel) as u64);
                from += rel + n.len();
                hits_here += 1;
                if hits_here >= 2 || addrs.len() >= 16 {
                    break;
                }
            }
        }
    }
    Ok(addrs)
}

fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    memchr::memmem::find(hay, needle)
}

fn normalize_lotus_path(path: &str) -> String {
    // DE dual form: Types ↔ StoreItems
    if let Some(rest) = path.strip_prefix("/Lotus/Types/Recipes/") {
        return format!("/Lotus/StoreItems/Types/Recipes/{rest}");
    }
    path.to_string()
}

fn extract_lotus_path(buf: &[u8], start: usize) -> Option<String> {
    if start >= buf.len() || buf[start] != b'/' {
        return None;
    }
    let mut end = start;
    while end < buf.len() {
        let c = buf[end];
        if c.is_ascii_alphanumeric() || matches!(c, b'/' | b'_' | b'.' | b'+' | b'-') {
            end += 1;
        } else {
            break;
        }
    }
    if end <= start + 16 {
        return None;
    }
    let mut s = String::from_utf8_lossy(&buf[start..end]).into_owned();
    while s.ends_with('/') || s.ends_with('.') {
        s.pop();
    }
    Some(s)
}

fn path_looks_complete(path: &str) -> bool {
    if path.is_empty() || path.contains("/null") {
        return false;
    }
    let leaf = path.rsplit('/').next().unwrap_or("");
    if leaf.len() < 6 {
        return false;
    }
    let l = leaf.to_ascii_lowercase();
    // Truncated / directory stubs from mem cuts.
    if matches!(
        l.as_str(),
        "weapon" | "weapons" | "recipes" | "components" | "warframerecipes" | "weaponparts"
    ) {
        return false;
    }
    // Reject mid-string cuts like "FormaBluepri" / "BurstonPrimeBarr"
    if l.contains("bluep") && !l.ends_with("blueprint") {
        return false;
    }
    if l.contains("prime") {
        let ok_end = [
            "blueprint", "barrel", "receiver", "stock", "blade", "handle", "grip", "string",
            "link", "helmet", "chassis", "systems", "neuroptics", "carapace", "cerebrum",
            "boot", "head", "hilt", "guard", "gauntlet", "lowerlimb", "upperlimb", "ornament",
            "disc", "pouch", "barrelblueprint", "receiverblueprint", "stockblueprint",
        ];
        if !ok_end.iter().any(|e| l.ends_with(e)) {
            return false;
        }
    }
    true
}

fn select_reward_paths(new_paths: &[String], max: usize) -> Vec<String> {
    let mut scored: Vec<(i32, String)> = new_paths
        .iter()
        .filter(|p| path_looks_complete(p))
        .filter(|p| is_live_reward_candidate(p))
        .map(|p| (score_path(p), p.clone()))
        .filter(|(s, _)| *s >= 50)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    // Prefer confident Prime/parts first; only then allow plain Forma to pad.
    let mut out = dedupe_take(
        &scored
            .iter()
            .filter(|(_, p)| is_confident_reward_path(p))
            .cloned()
            .collect::<Vec<_>>(),
        max,
    );
    if out.len() < max {
        for (_, p) in &scored {
            if out.len() >= max {
                break;
            }
            if is_plain_forma(p) && !out.iter().any(|x| x == p) {
                out.push(p.clone());
            }
        }
    }
    out
}

/// High-confidence fissure card (Prime part/BP). Used to decide if we still need Deep.
pub fn is_confident_reward_path(path: &str) -> bool {
    let l = path.to_ascii_lowercase();
    if !path_looks_complete(path) || is_static_noise(&l) {
        return false;
    }
    if l.contains("weaponparts") {
        return true;
    }
    if l.contains("prime") && l.contains("/recipes/") {
        return true;
    }
    false
}

fn is_plain_forma(path: &str) -> bool {
    let l = path.to_ascii_lowercase();
    (l.ends_with("/formablueprint") || l.ends_with("/miscitems/forma"))
        && !l.contains("formaaura")
        && !l.contains("formaomega")
        && !l.contains("formastance")
        && !l.contains("formaumbra")
}

pub fn is_plain_forma_path(path: &str) -> bool {
    is_plain_forma(path)
}

/// Live fissure rewards are almost always Prime parts/BPs, Forma, or Endo.
/// Static UI strings (Valkyr=Berserker set, non-prime rifles, plushies) must not win.
fn is_live_reward_candidate(path: &str) -> bool {
    let l = path.to_ascii_lowercase();
    if is_static_noise(&l) {
        return false;
    }
    if is_plain_forma(path) || l.contains("/endo") || l.ends_with("/endo") || l.contains("exilus")
    {
        return true;
    }
    // FormaAura / Omega / etc. are inventory chrome, not fissure cards.
    if l.contains("forma") {
        return false;
    }
    if l.contains("weaponparts") {
        return true;
    }
    if l.contains("prime") && l.contains("/recipes/") {
        return true;
    }
    false
}

fn is_static_noise(lower: &str) -> bool {
    // Valkyr internal name — always sits in recipe UI caches.
    if lower.contains("berserker") {
        return true;
    }
    const NOISE: &[&str] = &[
        "tennogreatsword",
        "burstonrifleblueprint",
        "goldspectre",
        "plush",
        "shipdeco",
        "incarnon",
        "distillpoints",
        "greedtoken",
        "orokincatalyst",
        "orokinreactorblueprint",
        "weaponutilityunlocker",
        "advanceducresourcedrone",
        "hunhowtrinket",
        "formaaura",
        "formaomega",
        "formastance",
        "formaumbra",
        "ambassador",
        "railjack",
        "sentinelrecipes",
        "nautilus",
        "necromech",
        "archwing",
        "cronusblueprint",
        "vorbolt",
    ];
    NOISE.iter().any(|n| lower.contains(n))
}

/// Paths within ±6KB of any EE hint occurrence — typically the squad table.
fn cluster_around_hints(hits: &[PathHit], ee_hints: &[String], max: usize) -> Vec<String> {
    if ee_hints.is_empty() || hits.is_empty() {
        return Vec::new();
    }
    let hint_leaves: HashSet<String> = ee_hints
        .iter()
        .map(|p| p.rsplit('/').next().unwrap_or(p).to_string())
        .collect();
    let mut centers = Vec::new();
    for h in hits {
        let leaf = h.path.rsplit('/').next().unwrap_or(&h.path);
        if hint_leaves.contains(leaf) || ee_hints.iter().any(|e| e == &h.path) {
            centers.push(h.addr);
        }
    }
    if centers.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(i32, String)> = Vec::new();
    let mut seen = HashSet::new();
    for h in hits {
        if !is_live_reward_candidate(&h.path) || !path_looks_complete(&h.path) {
            continue;
        }
        let near = centers.iter().any(|c| h.addr.abs_diff(*c) <= 256 * 1024);
        if !near {
            continue;
        }
        if !seen.insert(h.path.clone()) {
            continue;
        }
        scored.push((score_path(&h.path) + 30, h.path.clone()));
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    dedupe_take(&scored, max)
}

fn dedupe_take(scored: &[(i32, String)], max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut leaves = HashSet::new();
    for (_, p) in scored {
        let leaf = p.rsplit('/').next().unwrap_or(p);
        if !leaves.insert(leaf.to_string()) {
            continue;
        }
        out.push(p.clone());
        if out.len() >= max {
            break;
        }
    }
    out
}

fn score_path(path: &str) -> i32 {
    if !is_plausible_reward_path(path) {
        return 0;
    }
    let lower = path.to_ascii_lowercase();
    let mut s = 10;
    if lower.contains("/recipes/") {
        s += 30;
    }
    if lower.contains("warframerecipes")
        || lower.contains("/weapons/")
        || lower.contains("/components/")
        || lower.contains("weaponparts")
    {
        s += 15;
    }
    if lower.contains("prime") {
        s += 25;
    }
    if lower.contains("weaponparts") {
        s += 25;
    }
    if lower.ends_with("blueprint") {
        s += 20;
    }
    if lower.contains("helmet")
        || lower.contains("neuroptics")
        || lower.contains("chassis")
        || lower.contains("systems")
        || lower.contains("barrel")
        || lower.contains("receiver")
        || lower.contains("stock")
        || lower.contains("blade")
        || lower.contains("handle")
        || lower.contains("link")
        || lower.contains("grip")
        || lower.contains("string")
        || lower.contains("lowerlimb")
        || lower.contains("upperlimb")
        || lower.contains("carapace")
        || lower.contains("cerebrum")
    {
        s += 15;
    }
    // Forma is a real reward but common cache noise — below Prime parts.
    if lower.contains("forma") {
        s += 8;
    }
    if lower.contains("endo") || lower.contains("exilus") {
        s += 20;
    }
    if lower.contains("/mods/")
        || lower.contains("ability")
        || lower.contains("cosmetics")
        || lower.contains("shipdeco")
        || lower.contains("plush")
    {
        s -= 50;
    }
    if lower.contains("miscitems") && !(lower.contains("forma") || lower.contains("endo")) {
        s -= 20;
    }
    s
}

fn write_spike_dump(
    cache_dir: &Path,
    pid: u32,
    all: &[String],
    new: &[String],
    selected: &[String],
) -> Result<PathBuf> {
    fs::create_dir_all(cache_dir)?;
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = cache_dir.join(format!("reward_mem_{ts}.txt"));
    let mut body = String::new();
    body.push_str(&format!("pid={pid}\n"));
    body.push_str(&format!(
        "all={}\nnew={}\nselected={}\n\n",
        all.len(),
        new.len(),
        selected.len()
    ));
    body.push_str("=== selected ===\n");
    for p in selected {
        body.push_str(p);
        body.push('\n');
    }
    body.push_str("\n=== new ===\n");
    for p in new {
        body.push_str(p);
        body.push('\n');
    }
    body.push_str("\n=== all ===\n");
    for p in all {
        body.push_str(p);
        body.push('\n');
    }
    fs::write(&path, body)?;
    Ok(path)
}

/// Merge paths: **EE first** (local truth), then memory fills remaining slots up to `max_slots`.
pub fn merge_reward_paths(
    mem_paths: &[String],
    ee_paths: &[String],
    max_slots: usize,
) -> Vec<String> {
    let max_slots = max_slots.clamp(1, 4);
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for p in ee_paths.iter().chain(mem_paths.iter()) {
        let p = p.trim();
        if p.is_empty() || p.ends_with("/null") {
            continue;
        }
        if !seen.insert(p.to_string()) {
            continue;
        }
        out.push(p.to_string());
        if out.len() >= max_slots {
            break;
        }
    }
    out
}

pub fn path_leaf_counts(paths: &[String]) -> HashMap<String, usize> {
    let mut m = HashMap::new();
    for p in paths {
        let leaf = p.rsplit('/').next().unwrap_or(p).to_string();
        *m.entry(leaf).or_default() += 1;
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_path_from_buffer() {
        let buf = b"xx/Lotus/StoreItems/Types/Recipes/Weapons/LexPrimeBarrel\0yy";
        let off = find_bytes(buf, b"/Lotus/StoreItems/Types/Recipes/").unwrap();
        let p = extract_lotus_path(buf, off).unwrap();
        assert_eq!(p, "/Lotus/StoreItems/Types/Recipes/Weapons/LexPrimeBarrel");
    }

    #[test]
    fn select_prefers_new_blueprints() {
        let new = vec![
            "/Lotus/StoreItems/Types/Recipes/Weapons/LexPrimeBarrel".into(),
            "/Lotus/StoreItems/Types/Recipes/Weapons/LexPrimeReceiver".into(),
            "/Lotus/StoreItems/Types/Recipes/Components/FormaBlueprint".into(),
        ];
        let sel = select_reward_paths(&new, 4);
        assert!(sel.len() >= 2);
        assert!(sel.iter().any(|p| p.contains("Forma") || p.contains("Lex")));
        assert!(!sel.iter().any(|p| p.contains("Berserker")));
    }

    #[test]
    fn rejects_berserker_noise() {
        let new = vec![
            "/Lotus/StoreItems/Types/Recipes/WarframeRecipes/BerserkerChassisBlueprint".into(),
            "/Lotus/StoreItems/Types/Recipes/Weapons/KestrelPrimeBlueprint".into(),
            "/Lotus/StoreItems/Types/Recipes/Weapons/WeaponParts/BroncoPrimeReceiver".into(),
        ];
        let sel = select_reward_paths(&new, 4);
        assert!(sel.iter().all(|p| !p.contains("Berserker")));
        assert!(sel.iter().any(|p| p.contains("Bronco") || p.contains("Kestrel")));
    }

    #[test]
    fn noisy_diff_threshold() {
        // Simulate inventory flood — caller must not trust select on 50 identical-score parts.
        let mut new = Vec::new();
        for name in [
            "AkstilettoPrimeBarrel",
            "AstillaPrimeBarrel",
            "BratonPrimeReceiver",
            "BurstonPrimeReceiver",
            "CernosPrimeString",
            "LexPrimeBarrel",
        ] {
            new.push(format!(
                "/Lotus/StoreItems/Types/Recipes/Weapons/WeaponParts/{name}"
            ));
        }
        assert!(new.len() > 4);
        // Without EE cluster, picking any 4 from a flood is wrong — pipeline guards
        // with new.len() > max*4. Just ensure candidates are at least "live".
        assert!(new.iter().all(|p| is_live_reward_candidate(p)));
    }

    #[test]
    fn merge_ee_first() {
        let mem = vec![
            "/Lotus/StoreItems/Types/Recipes/WarframeRecipes/BerserkerChassisBlueprint".into(),
            "/Lotus/StoreItems/Types/Recipes/Components/FormaBlueprint".into(),
        ];
        let ee = vec![
            "/Lotus/StoreItems/Types/Recipes/Weapons/WeaponParts/BroncoPrimeReceiver".into(),
        ];
        let m = merge_reward_paths(&mem, &ee, 2);
        assert_eq!(m[0].contains("Bronco"), true);
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn merge_caps_four() {
        let mem: Vec<String> = (0..3)
            .map(|i| format!("/Lotus/StoreItems/Types/Recipes/Weapons/A{i}PrimeBlueprint"))
            .collect();
        let ee = vec![
            "/Lotus/StoreItems/Types/Recipes/Weapons/B0PrimeBlueprint".into(),
            "/Lotus/StoreItems/Types/Recipes/Weapons/B1PrimeBlueprint".into(),
        ];
        let m = merge_reward_paths(&mem, &ee, 4);
        assert_eq!(m.len(), 4);
        assert!(m[0].contains("B0"));
    }

    #[test]
    #[ignore]
    fn live_reward_mem_scan() {
        let scanner = RewardMemScanner::new();
        let n = scanner
            .refresh_baseline_depth(HarvestDepth::Deep)
            .expect("baseline");
        eprintln!("baseline paths={n}");
        let cache = dirs::cache_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
            .join("bngframe");
        let scan = scanner
            .scan_depth(HarvestDepth::Deep, Some(&cache), &[])
            .expect("scan");
        eprintln!(
            "all={} new={} selected={:?} dump={:?}",
            scan.all_paths.len(),
            scan.new_paths.len(),
            scan.selected,
            scan.dump_path
        );
        assert!(scan.regions_ok > 0);
        assert!(
            !scan.all_paths.is_empty(),
            "expected some StoreItems strings in memory"
        );
    }
}
