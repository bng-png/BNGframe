//! Mastery / set progress grouping for AlecaFrame-style SetCards.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::db::{Database, InventoryItem, ItemRow};
use crate::pricing::{
    alias_lotus_product_unique, build_lotus_leaf_index, canonicalize_set_slug,
    is_orderable_set_slug, normalize_name, resolve_lotus_label_indexed, strip_export_tags,
    wfm_thumb_url, CraftGearIng, PricingService,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetPartProgress {
    pub role: String,
    pub count: i64,
    /// How many are needed to craft (e.g. 2× Bronco Prime for Akbronco).
    #[serde(default = "default_part_required")]
    pub required: i64,
    pub url_name: Option<String>,
    pub name: Option<String>,
}

fn default_part_required() -> i64 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MasterySet {
    pub set_key: String,
    pub name: String,
    pub name_ru: Option<String>,
    pub category: String,
    pub thumb: Option<String>,
    pub thumb_url: Option<String>,
    pub vaulted: bool,
    pub owned: bool,
    pub mastered: bool,
    /// Helminth subsumed this warframe (ConsumedSuits).
    #[serde(default)]
    pub absorbed: bool,
    pub parts: Vec<SetPartProgress>,
    pub platinum: Option<f64>,
    pub ducats: Option<i64>,
    pub url_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MasterySetsResponse {
    pub sets: Vec<MasterySet>,
}

pub struct MasteryService {
    db: Arc<Mutex<Database>>,
    pricing: Arc<PricingService>,
}

impl MasteryService {
    pub fn new(db: Arc<Mutex<Database>>, pricing: Arc<PricingService>) -> Self {
        Self { db, pricing }
    }

    pub async fn list_sets(&self) -> Result<MasterySetsResponse> {
        let (inventory, catalog, prices, lotus, lotus_en, lotus_frames, absorbed_keys) = {
            let db = self.db.lock().await;
            let inventory = db.list_inventory()?;
            let catalog = db.all_items()?;
            let mut prices = HashMap::new();
            for p in db.list_all_prices()? {
                prices.insert(p.url_name, p.platinum);
            }
            // Later kinds overwrite — RU extras after EN so display prefers DE.
            let mut lotus = HashMap::new();
            for kind in [
                "mod",
                "arcane",
                "relic",
                "flavour",
                "weapon_en",
                "key",
                "drone",
                "gear",
                "resource",
                "custom",
                "sentinel",
                "warframe",
                "weapon",
            ] {
                if let Ok(m) = db.lotus_name_map(kind) {
                    lotus.extend(m);
                }
            }
            let lotus_en = db.lotus_name_map("weapon_en").unwrap_or_default();
            // Warframe kind only — excludes exalted/ability weapons that live under /Powersuits/.
            let lotus_frames = db.lotus_name_map("warframe").unwrap_or_default();
            let absorbed_keys = absorbed_set_keys_from_db(&db);
            (inventory, catalog, prices, lotus, lotus_en, lotus_frames, absorbed_keys)
        };

        let by_norm = PricingService::build_catalog_index(&catalog);
        let catalog_by_url: HashMap<&str, &ItemRow> =
            catalog.iter().map(|i| (i.url_name.as_str(), i)).collect();

        let craft_gear = self.pricing.craft_gear_map().await;
        let bp_product = self.pricing.weapon_blueprint_map().await;
        let lotus_by_leaf = build_lotus_leaf_index(&lotus);

        // Owned gear → set_key category (helps classify blueprints).
        let mut gear_cat: HashMap<String, String> = HashMap::new();
        for item in &inventory {
            if let Some((key, cat)) = owned_gear_mastery_cat(item) {
                let key =
                    public_set_key_for_item(item, &lotus, &lotus_en, &bp_product).unwrap_or(key);
                gear_cat.entry(key).or_insert(cat);
            }
        }

        let mut groups: HashMap<String, MasterySetBuilder> = HashMap::new();

        for item in &inventory {
            let (raw_key, role, category) = classify_inventory_item(item, &gear_cat);
            if raw_key.is_empty() || role == "skip" {
                continue;
            }
            let set_key =
                public_set_key_for_item(item, &lotus, &lotus_en, &bp_product).unwrap_or(raw_key);
            let entry = groups.entry(set_key.clone()).or_insert_with(|| {
                MasterySetBuilder::new(&set_key, &category)
            });
            // Prefer precise category from owned gear when set was created from a BP.
            if let Some(c) = gear_cat.get(&set_key) {
                if should_upgrade_mastery_category(&entry.category, c) {
                    entry.category = c.clone();
                }
            } else if should_upgrade_mastery_category(&entry.category, &category) {
                entry.category = category.clone();
            }

            if absorbed_keys.contains(&set_key) {
                entry.absorbed = true;
            }

            if role == "frame" || role == "weapon" {
                if item.count > 0 {
                    entry.owned = true;
                }
                if item.mastered {
                    entry.mastered = true;
                }
            }

            // Official Lotus display name (DE Export ru / WFCD)
            if let Some(n) =
                resolve_lotus_label_indexed(&item.unique_name, &lotus, &bp_product, &lotus_by_leaf)
            {
                apply_lotus_display_name(entry, &n, role == "frame" || role == "weapon");
            }

            // Resolve market row for thumb / vaulted / names
            let resolved = item
                .url_name
                .as_deref()
                .and_then(|u| catalog_by_url.get(u).copied())
                .or_else(|| {
                    PricingService::resolve_market_item_indexed(&item.name, &by_norm)
                        .and_then(|r| catalog_by_url.get(r.url_name.as_str()).copied())
                })
                .or_else(|| {
                    PricingService::resolve_market_item_indexed(&entry.display_name, &by_norm)
                        .and_then(|r| catalog_by_url.get(r.url_name.as_str()).copied())
                });

            if let Some(row) = resolved {
                apply_catalog_row(entry, row, &catalog_by_url, &prices, false);
            }

            if is_component_role(&role) {
                let part = entry
                    .parts
                    .entry(role.clone())
                    .or_insert(SetPartProgress {
                        role: role.clone(),
                        count: 0,
                        required: 1,
                        url_name: item.url_name.clone(),
                        name: Some(item.name.clone()),
                    });
                part.count += item.count;
                if part.url_name.is_none() {
                    part.url_name = item.url_name.clone();
                }
            }
        }

        // Helminth-only frames (subsumed, no longer owned)
        for key in &absorbed_keys {
            let entry = groups.entry(key.clone()).or_insert_with(|| {
                MasterySetBuilder::new(key, "warframe")
            });
            entry.absorbed = true;
        }

        // Prime sentinels etc. that exist on WFM but have no inventory rows yet
        seed_companion_sets_from_catalog(&mut groups, &catalog, &catalog_by_url, &prices);
        // Full companion roster from DE Export (kubrows, kavats, vulpas, predasites, …)
        seed_companions_from_lotus(&mut groups, &lotus, &lotus_en);
        // Full warframe / archwing / weapon roster + remaining WFM sets
        seed_warframes_from_lotus(&mut groups, &lotus_frames, &lotus_en);
        seed_weapons_from_lotus(&mut groups, &lotus, &lotus_en);
        seed_market_sets_from_catalog(&mut groups, &catalog, &catalog_by_url, &prices);

        let slug_ru = build_slug_ru_map(&lotus, &lotus_en, &bp_product);
        // Final catalog resolve + Russian labels
        for entry in groups.values_mut() {
            resolve_set_catalog(entry, &by_norm, &catalog_by_url, &prices);
            apply_slug_russian_label(entry, &slug_ru);
            finalize_set_market(entry, &catalog, &catalog_by_url, &prices);
            fill_slots_from_catalog(entry, &catalog);
            apply_slug_russian_label(entry, &slug_ru);
        }

        // Akimbo / dual crafts: whole weapons as ingredients (2× Bronco → Akbronco, …)
        if !craft_gear.is_empty() {
            for entry in groups.values_mut() {
                fill_craft_gear_components(
                    entry,
                    &craft_gear,
                    &inventory,
                    &lotus,
                    &by_norm,
                    &catalog_by_url,
                );
            }
        }

        let mut sets: Vec<MasterySet> = groups
            .into_values()
            .map(|b| b.finish())
            .filter(keep_mastery_set)
            .collect();
        sets.sort_by(|a, b| {
            let an = a.name_ru.as_deref().unwrap_or(&a.name);
            let bn = b.name_ru.as_deref().unwrap_or(&b.name);
            an.to_lowercase().cmp(&bn.to_lowercase())
        });

        Ok(MasterySetsResponse { sets })
    }
}

struct MasterySetBuilder {
    set_key: String,
    display_name: String,
    name_ru: Option<String>,
    category: String,
    thumb: Option<String>,
    vaulted: bool,
    owned: bool,
    mastered: bool,
    absorbed: bool,
    parts: HashMap<String, SetPartProgress>,
    platinum: Option<f64>,
    ducats: Option<i64>,
    url_name: Option<String>,
}

impl MasterySetBuilder {
    fn new(set_key: &str, category: &str) -> Self {
        Self {
            set_key: set_key.into(),
            display_name: humanize_set_key(set_key),
            name_ru: None,
            category: category.into(),
            thumb: None,
            vaulted: false,
            owned: false,
            mastered: false,
            absorbed: false,
            parts: HashMap::new(),
            platinum: None,
            ducats: None,
            url_name: None,
        }
    }

    fn finish(self) -> MasterySet {
        let category = match self.category.as_str() {
            "prime" | "weapon_prime" | "weapon" => "primary".into(),
            other => other.to_string(),
        };
        let prefer = part_role_order(&category);
        let mut parts: Vec<SetPartProgress> = Vec::new();
        for role in &prefer {
            if let Some(p) = self.parts.get(*role) {
                parts.push(p.clone());
            }
        }
        for (k, v) in &self.parts {
            if !prefer.iter().any(|r| r == &k.as_str()) {
                parts.push(v.clone());
            }
        }
        // Warframes always show the classic 4 slots
        if category == "warframe" {
            for role in ["blueprint", "neuroptics", "chassis", "systems"] {
                if !parts.iter().any(|p| p.role == role) {
                    parts.push(SetPartProgress {
                        role: role.into(),
                        count: 0,
                        required: 1,
                        url_name: None,
                        name: None,
                    });
                }
            }
            parts.sort_by_key(|p| {
                prefer
                    .iter()
                    .position(|r| *r == p.role.as_str())
                    .unwrap_or(99)
            });
        }
        // Archwing suits: blueprint + harness + wings + systems
        // (archguns/archmelee stay in this category but keep weapon part slots)
        if category == "archwing" {
            let has_weapon_parts = self.parts.keys().any(|r| {
                matches!(
                    r.as_str(),
                    "barrel"
                        | "receiver"
                        | "stock"
                        | "blade"
                        | "handle"
                        | "head"
                        | "link"
                        | "grip"
                        | "string"
                )
            });
            if !has_weapon_parts {
                for role in ["blueprint", "harness", "wings", "systems"] {
                    if !parts.iter().any(|p| p.role == role) {
                        parts.push(SetPartProgress {
                            role: role.into(),
                            count: 0,
                            required: 1,
                            url_name: None,
                            name: None,
                        });
                    }
                }
                parts.sort_by_key(|p| {
                    prefer
                        .iter()
                        .position(|r| *r == p.role.as_str())
                        .unwrap_or(99)
                });
            }
        }

        let thumb_url = self.thumb.as_ref().map(|t| wfm_thumb_url(t));
        MasterySet {
            set_key: self.set_key,
            name: self.display_name,
            name_ru: self.name_ru,
            category,
            thumb: self.thumb,
            thumb_url,
            vaulted: self.vaulted,
            owned: self.owned,
            mastered: self.mastered,
            absorbed: self.absorbed,
            parts,
            platinum: self.platinum,
            ducats: self.ducats,
            url_name: self.url_name,
        }
    }
}

/// Returns (set_key, role, category)
fn classify_inventory_item(
    item: &InventoryItem,
    gear_cat: &HashMap<String, String>,
) -> (String, String, String) {
    let unique = item.unique_name.split('#').next().unwrap_or(&item.unique_name);
    let lower = unique.to_lowercase();
    let name_l = item.name.to_lowercase();
    let url_l = item.url_name.as_deref().unwrap_or("").to_lowercase();

    // Skip junk / cosmetics / non-mastery noise
    if is_non_masterable_item(item)
        || lower.contains("shipdeco")
        || lower.contains("flavour")
        || lower.contains("emote")
        || lower.contains("augmentcard")
        || lower.contains("warframeskins")
        || lower.contains("operatorarmour")
        || lower.contains("landingcraft")
        || lower.contains("abilityoverrides")
        || lower.contains("entratimech")
        || lower.contains("geneticsignature")
        || lower.contains("geneticcode")
        || lower.contains("animaltag")
        || lower.contains("/recipes/components/")
        || lower.contains("resourcedrone")
        || lower.contains("keyblueprint")
        || name_l.contains("buff")
        || name_l.contains("debuff")
        || name_l.contains("quest")
        || name_l.contains("sequencer")
        || name_l.contains("resource drone")
        || (name_l.contains("drone") && !lower.contains("powersuit") && !lower.contains("sentinel"))
        || name_l.contains("alloy")
        || name_l.contains("cipher")
        || (name_l.contains("gem") && (name_l.contains("cut") || name_l.contains("ore")))
        || name_l.contains(" ore ")
        || name_l.ends_with(" ore")
        || lower.contains("/gems/")
        || lower.contains("fishparts")
        || lower.contains("miscitems/alertium")
        // Cosmetic helmets only — WarframeRecipes *Helmet* is neuroptics.
        || (lower.contains("helmet") && !lower.contains("warframerecipes"))
        || (name_l.contains("helmet") && !lower.contains("warframerecipes"))
        || (name_l.contains("шлем") && !lower.contains("warframerecipes"))
        || name_l.contains("augment")
        || name_l.contains("animal tag")
        || name_l.contains("жетон")
        || name_l.contains("spectre")
        || item.item_type == "decoration"
        || item.item_type == "flavour"
        || item.item_type == "mod"
        || item.item_type == "arcane"
        || item.item_type == "relic"
        || item.item_type == "consumable"
    {
        return (String::new(), "skip".into(), String::new());
    }

    // Owned / crafted gear — track owned + mastered on the set card
    if let Some((key, cat)) = owned_gear_mastery_cat(item) {
        let role = if cat == "warframe" || cat == "archwing" || cat == "companion" {
            "frame"
        } else {
            "weapon"
        };
        return (key, role.into(), cat);
    }

    // Warframe component blueprints / crafted parts
    if lower.contains("warframerecipes") {
        // Beacons / quest junk — not craft slots. DE calls neuroptics "Helmet".
        if lower.contains("beacon") {
            return (String::new(), "skip".into(), String::new());
        }
        let role = role_from_slug(&url_l)
            .map(|s| s.to_string())
            .unwrap_or_else(|| part_role_from(&lower, &name_l));
        let role = if role == "other" && (lower.contains("helmet") || name_l.contains("helmet")) {
            "neuroptics".into()
        } else {
            role
        };
        if role == "other" {
            return (String::new(), "skip".into(), String::new());
        }
        let leaf = unique.rsplit('/').next().unwrap_or(unique);
        let public = warframe_public_leaf(leaf);
        let key = camel_to_set_key(public);
        return (key, role, "warframe".into());
    }

    // Archwing recipes / parts (before prime component catch-all — "Prime Archwing Systems")
    if lower.contains("archwingrecipes")
        || (lower.contains("/recipes/") && lower.contains("archwing"))
    {
        let role = role_from_slug(&url_l)
            .map(|s| s.to_string())
            .unwrap_or_else(|| part_role_from(&lower, &name_l));
        if role == "other" {
            return (String::new(), "skip".into(), String::new());
        }
        let leaf = unique.rsplit('/').next().unwrap_or(unique);
        let public = archwing_recipe_public_leaf(leaf);
        let key = camel_to_set_key(&public);
        return (key, role, "archwing".into());
    }

    if name_l.contains("prime")
        && !lower.contains("archwing")
        && (name_l.contains("neuro")
            || name_l.contains("chassis")
            || name_l.contains("systems")
            || name_l.contains("каркас")
            || name_l.contains("систем")
            || name_l.contains("нейро"))
    {
        let role = role_from_slug(&url_l)
            .map(|s| s.to_string())
            .unwrap_or_else(|| part_role_from(&lower, &name_l));
        if role == "other" {
            return (String::new(), "skip".into(), String::new());
        }
        let key = leaf_set_key(unique);
        return (key, role, "warframe".into());
    }

    // Companion recipes (sentinels, pets, …)
    if lower.contains("sentinelrecipes")
        || lower.contains("/recipes/kubrow")
        || lower.contains("zanukapet")
        || lower.contains("moapet")
        || (lower.contains("/recipes/")
            && (lower.contains("sentinel")
                || lower.contains("kubrow")
                || lower.contains("kavat")
                || lower.contains("moa")
                || lower.contains("predasite")
                || lower.contains("vulpaphyla")
                || lower.contains("helminthcharger")))
    {
        let role = role_from_slug(&url_l)
            .map(|s| s.to_string())
            .unwrap_or_else(|| part_role_from(&lower, &name_l));
        let role = if role == "other" {
            "blueprint".into()
        } else {
            role
        };
        let leaf = unique.rsplit('/').next().unwrap_or(unique);
        // CompleteHeadB blueprints share the HeadB part set
        let key = if leaf.contains("ZanukaPetCompleteHead") {
            if let Some(alias) = alias_lotus_product_unique(unique) {
                leaf_set_key(&alias)
            } else {
                leaf_set_key(unique)
            }
        } else {
            leaf_set_key(unique)
        };
        return (key, role, "companion".into());
    }

    // Modular parts / amps / zaws / kitguns
    if is_modular_path(&lower) {
        let role = role_from_slug(&url_l)
            .map(|s| s.to_string())
            .unwrap_or_else(|| part_role_from(&lower, &name_l));
        let role = if role == "other" {
            "blueprint".into()
        } else {
            role
        };
        let key = leaf_set_key(unique);
        return (key, role, "modular".into());
    }

    // Weapon blueprints / parts (prime and non-prime)
    if lower.contains("/recipes/weapons/")
        || lower.contains("weaponparts")
        || (item.item_type == "blueprint" && looks_like_weapon_blueprint(&lower, &name_l, &url_l))
        || item.item_type == "part"
    {
        if name_l.contains("helmet") || lower.contains("helmet") {
            return (String::new(), "skip".into(), String::new());
        }
        let role = role_from_slug(&url_l)
            .map(|s| s.to_string())
            .unwrap_or_else(|| part_role_from(&lower, &name_l));
        let role = if role == "other" {
            "blueprint".into()
        } else {
            role
        };
        let key = leaf_set_key(unique);
        // ClanTech / player weapons must not inherit "companion" from a sentinel
        // that shares the same leaf (LaserRifle → sentinel vs Flux Rifle BP).
        let cat = weapon_or_companion_category(&lower, &name_l, &item.item_type, &key, gear_cat);
        if !is_mastery_type_category(&cat) || cat == "warframe" {
            return (
                key,
                role,
                infer_weapon_slot_category(&lower, &name_l).into(),
            );
        }
        return (key, role, cat);
    }

    // Generic blueprint with a known mastery category
    if item.item_type == "blueprint" {
        if let Some(_) = mastery_category_from_paths(&lower, &name_l, &item.item_type) {
            let role = role_from_slug(&url_l)
                .map(|s| s.to_string())
                .unwrap_or_else(|| part_role_from(&lower, &name_l));
            let role = if role == "other" {
                "blueprint".into()
            } else {
                role
            };
            let key = leaf_set_key(unique);
            let cat = weapon_or_companion_category(&lower, &name_l, &item.item_type, &key, gear_cat);
            return (key, role, cat);
        }
    }

    (String::new(), "skip".into(), String::new())
}

fn owned_gear_mastery_cat(item: &InventoryItem) -> Option<(String, String)> {
    let unique = item.unique_name.split('#').next().unwrap_or(&item.unique_name);
    let lower = unique.to_lowercase();

    // Mods/precepts are often stored as misc in DB — never treat as masterable gear.
    if is_non_masterable_item(item) {
        return None;
    }

    if lower.contains("shipdeco")
        || lower.contains("flavour")
        || lower.contains("emote")
        || lower.contains("augment")
        || (lower.contains("helmet") && !lower.contains("warframerecipes"))
        || lower.contains("entratimech")
        || lower.contains("/recipes/")
        || lower.contains("blueprint")
    {
        return None;
    }

    if matches!(
        item.item_type.as_str(),
        "blueprint" | "part" | "mod" | "relic" | "arcane" | "decoration" | "flavour" | "consumable"
    ) {
        return None;
    }

    let cat = match item.item_type.as_str() {
        // Venari lives under /Powersuits/…/Kavat/ but is a companion, not a warframe.
        _ if is_companion_path(&lower) && (lower.contains("powersuit") || lower.contains("kavat")) => {
            "companion"
        }
        "warframe" if lower.contains("/archwing/") => "archwing",
        "warframe" => "warframe",
        "primary" => "primary",
        "secondary" => "secondary",
        "melee" => "melee",
        "sentinel" => "companion",
        "archwing" => "archwing",
        "weapon" if lower.contains("/archwing/") => "archwing",
        "weapon" | "misc" => {
            mastery_category_from_paths(&lower, &item.name.to_lowercase(), item.item_type.as_str())?
        }
        _ => return None,
    };

    // Only companion/modular misc paths, not every misc row
    if item.item_type == "misc"
        && !matches!(cat, "companion" | "modular" | "archwing")
    {
        return None;
    }

    let key = gear_set_key(unique, &lower, cat);
    if key.is_empty() {
        return None;
    }
    Some((key, cat.into()))
}

/// Items that do not grant Mastery Rank when ranked (mods, precepts, cosmetics, …).
fn is_non_masterable_item(item: &InventoryItem) -> bool {
    let u = item.unique_name.to_lowercase();
    let bare = u.split('#').next().unwrap_or(&u);
    let leaf = bare.rsplit('/').next().unwrap_or(bare);
    matches!(
        item.item_type.as_str(),
        "mod" | "arcane" | "relic" | "flavour" | "decoration" | "consumable"
    ) || u.contains("#rawupgrades")
        || bare.contains("precept")
        || bare.contains("/upgrades/")
        || bare.contains("meleetrees")
        || bare.contains("/cosmeticenhancers/")
        || bare.contains("/avatarimages/")
        || bare.contains("/titles/")
        // Exalted / ability weapons (Exalted Blade, Regulators, …) do not grant MR.
        || is_exalted_ability_weapon(bare, leaf)
}

fn gear_set_key(unique: &str, lower: &str, cat: &str) -> String {
    let leaf = unique.rsplit('/').next().unwrap_or(unique);
    if cat == "archwing" && (lower.contains("/powersuits/") || lower.contains("jetpack")) {
        return camel_to_set_key(archwing_public_leaf(leaf));
    }
    if cat == "warframe" {
        let public = warframe_public_leaf(leaf);
        return camel_to_set_key(public);
    }
    if cat == "companion" {
        if let Some(public) = companion_public_leaf(leaf) {
            return camel_to_set_key(public);
        }
    }
    leaf_set_key(unique)
}

fn companion_public_leaf(leaf: &str) -> Option<&'static str> {
    match leaf {
        "ArcDronePowerSuit" => Some("Diriga"),
        "GubberPowerSuit" => Some("Djinn"),
        "MeleePetPowerSuit" => Some("Helios"),
        "PrimeHeliosPowerSuit" => Some("HeliosPrime"),
        "RadarPowerSuit" => Some("Oxylus"),
        "TnSentinelCrossPowerSuit" => Some("Taxon"),
        "EmpyreanSentinelPowerSuit" => Some("Nautilus"),
        "NautilusPrimeSentinelPowerSuit" => Some("NautilusPrime"),
        "CheshireCatbrowPetPowerSuit" => Some("SmeetaKavat"),
        "MirrorCatbrowPetPowerSuit" => Some("AdarzaKavat"),
        "VampireCatbrowPetPowerSuit" => Some("VascaKavat"),
        "RetrieverKubrowPetPowerSuit" => Some("ChesaKubrow"),
        "FurtiveKubrowPetPowerSuit" => Some("HurasKubrow"),
        "GuardKubrowPetPowerSuit" => Some("RaksaKubrow"),
        "AdventurerKubrowPetPowerSuit" => Some("SahasaKubrow"),
        "HunterKubrowPetPowerSuit" => Some("SunikaKubrow"),
        "ChargerKubrowPetPowerSuit" => Some("HelminthCharger"),
        "ArmoredInfestedCatbrowPetPowerSuit" => Some("PanzerVulpaphyla"),
        "HornedInfestedCatbrowPetPowerSuit" => Some("CrescentVulpaphyla"),
        "VulpineInfestedCatbrowPetPowerSuit" => Some("SlyVulpaphyla"),
        "MedjayPredatorKubrowPetPowerSuit" => Some("MedjayPredasite"),
        "PharaohPredatorKubrowPetPowerSuit" => Some("PharaohPredasite"),
        "VizierPredatorKubrowPetPowerSuit" => Some("VizierPredasite"),
        "KhoraKavatPowerSuit" => Some("Venari"),
        "KhoraPrimeKavatPowerSuit" => Some("VenariPrime"),
        "ZanukaPetAPowerSuit" => Some("Dorma"),
        "ZanukaPetBPowerSuit" => Some("Bhaira"),
        "ZanukaPetCPowerSuit" => Some("Hec"),
        _ => None,
    }
}

/// Prefer public market/wiki slug (Acceltra → acceltra) over internal Lotus leaves.
fn public_set_key_for_item(
    item: &InventoryItem,
    lotus: &HashMap<String, String>,
    lotus_en: &HashMap<String, String>,
    bp_product: &HashMap<String, String>,
) -> Option<String> {
    let unique = crate::pricing::strip_inventory_unique(&item.unique_name);
    let lower = unique.to_lowercase();
    let leaf = unique.rsplit('/').next().unwrap_or(unique);

    if lower.contains("/archwing/") && (lower.contains("jetpack") || lower.contains("/powersuits/"))
    {
        let key = camel_to_set_key(archwing_public_leaf(leaf));
        return (!key.is_empty()).then_some(key);
    }
    if lower.contains("archwingrecipes")
        || (lower.contains("/recipes/") && lower.contains("archwing"))
    {
        let key = camel_to_set_key(&archwing_recipe_public_leaf(leaf));
        return (!key.is_empty()).then_some(key);
    }
    if lower.contains("/powersuits/") || lower.contains("warframerecipes") {
        // Venari etc.
        if is_companion_path(&lower) {
            if let Some(public) = companion_public_leaf(leaf) {
                let key = camel_to_set_key(public);
                return (!key.is_empty()).then_some(key);
            }
        }
        let key = camel_to_set_key(warframe_public_leaf(leaf));
        return (!key.is_empty()).then_some(key);
    }
    if is_companion_path(&lower) {
        if let Some(public) = companion_public_leaf(leaf) {
            let key = camel_to_set_key(public);
            return (!key.is_empty()).then_some(key);
        }
    }

    // Blueprint → finished product unique → English public name
    let product_unique = bp_product
        .get(unique)
        .map(|s| s.as_str())
        .unwrap_or(unique);
    if let Some(key) = en_label_to_set_key(lotus_en.get(product_unique).map(|s| s.as_str())) {
        return Some(key);
    }
    if let Some(key) = en_label_to_set_key(lotus_en.get(unique).map(|s| s.as_str())) {
        return Some(key);
    }

    // Weapon part BP (NokkoArchGunBarrel) → parent weapon EN (Arbucep)
    if let Some(key) = parent_weapon_public_set_key(unique, lotus_en, bp_product) {
        return Some(key);
    }

    // Prefer English WFCD label for stable set_key / wiki / WFM matching
    if let Some(en) = lotus_en.get(unique).or_else(|| {
        lotus
            .get(unique)
            .filter(|n| !looks_russian(n) && !strip_export_tags(n).is_empty())
    }) {
        if let Some(key) = en_label_to_set_key(Some(en.as_str())) {
            return Some(key);
        }
    }
    None
}

fn en_label_to_set_key(en: Option<&str>) -> Option<String> {
    let cleaned = strip_export_tags(en?);
    if cleaned.is_empty() || looks_russian(&cleaned) {
        return None;
    }
    // "Arbucep Barrel" / "Arbucep: Barrel" → arbucep
    let parent = strip_weapon_part_label(&cleaned);
    let key = normalize_name(&parent).replace(' ', "_");
    (!key.is_empty()).then_some(key)
}

fn strip_weapon_part_label(name: &str) -> String {
    let mut s = name.trim().to_string();
    if let Some((head, _)) = s.split_once(':') {
        s = head.trim().to_string();
    }
    for suf in [
        " Barrel",
        " Receiver",
        " Reciever",
        " Stock",
        " Blade",
        " Handle",
        " Link",
        " Grip",
        " String",
        " Gauntlet",
        " Pouch",
        " Chain",
        " Hilt",
        " Guard",
        " Head",
        " Neuroptics",
        " Chassis",
        " Systems",
        " Harness",
        " Wings",
        " Carapace",
        " Cerebrum",
        " Blueprint",
    ] {
        if let Some(stripped) = s.strip_suffix(suf) {
            if !stripped.is_empty() {
                s = stripped.trim_end().to_string();
            }
        }
    }
    s
}

/// NokkoArchGunBarrelBlueprint → look up EN name for NokkoArchGun product.
fn parent_weapon_public_set_key(
    unique: &str,
    lotus_en: &HashMap<String, String>,
    bp_product: &HashMap<String, String>,
) -> Option<String> {
    let leaf = unique.rsplit('/').next().unwrap_or(unique);
    let mut parent_leaf = leaf.to_string();
    for suf in [
        "Blueprint",
        "Item",
        "Component",
        "Neuroptics",
        "Chassis",
        "Systems",
        "Harness",
        "Wings",
        "Receiver",
        "Barrel",
        "Stock",
        "Blade",
        "Handle",
        "Link",
        "Grip",
        "String",
        "Gauntlet",
        "Pouch",
        "Chain",
        "Hilt",
        "Guard",
        "Head",
        "Carapace",
        "Cerebrum",
        "LowerLimb",
        "UpperLimb",
    ] {
        if let Some(stripped) = parent_leaf.strip_suffix(suf) {
            if !stripped.is_empty() {
                parent_leaf = stripped.to_string();
            }
        }
    }
    if parent_leaf.is_empty() || parent_leaf == leaf {
        // Still try bp map on stripped blueprint path
    }

    // Direct product path from main blueprint map
    for bp in [
        format!("/Lotus/Types/Recipes/Weapons/{parent_leaf}Blueprint"),
        format!("/Lotus/Types/Recipes/SentinelRecipes/{parent_leaf}Blueprint"),
        format!("/Lotus/Types/Recipes/SentinelRecipes/{parent_leaf}SentinelBlueprint"),
    ] {
        if let Some(prod) = bp_product.get(&bp) {
            if let Some(key) = en_label_to_set_key(lotus_en.get(prod).map(|s| s.as_str())) {
                return Some(key);
            }
        }
    }

    // Match any EN weapon/sentinel whose leaf equals parent (…/NokkoArchGun,
    // EmpyreanSentinelPowerSuit ← EmpyreanSentinel).
    for (path, en) in lotus_en {
        let pleaf = path.rsplit('/').next().unwrap_or(path.as_str());
        let matches = pleaf == parent_leaf
            || pleaf.strip_suffix("PowerSuit") == Some(parent_leaf.as_str())
            || pleaf.strip_suffix("Weapon") == Some(parent_leaf.as_str());
        if matches {
            if let Some(key) = en_label_to_set_key(Some(en.as_str())) {
                return Some(key);
            }
        }
    }
    None
}

fn apply_lotus_display_name(entry: &mut MasterySetBuilder, raw: &str, prefer: bool) {
    let cleaned = strip_export_tags(raw);
    if cleaned.is_empty() {
        return;
    }
    // Part labels ("Арбуцеп: Ствол") must not rename the whole set card.
    if cleaned.contains(':') && !prefer {
        return;
    }
    if looks_russian(&cleaned) {
        if prefer || entry.name_ru.as_ref().is_none_or(|r| !looks_russian(r)) {
            entry.name_ru = Some(cleaned.clone());
        }
        if prefer || entry.display_name == humanize_set_key(&entry.set_key) {
            entry.display_name = cleaned;
        }
        return;
    }
    // English (WFCD): use as display, never as name_ru
    if prefer || entry.display_name == humanize_set_key(&entry.set_key) {
        entry.display_name = cleaned;
    }
    if entry.name_ru.as_ref().is_some_and(|r| !looks_russian(r)) {
        entry.name_ru = None;
    }
}

fn looks_russian(s: &str) -> bool {
    s.chars().any(|c| ('\u{0400}'..='\u{04FF}').contains(&c))
}

fn archwing_public_leaf(leaf: &str) -> &str {
    match leaf {
        "SupportJetPack" => "Amesha",
        "StealthJetPack" => "Itzal",
        "StandardJetPack" => "Odonata",
        "DemolitionJetPack" => "Elytron",
        "PrimeJetPack" => "OdonataPrime",
        other => other,
    }
}

/// Archwing recipe folder leaves (PrimeArchwingSystems → OdonataPrime).
fn archwing_recipe_public_leaf(leaf: &str) -> String {
    let mut base = leaf;
    for _ in 0..4 {
        let next = ["Blueprint", "Chassis", "Systems", "Harness", "Wings", "Component"]
            .iter()
            .find_map(|suf| base.strip_suffix(suf));
        match next {
            Some(s) if s != base && !s.is_empty() => base = s,
            _ => break,
        }
    }
    match base {
        "PrimeArchwing" => "OdonataPrime".into(),
        "StandardArchwing" => "Odonata".into(),
        "SupportArchwing" => "Amesha".into(),
        "StealthArchwing" => "Itzal".into(),
        "DemolitionArchwing" => "Elytron".into(),
        other => other.to_string(),
    }
}

fn is_modular_path(lower: &str) -> bool {
    lower.contains("operatoramplifier")
        || lower.contains("operatoramplifiers")
        || lower.contains("modularmelee")
        || lower.contains("modularprimary")
        || lower.contains("modularsecondary")
        || lower.contains("sumodular")
        || lower.contains("kitgun")
        || lower.contains("/ostron/melee/modular")
        || lower.contains("modularmeleeinfested")
        || lower.contains("ampset")
        || (lower.contains("modular")
            && (lower.contains("barrel")
                || lower.contains("handle")
                || lower.contains("grip")
                || lower.contains("chamber")
                || lower.contains("loader")
                || lower.contains("tip")
                || lower.contains("strike")))
}

fn looks_like_weapon_blueprint(lower: &str, name_l: &str, url_l: &str) -> bool {
    lower.contains("/recipes/weapons/")
        || lower.contains("weaponparts")
        || url_l.contains("_barrel")
        || url_l.contains("_receiver")
        || url_l.contains("_stock")
        || url_l.contains("_blade")
        || url_l.contains("_handle")
        || url_l.contains("_link")
        || name_l.contains("barrel")
        || name_l.contains("receiver")
        || name_l.contains("ствол")
        || name_l.contains("приёмник")
}

fn mastery_category_from_paths(lower: &str, name_l: &str, item_type: &str) -> Option<&'static str> {
    let leaf = lower.rsplit('/').next().unwrap_or(lower);
    if is_exalted_ability_weapon(lower, leaf) {
        return None;
    }
    if lower.contains("/archwing/")
        || lower.contains("archwingrecipes")
        || lower.contains("spacesuit")
        || lower.contains("spacegun")
        || lower.contains("spacemelee")
        || item_type == "archwing"
    {
        return Some("archwing");
    }
    if is_modular_path(lower) {
        return Some("modular");
    }
    if is_companion_path(lower) || item_type == "sentinel" {
        return Some("companion");
    }
    if lower.contains("warframerecipes")
        || (lower.contains("/powersuits/") && !lower.contains("/archwing/"))
        || item_type == "warframe"
    {
        return Some("warframe");
    }
    match item_type {
        "primary" => return Some("primary"),
        "secondary" => return Some("secondary"),
        "melee" => return Some("melee"),
        _ => {}
    }
    Some(infer_weapon_slot_category(lower, name_l))
}

