use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RivenAnalysis {
    pub weapon: String,
    pub stats: Vec<RivenStat>,
    pub disposition: Option<f64>,
    pub roll_score: f64,
    pub similar_plat_avg: Option<f64>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RivenStat {
    pub name: String,
    pub value: f64,
    pub unit: String,
    pub quality: f64, // 0..1 within typical range
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RivenCompare {
    pub old: RivenAnalysis,
    pub new: RivenAnalysis,
    pub recommendation: String,
}

/// Best-effort parse of a riven link / OCR blob into structured analysis.
pub fn analyze_riven_text(text: &str) -> RivenAnalysis {
    analyze_riven_text_lang(text, "ru")
}

pub fn analyze_riven_text_lang(text: &str, lang: &str) -> RivenAnalysis {
    let ru = !lang.eq_ignore_ascii_case("en");
    let weapon = extract_weapon(text).unwrap_or_else(|| {
        if ru {
            "Неизвестно".into()
        } else {
            "Unknown".into()
        }
    });
    let stats = extract_stats(text);
    let roll_score = if stats.is_empty() {
        0.0
    } else {
        stats.iter().map(|s| s.quality).sum::<f64>() / stats.len() as f64
    };
    let mut notes = Vec::new();
    if roll_score >= 0.85 {
        notes.push(if ru {
            "Сильный ролл — стоит оставить или выставить дорого".into()
        } else {
            "Strong roll — consider keeping or listing high".into()
        });
    } else if roll_score <= 0.4 {
        notes.push(if ru {
            "Слабый ролл — кандидат на реролл".into()
        } else {
            "Weak roll — candidate for reroll".into()
        });
    } else {
        notes.push(if ru {
            "Средний ролл".into()
        } else {
            "Average roll".into()
        });
    }
    RivenAnalysis {
        weapon,
        stats,
        disposition: None,
        roll_score,
        similar_plat_avg: None,
        notes,
    }
}

pub fn compare_rivens(old_text: &str, new_text: &str) -> RivenCompare {
    compare_rivens_lang(old_text, new_text, "ru")
}

pub fn compare_rivens_lang(old_text: &str, new_text: &str, lang: &str) -> RivenCompare {
    let ru = !lang.eq_ignore_ascii_case("en");
    let old = analyze_riven_text_lang(old_text, lang);
    let new = analyze_riven_text_lang(new_text, lang);
    let recommendation = if new.roll_score > old.roll_score + 0.05 {
        if ru {
            "Оставьте новый ролл".into()
        } else {
            "Keep the new roll".into()
        }
    } else if old.roll_score > new.roll_score + 0.05 {
        if ru {
            "Оставьте старый ролл".into()
        } else {
            "Keep the old roll".into()
        }
    } else if ru {
        "Ролы похожи — выбирайте по предпочитаемым статам".into()
    } else {
        "Rolls are similar — pick preferred stats".into()
    };
    RivenCompare {
        old,
        new,
        recommendation,
    }
}

fn extract_weapon(text: &str) -> Option<String> {
    for line in text.lines() {
        let l = line.trim();
        if l.to_lowercase().contains("riven") {
            return Some(
                l.replace("Riven", "")
                    .replace("riven", "")
                    .replace("Mod", "")
                    .trim()
                    .trim_matches(|c: char| c == '[' || c == ']')
                    .to_string(),
            );
        }
    }
    text.lines()
        .next()
        .map(|s| s.trim().trim_matches(|c: char| c == '[' || c == ']').to_string())
}

fn extract_stats(text: &str) -> Vec<RivenStat> {
    let mut stats = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        // e.g. "+120.5% Multishot" or "-40% Recoil"
        if let Some((sign, _rest)) = l.split_once(|c: char| c == '+' || c == '-') {
            let _ = sign;
            let negative = l.starts_with('-');
            let rest = l.trim_start_matches(['+', '-']).trim();
            let mut parts = rest.splitn(2, |c: char| c.is_whitespace() || c == '%');
            if let Some(num) = parts.next() {
                if let Ok(mut value) = num.trim_end_matches('%').parse::<f64>() {
                    if negative {
                        value = -value;
                    }
                    let name = rest
                        .trim_start_matches(num)
                        .trim_start_matches('%')
                        .trim()
                        .to_string();
                    if name.len() >= 3 {
                        let quality = estimate_quality(&name, value);
                        stats.push(RivenStat {
                            name,
                            value,
                            unit: "%".into(),
                            quality,
                        });
                    }
                }
            }
        }
    }
    stats
}

fn estimate_quality(name: &str, value: f64) -> f64 {
    let n = name.to_lowercase();
    // Rough heuristic ranges for common positives
    let (lo, hi) = if n.contains("multishot") {
        (60.0, 140.0)
    } else if n.contains("critical chance") || n.contains("crit chance") {
        (60.0, 180.0)
    } else if n.contains("critical damage") || n.contains("crit damage") {
        (40.0, 120.0)
    } else if n.contains("damage") {
        (80.0, 220.0)
    } else if n.contains("recoil") || n.contains("zoom") {
        // negatives often desirable
        return if value < 0.0 { 0.7 } else { 0.3 };
    } else {
        (20.0, 150.0)
    };
    let v = value.abs();
    ((v - lo) / (hi - lo)).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stats() {
        let text = "Soma Riven\n+110.2% Multishot\n+80% Critical Chance\n-40% Recoil\n";
        let a = analyze_riven_text(text);
        assert!(a.stats.len() >= 2);
        assert!(a.roll_score > 0.0);
    }
}