fn is_companion_path(lower: &str) -> bool {
    lower.contains("sentinel")
        || lower.contains("kubrow")
        || lower.contains("kavat")
        || lower.contains("catbrow")
        || lower.contains("creaturepets")
        || lower.contains("moa")
        || lower.contains("predasite")
        || lower.contains("vulpaphyla")
        || lower.contains("zanuka")
        || lower.contains("helminthcharger")
        || lower.contains("/pets/")
}

/// Resolve category for weapon/part BPs without letting sentinel leaf collisions
/// (LaserRifle) pull ClanTech player weapons into Companions.
fn weapon_or_companion_category(
    lower: &str,
    name_l: &str,
    item_type: &str,
    leaf_key: &str,
    gear_cat: &HashMap<String, String>,
) -> String {
    if is_companion_path(lower) {
        return "companion".into();
    }
    // Player/clan weapons — never companion, even if a sentinel shares the leaf name.
    if lower.contains("clantech")
        || (lower.contains("/weapons/")
            && !lower.contains("sentinel")
            && !lower.contains("/pets/"))
    {
        return infer_weapon_slot_category(lower, name_l).into();
    }
    if let Some(c) = gear_cat.get(leaf_key) {
        if c == "companion" && !is_companion_path(lower) {
            return infer_weapon_slot_category(lower, name_l).into();
        }
        if is_mastery_type_category(c) {
            return c.clone();
        }
    }
    mastery_category_from_paths(lower, name_l, item_type)
        .unwrap_or_else(|| infer_weapon_slot_category(lower, name_l))
        .into()
}

fn infer_weapon_slot_category(lower: &str, name_l: &str) -> &'static str {
    if lower.contains("/melee/")
        || lower.contains("meleeweapon")
        || lower.contains("/swords/")
        || lower.contains("/dagger")
        || lower.contains("/scythe")
        || lower.contains("/hammer")
        || lower.contains("/staff")
        || lower.contains("/glaive")
        || lower.contains("/nikana")
        || lower.contains("/whip")
        || lower.contains("/fist")
        || lower.contains("/sparring")
        || lower.contains("gunblade")
        || name_l.contains("sword")
        || name_l.contains("мече")
        || name_l.contains("клинок")
        || name_l.contains("молот")
        || name_l.contains("глеф")
        || name_l.contains("никана")
        || name_l.contains("кинжал")
        || name_l.contains("копьё")
        || name_l.contains("копье")
        || name_l.contains("посох")
        || name_l.contains("whip")
        || name_l.contains("scythe")
        || name_l.contains("glaive")
        || name_l.contains("gunblade")
    {
        return "melee";
    }
    if lower.contains("/pistols/")
        || lower.contains("/pistol/")
        || lower.contains("/akimbo/")
        || lower.contains("/secondaries/")
        || lower.contains("throwingweapons")
        || name_l.contains("pistol")
        || name_l.contains("пистолет")
        || name_l.contains("furis")
        || name_l.contains("kunai")
        || name_l.contains("кунай")
        || name_l.contains("вторич")
    {
        return "secondary";
    }
    if lower.contains("/longguns/")
        || lower.contains("/rifle/")
        || lower.contains("/bow")
        || lower.contains("/shotgun")
        || lower.contains("/sniper")
        || lower.contains("launcher")
        || lower.contains("speargun")
        || lower.contains("heavyweapons")
        || name_l.contains("rifle")
        || name_l.contains("shotgun")
        || name_l.contains("винтов")
        || name_l.contains("дробов")
        || name_l.contains("лук")
        || name_l.contains("bow")
        || name_l.contains("sniper")
        || name_l.contains("снайпер")
        || name_l.contains("основн")
    {
        return "primary";
    }
    "primary"
}

fn is_mastery_type_category(cat: &str) -> bool {
    matches!(
        cat,
        "warframe" | "primary" | "secondary" | "melee" | "companion" | "archwing" | "modular"
    )
}

fn should_upgrade_mastery_category(current: &str, next: &str) -> bool {
    if current == next {
        return false;
    }
    if !is_mastery_type_category(next) {
        return false;
    }
    matches!(
        current,
        "prime" | "weapon_prime" | "weapon" | "primary" | ""
    ) || !is_mastery_type_category(current)
}


fn apply_catalog_row(
    entry: &mut MasterySetBuilder,
    row: &ItemRow,
    catalog_by_url: &HashMap<&str, &ItemRow>,
    prices: &HashMap<String, f64>,
    _prefer_set: bool,
) {
    let set_row = row
        .set_url_name
        .as_deref()
        .and_then(|u| catalog_by_url.get(u).copied())
        .or_else(|| {
            if row.url_name.ends_with("_set") {
                Some(row)
            } else {
                None
            }
        });
    let use_row = set_row.unwrap_or(row);

    if entry.thumb.is_none() {
        entry.thumb = use_row.thumb.clone().or(row.thumb.clone());
    }
    if use_row.vaulted == Some(true) || row.vaulted == Some(true) {
        entry.vaulted = true;
    }

    let nice_en = strip_part_suffix_name(&use_row.name);
    if let Some(ru) = use_row
        .name_ru
        .as_deref()
        .or(row.name_ru.as_deref())
        .map(strip_part_suffix_name)
        .filter(|s| !s.is_empty() && looks_russian(s))
    {
        entry.name_ru = Some(ru);
    }
    // Prefer readable catalog EN over raw Lotus leaf keys
    if entry.display_name == humanize_set_key(&entry.set_key) || entry.thumb.is_none() {
        entry.display_name = nice_en;
    }

    let set_url = use_row
        .set_url_name
        .as_deref()
        .and_then(canonicalize_set_slug)
        .or_else(|| canonicalize_set_slug(&use_row.url_name))
        .or_else(|| {
            row.set_url_name
                .as_deref()
                .and_then(canonicalize_set_slug)
        })
        .or_else(|| canonicalize_set_slug(&row.url_name));

    if let Some(set_url) = set_url {
        entry.url_name = Some(set_url.clone());
        if let Some(sr) = catalog_by_url.get(set_url.as_str()) {
            entry.thumb = sr.thumb.clone().or(entry.thumb.clone());
            entry.vaulted = sr.vaulted == Some(true) || entry.vaulted;
            if let Some(ru) = sr.name_ru.as_ref().map(|s| strip_part_suffix_name(s)) {
                entry.name_ru = Some(ru);
            }
            entry.display_name = strip_part_suffix_name(&sr.name);
            // Ducats for a full set listing (not sum of owned scraps)
            if let Some(d) = sr.ducats {
                entry.ducats = Some(d);
            }
            if is_orderable_set_slug(&set_url) {
                if let Some(plat) = prices.get(&set_url) {
                    entry.platinum = Some(*plat);
                }
            }
        }
    } else if entry.url_name.is_none() {
        entry.url_name = Some(row.url_name.clone());
    }
    // Never stamp a single part's platinum onto the set card here —
    // finalize_set_market() picks *_set price or sums all parts.
}

/// Prefer full-set WFM price (`*_set`); fall back to sum of all set parts.
fn finalize_set_market(
    entry: &mut MasterySetBuilder,
    catalog: &[ItemRow],
    catalog_by_url: &HashMap<&str, &ItemRow>,
    prices: &HashMap<String, f64>,
) {
    let set_slug = entry
        .url_name
        .as_deref()
        .and_then(canonicalize_set_slug)
        .or_else(|| canonicalize_set_slug(&format!("{}_set", entry.set_key)))
        .filter(|u| catalog_by_url.contains_key(u.as_str()));

    let Some(set_slug) = set_slug else {
        // No tradeable set listing — don't show a misleading part price
        entry.platinum = None;
        return;
    };

    entry.url_name = Some(set_slug.clone());

    if let Some(sr) = catalog_by_url.get(set_slug.as_str()) {
        if let Some(d) = sr.ducats {
            entry.ducats = Some(d);
        }
        if entry.thumb.is_none() {
            entry.thumb = sr.thumb.clone();
        }
        if entry.name_ru.is_none() {
            entry.name_ru = sr.name_ru.as_ref().map(|s| strip_part_suffix_name(s));
        }
    }

    if let Some(plat) = prices.get(&set_slug) {
        if *plat > 0.0 {
            entry.platinum = Some(*plat);
            return;
        }
    }

    // Sum every part that belongs to this set (exclude the set row itself)
    let mut sum = 0.0_f64;
    let mut n = 0usize;
    for row in catalog {
        if row.url_name == set_slug || row.url_name.ends_with("_set") {
            continue;
        }
        let parent = row
            .set_url_name
            .as_deref()
            .and_then(canonicalize_set_slug)
            .or_else(|| canonicalize_set_slug(&row.url_name));
        if parent.as_deref() != Some(set_slug.as_str()) {
            continue;
        }
        if let Some(p) = prices.get(&row.url_name) {
            sum += *p;
            n += 1;
        }
    }
    entry.platinum = if n > 0 { Some(sum) } else { None };
}

fn resolve_set_catalog(
    entry: &mut MasterySetBuilder,
    by_norm: &HashMap<String, ItemRow>,
    catalog_by_url: &HashMap<&str, &ItemRow>,
    prices: &HashMap<String, f64>,
) {
    let needs_thumb = entry.thumb.is_none();
    let needs_ru = entry.name_ru.as_ref().is_none_or(|r| !looks_russian(r));
    let needs_url = entry.url_name.is_none();
    if !needs_thumb && !needs_ru && !needs_url {
        return;
    }

    let mut candidates: Vec<String> = Vec::new();
    let key = &entry.set_key;
    candidates.push(format!("{key}_set"));
    candidates.push(key.clone());
    if key.ends_with("_prime") {
        candidates.push(format!("{key}_set"));
    }
    // Public display name lookup
    candidates.push(normalize_name(&entry.display_name).replace(' ', "_"));
    candidates.push(normalize_name(&entry.display_name.replace('-', " ")).replace(' ', "_"));
    if let Some(ru) = entry.name_ru.as_ref().filter(|s| looks_russian(s)) {
        // Also try English display if we only have RU
        let _ = ru;
    }

    for c in &candidates {
        if let Some(row) = catalog_by_url.get(c.as_str()).copied() {
            apply_catalog_row(entry, row, catalog_by_url, prices, true);
            break;
        }
        let norm = normalize_name(&c.replace('_', " "));
        if let Some(row) = by_norm.get(&norm) {
            if let Some(full) = catalog_by_url.get(row.url_name.as_str()).copied() {
                apply_catalog_row(entry, full, catalog_by_url, prices, true);
                break;
            }
        }
    }

    if entry.thumb.is_none() || entry.name_ru.as_ref().is_none_or(|r| !looks_russian(r)) {
        if let Some(row) =
            PricingService::resolve_market_item_indexed(&entry.display_name, by_norm)
        {
            if let Some(full) = catalog_by_url.get(row.url_name.as_str()).copied() {
                apply_catalog_row(entry, full, catalog_by_url, prices, true);
            }
        }
    }

    // Borrow art + RU from exact set or prime set (base Acceltra ← Acceltra Prime)
    borrow_market_label_and_thumb(entry, catalog_by_url);
}

fn borrow_market_label_and_thumb(
    entry: &mut MasterySetBuilder,
    catalog_by_url: &HashMap<&str, &ItemRow>,
) {
    let base = entry.set_key.trim_end_matches("_prime").to_string();
    let mut candidates = vec![
        format!("{}_set", entry.set_key),
        format!("{base}_set"),
    ];
    if !entry.set_key.ends_with("_prime") {
        candidates.push(format!("{base}_prime_set"));
    }

    for c in candidates {
        let Some(row) = catalog_by_url.get(c.as_str()) else {
            continue;
        };
        if entry.thumb.is_none() {
            entry.thumb = row.thumb.clone();
        }
        if entry.url_name.is_none() && c == format!("{}_set", entry.set_key) {
            entry.url_name = Some(c.clone());
        }
        if entry.name_ru.as_ref().is_none_or(|r| !looks_russian(r)) {
            if let Some(ru) = row.name_ru.as_ref() {
                let mut label = strip_part_suffix_name(ru);
                if !entry.set_key.ends_with("_prime") && c.contains("_prime") {
                    label = strip_prime_label(&label);
                }
                if looks_russian(&label) {
                    entry.name_ru = Some(label);
                }
            }
        }
        if entry.thumb.is_some() && entry.name_ru.as_ref().is_some_and(|r| looks_russian(r)) {
            break;
        }
    }
}

fn keep_mastery_set(s: &MasterySet) -> bool {
    let name_l = s.name.to_lowercase();
    let key_l = s.set_key.to_lowercase();
    if name_l.contains("helmet")
        || name_l.contains("augment")
        || name_l.contains("charm")
        || name_l.contains("buff")
        || name_l.contains("debuff")
        || name_l.contains("quest")
        || name_l.contains("sequencer")
        || name_l.contains("resource drone")
        || (name_l.contains("drone")
            && !name_l.contains("diriga")
            && !key_l.contains("arc_drone")
            && !key_l.contains("powersuit"))
        || name_l.contains("antigen")
        || name_l.contains("event ingredient")
        || name_l.contains("event clan")
        || name_l.contains("eventium")
        || name_l.contains("negator")
        || name_l.contains("specter summon")
        || name_l.contains("spectre summon")
        || name_l.contains("kubrow egg")
        || name_l.contains("kubrow pet food")
        || name_l.contains("pet food")
        || name_l.contains("pet collar")
        || name_l.contains("genetic foundry")
        || name_l.contains("egg hatcher")
        || name_l.contains("lens ostron")
        || name_l.contains("generic lens")
        || name_l.contains("team energy")
        || name_l.contains("hoverboard") && name_l.contains("jet")
        || name_l.contains("hacking device")
        || name_l.contains("necromech part")
        || key_l.contains("buff")
        || key_l.contains("debuff")
        || key_l.contains("quest")
        || (key_l.contains("drone")
            && !key_l.contains("arc_drone")
            && !key_l.contains("powersuit"))
        || key_l.ends_with("_key")
        || key_l.contains("_key_")
        || key_l.contains("alloy")
        || key_l.contains("cipher")
        || key_l.contains("antigen")
        || key_l.contains("event_ingredient")
        || key_l.contains("eventium")
        || key_l.contains("negator")
        || key_l.contains("specter_summon")
        || key_l.contains("kubrow_egg")
        || key_l.contains("pet_food")
        || key_l.contains("pet_collar")
        || key_l.contains("genetic_foundry")
        || key_l.contains("egg_hatcher")
        || key_l.contains("generic_lens")
        || key_l.contains("team_energy")
        || key_l.contains("hoverboard")
        || key_l.contains("hacking_device")
        || key_l.contains("necromech_part")
        || name_l.contains("alloy")
        || name_l.contains("cipher")
        || (name_l.contains("gem") && name_l.contains("cut"))
        || name_l.contains(" ore")
    {
        return false;
    }
    // Modular hound component cards — not masterable companions themselves
    if key_l.starts_with("zanuka_pet_part")
        || key_l.contains("zanuka_pet_complete")
        || key_l.starts_with("zanuka_pet_") && key_l.contains("power_suit")
    {
        return false;
    }
    if key_l.ends_with("_armor")
        || key_l.contains("disable_passive")
        || key_l == "helminth"
        || name_l.contains("набор брони")
        || name_l.contains("disable passive")
    {
        return false;
    }
    // Helminth ability names (not warframes)
    if name_l.contains("helminth") && s.category != "warframe" && !key_l.contains("charger") {
        return false;
    }
    // Exalted / ability weapons wrongly seeded as frames
    if s.category == "warframe"
        && (key_l.contains("doom_sword")
            || key_l.contains("exalted_")
            || key_l.contains("flight_sword")
            || key_l.contains("flight_pistols")
            || key_l.contains("slinger_pistols")
            || key_l.contains("berserker_melee")
            || key_l.contains("pacifist_fist")
            || key_l.contains("punch_weapon")
            || key_l.contains("whipclaw")
            || key_l.contains("shank_weapon")
            || key_l.contains("storm_weapon")
            || key_l.contains("erupt_weapon")
            || key_l.contains("blast_weapon")
            || key_l.contains("reaper_melee")
            || key_l.contains("_claws")
            || key_l.ends_with("_staff")
            || key_l.contains("monkey_king_staff"))
    {
        return false;
    }
    let cat = match s.category.as_str() {
        "prime" | "weapon_prime" | "weapon" => "primary",
        other => other,
    };
    if !is_mastery_type_category(cat) {
        return false;
    }
    let has_owned_parts = s.parts.iter().any(|p| p.count > 0);
    if has_owned_parts {
        return true;
    }
    if s.owned || s.mastered || s.absorbed {
        return true;
    }
    // Catalog-seeded craftable sets (e.g. Helios Prime with 0 parts yet)
    if s.url_name
        .as_deref()
        .is_some_and(|u| u.ends_with("_set"))
        && !s.parts.is_empty()
    {
        return true;
    }
    // Roster-seeded frames / weapons / companions (may have empty or zero-count slots)
    matches!(
        cat,
        "warframe" | "companion" | "primary" | "secondary" | "melee" | "archwing" | "modular"
    )
}

/// English/public slug (`brakk`, `xaku`, `paris_prime`) → Russian display label.
fn build_slug_ru_map(
    lotus: &HashMap<String, String>,
    lotus_en: &HashMap<String, String>,
    bp_product: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut out: HashMap<String, String> = HashMap::new();

    let insert_slug = |out: &mut HashMap<String, String>, slug: String, ru: String| {
        if slug.is_empty() || !looks_russian(&ru) {
            return;
        }
        out.entry(slug).or_insert(ru);
    };

    // EN display slug → RU (paired by unique)
    for (unique, en) in lotus_en {
        let slug = normalize_name(&strip_export_tags(en)).replace(' ', "_");
        if let Some(ru) = lotus.get(unique) {
            insert_slug(&mut out, slug, strip_export_tags(ru));
        }
    }

    // Every RU lotus row → keyed by unique leaf slug (and trimmed variants)
    for (unique, name) in lotus {
        let cleaned = strip_export_tags(name);
        if !looks_russian(&cleaned) {
            continue;
        }
        let leaf = unique.rsplit('/').next().unwrap_or(unique);
        let mut slug = leaf_set_key(leaf);
        insert_slug(&mut out, slug.clone(), cleaned.clone());
        for suf in [
            "_weapon",
            "_wep",
            "_player_wep",
            "_power_suit",
            "_powersuit",
            "_pet_power_suit",
            "_blueprint",
        ] {
            if let Some(s) = slug.strip_suffix(suf) {
                insert_slug(&mut out, s.to_string(), cleaned.clone());
                slug = s.to_string();
            }
        }
        let lower = unique.to_lowercase();
        if lower.contains("/powersuits/") || lower.contains("warframerecipes") {
            let public = if lower.contains("/archwing/") {
                archwing_public_leaf(leaf)
            } else {
                warframe_public_leaf(leaf)
            };
            insert_slug(&mut out, camel_to_set_key(public), cleaned.clone());
        }
    }

    // Blueprint unique → same RU as product
    for (bp, product) in bp_product {
        let Some(ru) = lotus.get(product).map(|s| strip_export_tags(s)) else {
            continue;
        };
        if !looks_russian(&ru) {
            continue;
        }
        if let Some(en) = lotus_en.get(product) {
            insert_slug(
                &mut out,
                normalize_name(&strip_export_tags(en)).replace(' ', "_"),
                ru.clone(),
            );
        }
        let leaf = bp.rsplit('/').next().unwrap_or(bp);
        insert_slug(&mut out, leaf_set_key(leaf), ru.clone());
        // Product leaf too (CrpGunbladeWeapon → crp_gunblade)
        let pleaf = product.rsplit('/').next().unwrap_or(product);
        let mut pslug = leaf_set_key(pleaf);
        insert_slug(&mut out, pslug.clone(), ru.clone());
        for suf in ["_weapon", "_wep", "_player_wep"] {
            if let Some(s) = pslug.strip_suffix(suf) {
                insert_slug(&mut out, s.to_string(), ru.clone());
                pslug = s.to_string();
            }
        }
    }
    out
}

fn apply_slug_russian_label(entry: &mut MasterySetBuilder, slug_ru: &HashMap<String, String>) {
    if entry.name_ru.as_ref().is_some_and(|r| looks_russian(r)) {
        // Prefer RU as the primary card title
        if let Some(ru) = entry.name_ru.clone() {
            entry.display_name = ru;
        }
        return;
    }
    let mut candidates = vec![entry.set_key.clone()];
    let from_display = normalize_name(&entry.display_name).replace(' ', "_");
    if !from_display.is_empty() {
        candidates.push(from_display);
    }
    for c in candidates {
        if let Some(ru) = slug_ru.get(&c) {
            entry.name_ru = Some(ru.clone());
            entry.display_name = ru.clone();
            return;
        }
        // Acceltra Prime ← acceltra_prime
        if c.ends_with("_prime") {
            let base = c.trim_end_matches("_prime");
            if let Some(ru) = slug_ru.get(base) {
                let labeled = format!("{ru} Прайм");
                entry.name_ru = Some(labeled.clone());
                entry.display_name = labeled;
                return;
            }
        }
    }
}

fn absorbed_set_keys_from_db(db: &Database) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let Ok(Some(raw)) = db.get_setting("helminth_consumed_suits") else {
        return out;
    };
    let Ok(paths) = serde_json::from_str::<Vec<String>>(&raw) else {
        return out;
    };
    for unique in paths {
        let leaf = unique.rsplit('/').next().unwrap_or(&unique);
        let public = warframe_public_leaf(leaf);
        let key = camel_to_set_key(public);
        if !key.is_empty() {
            out.insert(key);
        }
    }
    out
}

/// Add whole-item craft slots (Akbronco ← 2× Bronco, Dual Kamas ← 2× Kama, …).
fn fill_craft_gear_components(
    entry: &mut MasterySetBuilder,
    craft_gear: &HashMap<String, Vec<CraftGearIng>>,
    inventory: &[InventoryItem],
    lotus: &HashMap<String, String>,
    by_norm: &HashMap<String, ItemRow>,
    catalog_by_url: &HashMap<&str, &ItemRow>,
) {
    let Some(ings) = craft_gear.get(&entry.set_key) else {
        return;
    };
    for ing in ings {
        let leaf = ing.unique.rsplit('/').next().unwrap_or(&ing.unique);
        let public = warframe_public_leaf(leaf);
        // Weapons use leaf as-is (BroncoPrime); frames use public map
        let display_leaf = if ing.unique.contains("/Powersuits/") {
            public
        } else {
            leaf
        };
        let ing_key = camel_to_set_key(display_leaf);
        let role = format!("component_{ing_key}");

        let owned: i64 = inventory
            .iter()
            .filter(|i| {
                let u = crate::pricing::strip_inventory_unique(&i.unique_name);
                u == ing.unique
            })
            .map(|i| i.count)
            .sum();

        let mut name = lotus
            .get(&ing.unique)
            .cloned()
            .unwrap_or_else(|| humanize_set_key(&ing_key));
        let mut url_name: Option<String> = None;

        // Prefer WFM set / item for thumb + RU name
        let candidates = [
            format!("{ing_key}_set"),
            ing_key.clone(),
            format!("{ing_key}_blueprint"),
        ];
        for c in &candidates {
            if let Some(row) = catalog_by_url.get(c.as_str()) {
                url_name = Some(row.url_name.clone());
                if let Some(ru) = row.name_ru.as_ref().filter(|s| !s.is_empty()) {
                    name = strip_part_suffix_name(ru);
                } else if !row.name.is_empty() {
                    name = strip_part_suffix_name(&row.name);
                }
                break;
            }
        }
        if url_name.is_none() {
            if let Some(row) = PricingService::resolve_market_item_indexed(&name, by_norm) {
                url_name = Some(row.url_name.clone());
                if let Some(ru) = row.name_ru {
                    name = strip_part_suffix_name(&ru);
                }
            }
        }

        let part = entry.parts.entry(role.clone()).or_insert(SetPartProgress {
            role: role.clone(),
            count: 0,
            required: ing.count.max(1),
            url_name: url_name.clone(),
            name: Some(name.clone()),
        });
        part.required = ing.count.max(1);
        part.count = owned;
        if part.url_name.is_none() {
            part.url_name = url_name;
        }
        if part.name.as_deref().unwrap_or("").is_empty() {
            part.name = Some(name);
        }
    }
}

/// DE internal Powersuit leaf → public Warframe name (for UI + WFM slug guesses).
fn warframe_public_leaf(leaf: &str) -> &str {
    // Strip all component suffixes (ChassisBlueprint → … → BrokenFrame)
    let mut base = leaf;
    for _ in 0..4 {
        let next = [
            "Blueprint",
            "Neuroptics",
            "Chassis",
            "Systems",
            "Helmet",
            "Component",
            "Harness",
            "Wings",
        ]
        .iter()
        .find_map(|suf| base.strip_suffix(suf));
        match next {
            Some(s) if s != base && !s.is_empty() => base = s,
            _ => break,
        }
    }

    match base {
        "Anti" => "Nova",
        "Berserker" => "Valkyr",
        "Paladin" => "Oberon",
        "Harlequin" => "Mirage",
        "Necro" => "Nekros",
        "Wraith" => "Sevagoth",
        "Trapper" => "Vauban",
        "Runner" => "Gauss",
        "Hoplite" => "Styanax",
        "Fairy" => "Titania",
        "Tengu" => "Zephyr",
        "BrokenFrame" => "Xaku",
        "MonkeyKing" => "Wukong",
        "Bard" => "Octavia",
        "Priest" => "Harrow",
        "Ranger" => "Ivara",
        "Odalisk" => "Protea",
        "Alchemist" => "Lavos",
        "Infestation" => "Nidus",
        "InfestationPrime" => "NidusPrime",
        "Ninja" => "Ash",
        "YinYang" => "Equinox",
        "Anima" | "Animus" => "Equinox", // day/night form parts
        "Pirate" => "Hydroid",
        "Pacifist" => "Baruuk",
        "Glass" => "Gara",
        "IronFrame" | "Ironframe" => "Grendel",
        "Choir" => "Jade", // /Powersuits/Choir = Jade (not Dante)
        "Pagemaster" => "Dante",
        "Jade" => "Nyx", // /Powersuits/Jade = Nyx (folder reuse)
        "ConcreteFrame" | "Concreteframe" => "Qorvex",
        "Cyte09" => "Cyte-09",
        "ExcaliburUmbra" => "Excalibur Umbra",
        "NechroTech" => "Bonewidow",
        "StandardJetPack" | "SupportJetPack" | "StealthJetPack" | "DemolitionJetPack" => base, // filtered elsewhere
        other => other,
    }
}

fn camel_to_set_key(name: &str) -> String {
    if name.contains(' ') {
        return name
            .split_whitespace()
            .map(|p| p.to_ascii_lowercase())
            .collect::<Vec<_>>()
            .join("_");
    }
    leaf_set_key(name)
}

fn part_role_order(category: &str) -> Vec<&'static str> {
    match category {
        "warframe" => vec!["blueprint", "neuroptics", "chassis", "systems"],
        "companion" => vec!["blueprint", "carapace", "cerebrum", "systems"],
        "archwing" => vec!["blueprint", "harness", "wings", "systems"],
        "modular" => vec!["blueprint", "barrel", "receiver", "handle", "grip", "blade", "link"],
        // Weapons (primary / secondary / melee) and legacy labels
        "primary" | "secondary" | "melee" | "weapon_prime" | "weapon" | "prime" => {
            vec!["blueprint", "barrel", "receiver", "stock", "link", "blade", "handle"]
        }
        _ => vec!["blueprint"],
    }
}

fn is_component_role(role: &str) -> bool {
    matches!(
        role,
        "blueprint"
            | "neuroptics"
            | "chassis"
            | "systems"
            | "barrel"
            | "receiver"
            | "stock"
            | "link"
            | "blade"
            | "handle"
            | "gauntlet"
            | "grip"
            | "string"
            | "pouch"
            | "chain"
            | "hilt"
            | "guard"
            | "head"
            | "lower_limb"
            | "upper_limb"
            | "carapace"
            | "cerebrum"
            | "harness"
            | "wings"
            | "other"
    )
}

fn role_from_slug(slug: &str) -> Option<&'static str> {
    let s = slug.to_lowercase();
    // Longer / more specific suffixes first — `…_barrel_blueprint` must not become `blueprint`.
    const SUFFIXES: &[(&str, &str)] = &[
        ("_neuroptics_blueprint", "neuroptics"),
        ("_chassis_blueprint", "chassis"),
        ("_systems_blueprint", "systems"),
        ("_barrel_blueprint", "barrel"),
        ("_reciever_blueprint", "receiver"),
        ("_receiver_blueprint", "receiver"),
        ("_stock_blueprint", "stock"),
        ("_link_blueprint", "link"),
        ("_blade_blueprint", "blade"),
        ("_handle_blueprint", "handle"),
        ("_gauntlet_blueprint", "gauntlet"),
        ("_grip_blueprint", "grip"),
        ("_string_blueprint", "string"),
        ("_pouch_blueprint", "pouch"),
        ("_chain_blueprint", "chain"),
        ("_hilt_blueprint", "hilt"),
        ("_guard_blueprint", "guard"),
        ("_head_blueprint", "head"),
        ("_lower_limb_blueprint", "lower_limb"),
        ("_upper_limb_blueprint", "upper_limb"),
        ("_carapace_blueprint", "carapace"),
        ("_cerebrum_blueprint", "cerebrum"),
        ("_harness_blueprint", "harness"),
        ("_wings_blueprint", "wings"),
        ("_helmet_blueprint", "neuroptics"),
        ("_neuroptics", "neuroptics"),
        ("_chassis", "chassis"),
        ("_systems", "systems"),
        ("_barrel", "barrel"),
        ("_reciever", "receiver"),
        ("_receiver", "receiver"),
        ("_stock", "stock"),
        ("_link", "link"),
        ("_blade", "blade"),
        ("_handle", "handle"),
        ("_gauntlet", "gauntlet"),
        ("_grip", "grip"),
        ("_string", "string"),
        ("_pouch", "pouch"),
        ("_chain", "chain"),
        ("_hilt", "hilt"),
        ("_guard", "guard"),
        ("_head", "head"),
        ("_lower_limb", "lower_limb"),
        ("_upper_limb", "upper_limb"),
        ("_carapace", "carapace"),
        ("_cerebrum", "cerebrum"),
        ("_harness", "harness"),
        ("_wings", "wings"),
        ("_helmet", "neuroptics"),
        ("_blueprint", "blueprint"),
    ];
    for (suf, role) in SUFFIXES {
        if s.ends_with(suf) {
            return Some(*role);
        }
    }
    None
}

/// Add WFM companion sets (Helios Prime, Carrier Prime, …) even with 0 owned parts.
fn seed_companion_sets_from_catalog(
    groups: &mut HashMap<String, MasterySetBuilder>,
    catalog: &[ItemRow],
    catalog_by_url: &HashMap<&str, &ItemRow>,
    prices: &HashMap<String, f64>,
) {
    for row in catalog {
        if !row.url_name.ends_with("_set") {
            continue;
        }
        let is_companion_set = catalog.iter().any(|p| {
            if p.url_name.ends_with("_set") {
                return false;
            }
            let parent = p
                .set_url_name
                .as_deref()
                .and_then(canonicalize_set_slug)
                .or_else(|| canonicalize_set_slug(&p.url_name));
            parent.as_deref() == Some(row.url_name.as_str())
                && (p.url_name.contains("carapace")
                    || p.url_name.contains("cerebrum")
                    || p.url_name.contains("sentinel"))
        });
        if !is_companion_set {
            continue;
        }
        let key = row
            .url_name
            .strip_suffix("_set")
            .unwrap_or(&row.url_name)
            .to_string();
        let entry = groups
            .entry(key.clone())
            .or_insert_with(|| MasterySetBuilder::new(&key, "companion"));
        if entry.category != "companion" {
            entry.category = "companion".into();
        }
        apply_catalog_row(entry, row, catalog_by_url, prices, true);
    }
}

/// Seed every masterable companion powersuit from DE lotus maps (pets + sentinels).
fn seed_companions_from_lotus(
    groups: &mut HashMap<String, MasterySetBuilder>,
    lotus: &HashMap<String, String>,
    lotus_en: &HashMap<String, String>,
) {
    for (path, name) in lotus {
        let lower = path.to_lowercase();
        if !is_companion_path(&lower) {
            continue;
        }
        // Full pet/sentinel bodies only — not precepts, parts, collars, eggs.
        if !lower.contains("powersuit") {
            continue;
        }
        if lower.contains("precept")
            || lower.contains("augment")
            || lower.contains("/parts/")
            || lower.contains("collar")
            || lower.contains("egg")
        {
            continue;
        }
        let key = lotus_en
            .get(path)
            .and_then(|en| en_label_to_set_key(Some(en.as_str())))
            .filter(|k| !k.is_empty())
            .unwrap_or_else(|| {
                let leaf = path.rsplit('/').next().unwrap_or(path.as_str());
                if let Some(public) = companion_public_leaf(leaf) {
                    return camel_to_set_key(public);
                }
                let mut leaf = leaf.to_string();
                for suf in ["PowerSuit", "PetPowerSuit", "SentinelPowerSuit"] {
                    if let Some(s) = leaf.strip_suffix(suf) {
                        if !s.is_empty() {
                            leaf = s.to_string();
                        }
                    }
                }
                camel_to_set_key(&leaf)
            });
        if key.is_empty() {
            continue;
        }
        let entry = groups
            .entry(key.clone())
            .or_insert_with(|| MasterySetBuilder::new(&key, "companion"));
        entry.category = "companion".into();
        if looks_russian(name) {
            apply_lotus_display_name(entry, name, true);
        } else if let Some(en) = lotus_en.get(path) {
            // Prefer EN public name as display until RU slug map runs
            if entry.display_name == humanize_set_key(&entry.set_key) {
                entry.display_name = strip_export_tags(en);
            }
        }
    }

    // Hound models (not always present as PowerSuit rows in sentinel export)
    for (key, ru) in [
        ("bhaira", "Гончая: Бхайра"),
        ("dorma", "Гончая: Дорма"),
        ("hec", "Гончая: Хек"),
    ] {
        let entry = groups
            .entry(key.to_string())
            .or_insert_with(|| MasterySetBuilder::new(key, "companion"));
        entry.category = "companion".into();
        apply_lotus_display_name(entry, ru, true);
    }
}

fn is_seedable_warframe_path(lower: &str, leaf: &str) -> bool {
    if !lower.contains("/powersuits/") {
        return false;
    }
    if is_companion_path(lower) {
        return false;
    }
    if is_exalted_ability_weapon(lower, leaf) {
        return false;
    }
    if lower.contains("augment")
        || lower.contains("charm")
        || lower.contains("beacon")
        || lower.contains("helmet")
        || lower.contains("animus")
        || lower.contains("anima")
        || lower.contains("disablepassive")
        || leaf.ends_with("Mod")
        || leaf == "Helminth"
    {
        return false;
    }
    for suf in [
        "Blueprint",
        "Neuroptics",
        "Chassis",
        "Systems",
        "Helmet",
        "Component",
        "Harness",
        "Wings",
    ] {
        if leaf.ends_with(suf) && leaf.len() > suf.len() {
            return false;
        }
    }
    if leaf.contains("Orion") || leaf.contains("Sirius") {
        return false;
    }
    true
}

/// Warframe ability / exalted weapons live under `/Powersuits/` but are not frames
/// and do not grant Mastery Rank (Exalted Blade, Regulators, Diwata, …).
fn is_exalted_ability_weapon(path_lower: &str, leaf: &str) -> bool {
    let leaf_l = leaf.to_ascii_lowercase();
    if path_lower.contains("exalted") || leaf_l.contains("exalted") {
        return true;
    }
    // Strip variant suffixes so DoomSwordPrime / DoomSwordUmbra match.
    let stem = leaf_l
        .strip_suffix("prime")
        .or_else(|| leaf_l.strip_suffix("umbra"))
        .unwrap_or(leaf_l.as_str());
    if matches!(
        stem,
        "doomsword"
            | "flightsword"
            | "flightpistols"
            | "slingerpistols"
            | "berserkermelee"
            | "pacifistfist"
            | "monkeykingstaff"
            | "wukongprimestaff"
            | "garudaclaws"
            | "atlaspunchweapon"
            | "khorawhipclawweapon"
            | "garashankweapon"
            | "ninjastormweapon"
            | "choireruptweapon"
            | "blastweapon"
            | "reapermeleeweapon"
            | "sevagothshadowclawsweapon"
            | "sevagothshadowprimeclawsweapon"
    ) {
        return true;
    }
    // Generic ability-weapon leaf shapes under Powersuits (or Exalted* elsewhere).
    if path_lower.contains("/powersuits/")
        && (stem.ends_with("weapon")
            || stem.ends_with("melee")
            || stem.ends_with("pistols")
            || stem.ends_with("sword")
            || stem.ends_with("bow")
            || stem.ends_with("staff")
            || stem.ends_with("fist")
            || stem.ends_with("claws")
            || stem.ends_with("book")
            || stem.ends_with("sniper"))
    {
        return true;
    }
    false
}

fn is_weapon_component_path(lower: &str) -> bool {
    lower.contains("weaponparts")
        || lower.contains("blueprint")
        || lower.contains("/recipes/")
        || lower.contains("barrel")
        || lower.contains("receiver")
        || lower.contains("reciever")
        || lower.contains("stock")
        || lower.contains("/blade")
        || lower.contains("handle")
        || lower.contains("/link")
        || lower.contains("gauntlet")
        || lower.contains("carapace")
        || lower.contains("cerebrum")
        || lower.contains("neuroptics")
        || lower.contains("chassis")
}

/// Seed every warframe + archwing powersuit from DE Export.
fn seed_warframes_from_lotus(
    groups: &mut HashMap<String, MasterySetBuilder>,
    lotus: &HashMap<String, String>,
    _lotus_en: &HashMap<String, String>,
) {
    for (path, name) in lotus {
        let lower = path.to_lowercase();
        let leaf = path.rsplit('/').next().unwrap_or(path.as_str());
        if !is_seedable_warframe_path(&lower, leaf) {
            continue;
        }
        let is_arch = lower.contains("/archwing/") || lower.contains("jetpack");
        let category = if is_arch { "archwing" } else { "warframe" };
        let public = if is_arch {
            archwing_public_leaf(leaf).to_string()
        } else {
            warframe_public_leaf(leaf).to_string()
        };
        if public.contains("JetPack") {
            continue;
        }
        let key = camel_to_set_key(&public);
        if key.is_empty() {
            continue;
        }
        let entry = groups
            .entry(key.clone())
            .or_insert_with(|| MasterySetBuilder::new(&key, category));
        if matches!(entry.category.as_str(), "primary" | "secondary" | "melee" | "companion") {
            continue;
        }
        entry.category = category.into();
        if looks_russian(name) {
            apply_lotus_display_name(entry, name, true);
        }
    }
}

/// Seed finished weapons from WFCD English names (paired with DE RU when present).
fn seed_weapons_from_lotus(
    groups: &mut HashMap<String, MasterySetBuilder>,
    lotus: &HashMap<String, String>,
    lotus_en: &HashMap<String, String>,
) {
    for (path, en_name) in lotus_en {
        let lower = path.to_lowercase();
        let leaf = path.rsplit('/').next().unwrap_or(path.as_str());
        if is_exalted_ability_weapon(&lower, leaf) {
            continue;
        }
        if is_weapon_component_path(&lower) {
            continue;
        }
        if is_modular_path(&lower) {
            continue;
        }
        if lower.contains("/sentinels/")
            || lower.contains("sentinelweapon")
            || lower.contains("/pets/")
            || lower.contains("zanuka")
        {
            continue;
        }
        if !(lower.contains("/weapons/")
            || lower.contains("/longguns/")
            || lower.contains("/pistols/")
            || lower.contains("/melee/")
            || lower.contains("clantech"))
        {
            continue;
        }
        let en_l = en_name.to_lowercase();
        let Some(cat) = mastery_category_from_paths(&lower, &en_l, "weapon") else {
            continue;
        };
        if !matches!(cat, "primary" | "secondary" | "melee" | "archwing") {
            continue;
        }
        let Some(key) = en_label_to_set_key(Some(en_name.as_str())) else {
            continue;
        };
        if key.is_empty() {
            continue;
        }
        let entry = groups
            .entry(key.clone())
            .or_insert_with(|| MasterySetBuilder::new(&key, cat));
        if matches!(entry.category.as_str(), "warframe" | "companion") {
            continue;
        }
        if should_upgrade_mastery_category(&entry.category, cat) || entry.category.is_empty() {
            entry.category = cat.into();
        }
        if let Some(ru) = lotus.get(path) {
            apply_lotus_display_name(entry, ru, true);
        } else if entry.display_name == humanize_set_key(&entry.set_key) {
            entry.display_name = strip_export_tags(en_name);
        }
    }
}

/// Seed remaining WFM `*_set` rows (primes / craftable gear) not covered by lotus seeds.
fn seed_market_sets_from_catalog(
    groups: &mut HashMap<String, MasterySetBuilder>,
    catalog: &[ItemRow],
    catalog_by_url: &HashMap<&str, &ItemRow>,
    prices: &HashMap<String, f64>,
) {
    for row in catalog {
        if !row.url_name.ends_with("_set") {
            continue;
        }
        let key = row
            .url_name
            .strip_suffix("_set")
            .unwrap_or(&row.url_name)
            .to_string();
        if key.is_empty() {
            continue;
        }
        // Cosmetic armor bundles, not mastery gear
        if key.ends_with("_armor")
            || key.contains("_armor_")
            || row.name.to_lowercase().contains("armor")
                && row.name.to_lowercase().contains("bundle")
            || row
                .name_ru
                .as_deref()
                .is_some_and(|n| n.to_lowercase().contains("набор брони"))
        {
            continue;
        }

        let mut has_frame = false;
        let mut has_weapon = false;
        let mut has_companion = false;
        for p in catalog {
            if p.url_name.ends_with("_set") {
                continue;
            }
            let parent = p
                .set_url_name
                .as_deref()
                .and_then(canonicalize_set_slug)
                .or_else(|| canonicalize_set_slug(&p.url_name));
            if parent.as_deref() != Some(row.url_name.as_str()) {
                continue;
            }
            let u = p.url_name.to_lowercase();
            if u.contains("neuroptics") || u.contains("chassis") {
                has_frame = true;
            }
            if u.contains("barrel")
                || u.contains("receiver")
                || u.contains("stock")
                || u.contains("blade")
                || u.contains("handle")
                || u.contains("link")
            {
                has_weapon = true;
            }
            if u.contains("carapace") || u.contains("cerebrum") || u.contains("sentinel") {
                has_companion = true;
            }
        }

        let category = if has_companion {
            "companion"
        } else if has_frame && !has_weapon {
            "warframe"
        } else {
            "primary"
        };

        let entry = groups
            .entry(key.clone())
            .or_insert_with(|| MasterySetBuilder::new(&key, category));
        if entry.category == "companion" && category != "companion" {
            // keep companion
        } else if entry.category == "warframe" && category == "primary" {
            // keep warframe
        } else if should_upgrade_mastery_category(&entry.category, category)
            || entry.category.is_empty()
            || (entry.category == "primary" && category == "warframe")
        {
            entry.category = category.into();
        }
        apply_catalog_row(entry, row, catalog_by_url, prices, true);
    }
}

/// Ensure weapon/frame sets expose every catalog part slot (even at count 0).
fn fill_slots_from_catalog(entry: &mut MasterySetBuilder, catalog: &[ItemRow]) {
    let set_key = entry.set_key.clone();
    let set_url = entry
        .url_name
        .as_deref()
        .and_then(canonicalize_set_slug)
        .or_else(|| entry.url_name.clone())
        .or_else(|| Some(format!("{set_key}_set")));
    let Some(set_url) = set_url else {
        return;
    };
    if !set_url.ends_with("_set") {
        return;
    }
    for row in catalog {
        if row.url_name.ends_with("_set") {
            continue;
        }
        let parent = row
            .set_url_name
            .as_deref()
            .and_then(canonicalize_set_slug)
            .or_else(|| canonicalize_set_slug(&row.url_name));
        // Match by canonical parent set only. Do NOT use a bare `{set_key}_` prefix —
        // that wrongly pulls `odonata_prime_*` into the `odonata` set.
        let belongs = parent.as_deref() == Some(set_url.as_str())
            || catalog_part_belongs_to_set_key(&row.url_name, &set_key);
        if !belongs {
            continue;
        }
        let Some(role) = role_from_slug(&row.url_name) else {
            continue;
        };
        entry.parts.entry(role.to_string()).or_insert(SetPartProgress {
            role: role.into(),
            count: 0,
            required: 1,
            url_name: Some(row.url_name.clone()),
            name: Some(row.name_ru.clone().unwrap_or_else(|| row.name.clone())),
        });
    }

    // Prefer weapon category when catalog parts are weapon-like
    if entry.category == "prime" || entry.category == "warframe" {
        let companionish = entry.parts.keys().any(|r| {
            matches!(r.as_str(), "carapace" | "cerebrum")
        });
        let weaponish = entry.parts.keys().any(|r| {
            matches!(
                r.as_str(),
                "barrel"
                    | "receiver"
                    | "stock"
                    | "link"
                    | "blade"
                    | "handle"
            )
        });
        let frameish = entry.parts.keys().any(|r| {
            matches!(r.as_str(), "neuroptics" | "chassis" | "systems")
        });
        if companionish {
            entry.category = "companion".into();
        } else if weaponish && !frameish {
            entry.category = "primary".into();
        }
    }
}

/// `{set_key}_harness_blueprint` belongs to `amesha`; `{set_key}_prime_*` does not.
fn catalog_part_belongs_to_set_key(url: &str, set_key: &str) -> bool {
    let expected = format!("{set_key}_set");
    if canonicalize_set_slug(url).as_deref() == Some(expected.as_str()) {
        return true;
    }
    let Some(rest) = url.strip_prefix(&format!("{set_key}_")) else {
        return false;
    };
    let head = rest.split('_').next().unwrap_or("");
    matches!(
        head,
        "blueprint"
            | "neuroptics"
            | "chassis"
            | "systems"
            | "harness"
            | "wings"
            | "barrel"
            | "receiver"
            | "reciever"
            | "stock"
            | "link"
            | "blade"
            | "handle"
            | "gauntlet"
            | "grip"
            | "string"
            | "pouch"
            | "chain"
            | "hilt"
            | "guard"
            | "head"
            | "lower"
            | "upper"
            | "carapace"
            | "cerebrum"
    )
}

fn part_role_from(unique_l: &str, name_l: &str) -> String {
    // Prefer explicit weapon part tokens before warframe ones
    if unique_l.contains("barrel") || name_l.contains("barrel") || name_l.contains("ствол") {
        return "barrel".into();
    }
    if unique_l.contains("reciever")
        || unique_l.contains("receiver")
        || name_l.contains("receiver")
        || name_l.contains("приёмник")
        || name_l.contains("приемник")
    {
        return "receiver".into();
    }
    if unique_l.contains("stock") || name_l.contains("stock") || name_l.contains("приклад") {
        return "stock".into();
    }
    if unique_l.contains("link") || name_l.contains("link") || name_l.contains("связь") {
        return "link".into();
    }
    if unique_l.contains("blade") || name_l.contains("blade") || name_l.contains("клинок") {
        return "blade".into();
    }
    if unique_l.contains("handle")
        || name_l.contains("handle")
        || name_l.contains("рукоят")
        || name_l.contains("рукоять")
    {
        return "handle".into();
    }
    if unique_l.contains("carapace") || name_l.contains("carapace") || name_l.contains("панцир") {
        return "carapace".into();
    }
    if unique_l.contains("cerebrum") || name_l.contains("cerebrum") || name_l.contains("мозг") {
        return "cerebrum".into();
    }
    if unique_l.contains("harness") || name_l.contains("harness") || name_l.contains("упряжь") {
        return "harness".into();
    }
    if unique_l.contains("wings") || name_l.contains("wings") || name_l.contains("крыл") {
        return "wings".into();
    }
    if unique_l.contains("gauntlet") || name_l.contains("gauntlet") {
        return "gauntlet".into();
    }
    if unique_l.contains("grip") || name_l.contains("grip") {
        return "grip".into();
    }
    if unique_l.contains("string") || name_l.contains("string") {
        return "string".into();
    }
    if unique_l.contains("neuro")
        || name_l.contains("neuro")
        || name_l.contains("нейро")
        // DE Export names warframe neuroptics as *Helmet* under WarframeRecipes.
        || unique_l.contains("helmet")
        || (name_l.contains("helmet")
            && (unique_l.contains("warframerecipes") || unique_l.contains("powersuit")))
    {
        return "neuroptics".into();
    }
    if unique_l.contains("chassis") || name_l.contains("chassis") || name_l.contains("каркас") {
        return "chassis".into();
    }
    if unique_l.contains("systems") || name_l.contains("systems") || name_l.contains("систем") {
        return "systems".into();
    }
    if unique_l.contains("blueprint") || name_l.contains("blueprint") || name_l.contains("чертеж") {
        if !name_l.contains("neuro")
            && !name_l.contains("chassis")
            && !name_l.contains("systems")
            && !name_l.contains("helmet")
            && !name_l.contains("нейро")
            && !name_l.contains("каркас")
            && !name_l.contains("систем")
            && !name_l.contains("barrel")
            && !name_l.contains("receiver")
        {
            return "blueprint".into();
        }
    }
    "other".into()
}

fn leaf_set_key(unique: &str) -> String {
    let leaf = unique.rsplit('/').next().unwrap_or(unique);
    let mut s = leaf.to_string();
    for suf in [
        "Blueprint",
        "Item",
        "Component",
        "Neuroptics",
        "Chassis",
        "Systems",
        "Harness",
        "Wings",
        "Receiver",
        "Barrel",
        "Stock",
        "Blade",
        "Handle",
        "Link",
        "Grip",
        "String",
        "Gauntlet",
        "Pouch",
        "Chain",
        "Hilt",
        "Guard",
        "Head",
        "LowerLimb",
        "UpperLimb",
        "Carapace",
        "Cerebrum",
    ] {
        if let Some(stripped) = s.strip_suffix(suf) {
            if !stripped.is_empty() {
                s = stripped.to_string();
            }
        }
    }
    // CamelCase → snake-ish for grouping: AshPrime → ash_prime
    let mut out = String::new();
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 {
            let prev = chars[i - 1];
            let next = chars.get(i + 1).copied();
            let boundary = (prev.is_lowercase() && c.is_uppercase())
                || (prev.is_uppercase()
                    && c.is_uppercase()
                    && next.is_some_and(|n| n.is_lowercase()));
            if boundary {
                out.push('_');
            }
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

fn humanize_set_key(key: &str) -> String {
    key.split('_')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut c = p.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn strip_part_suffix_name(name: &str) -> String {
    let mut n = name.to_string();
    for suf in [
        " Neuroptics Blueprint",
        " Chassis Blueprint",
        " Systems Blueprint",
        " Blueprint",
        " Neuroptics",
        " Chassis",
        " Systems",
        " Set",
        " Receiver",
        " Barrel",
        " Stock",
        " Link",
        " Blade",
        " Handle",
        " Gauntlet",
        ": Нейрооптика (Чертеж)",
        ": Каркас (Чертеж)",
        ": Система (Чертеж)",
        ": Нейрооптика",
        ": Каркас",
        ": Система",
        ": Приёмник",
        ": Ствол",
        ": Приклад",
        ": Связь",
        ": Клинок",
        ": Рукоять",
        " (Чертеж)",
        ": Комплект",
    ] {
        if let Some(stripped) = n.strip_suffix(suf) {
            n = stripped.to_string();
        }
    }
    n
}

fn strip_prime_label(name: &str) -> String {
    name.replace(" Прайм", "")
        .replace(" Prime", "")
        .trim()
        .to_string()
}
