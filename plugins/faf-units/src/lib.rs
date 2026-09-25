use extism_pdk::*;
use serde::{Deserialize, Serialize};

use faf_units::{FafUnitIndex, Unit};

// The unit index is baked into the WASM binary at compile time.
// Run `cargo run -p faf-unit-tools -- download -f json -o plugins/faf-units/data/faf_units.json`
// to refresh it.
const UNITS_JSON: &str = include_str!("../data/faf_units.json");

fn load_index() -> Result<FafUnitIndex, String> {
    serde_json::from_str(UNITS_JSON)
        .map_err(|e| format!("failed to parse embedded unit index: {e}"))
}

#[derive(Debug, Clone, Serialize)]
struct ToolDef {
    name: String,
    description: String,
    prompt_fragment: Option<String>,
    parameters: serde_json::Value,
}

#[plugin_fn]
pub fn register_tools(_input: String) -> FnResult<String> {
    let tools = vec![
        ToolDef {
            name: "faf_units_search".to_string(),
            description: "Search FAF units by id, name, description or category.".to_string(),
            prompt_fragment: Some(
                "When the user asks about FAF (Forged Alliance Forever) units, use faf_units_search to find relevant units, then faf_units_get or faf_units_compare to inspect them."
                    .to_string(),
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Free-text search query (e.g. 'UEF tech3 tank' or 'cybran destroyer')." },
                    "limit": { "type": "integer", "description": "Maximum number of results.", "default": 10 }
                },
                "required": ["query"]
            }),
        },
        ToolDef {
            name: "faf_units_get".to_string(),
            description: "Get detailed information about a single FAF unit by blueprint id.".to_string(),
            prompt_fragment: None,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Blueprint id, e.g. UEL0201." }
                },
                "required": ["id"]
            }),
        },
        ToolDef {
            name: "faf_units_compare".to_string(),
            description: "Compare two FAF units side-by-side.".to_string(),
            prompt_fragment: None,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "id_a": { "type": "string", "description": "First blueprint id." },
                    "id_b": { "type": "string", "description": "Second blueprint id." }
                },
                "required": ["id_a", "id_b"]
            }),
        },
        ToolDef {
            name: "faf_units_naive_dps".to_string(),
            description: "Return the total naive DPS (damage * rate of fire) for each weapon on a unit.".to_string(),
            prompt_fragment: None,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Blueprint id." }
                },
                "required": ["id"]
            }),
        },
    ];

    Ok(serde_json::to_string(&tools)?)
}

#[derive(Debug, Clone, Deserialize)]
struct ExecuteInput {
    tool: String,
    arguments: serde_json::Value,
}

#[plugin_fn]
pub fn execute(input: String) -> FnResult<String> {
    if input.is_empty() {
        return Ok(serde_json::to_string(
            &serde_json::json!({"error":"empty input"}),
        )?);
    }

    let parsed: ExecuteInput = match serde_json::from_str(&input) {
        Ok(i) => i,
        Err(e) => {
            return Ok(serde_json::to_string(
                &serde_json::json!({"error": format!("invalid input: {e}") }),
            )?);
        }
    };

    let result = match parsed.tool.as_str() {
        "faf_units_search" => search(parsed.arguments),
        "faf_units_get" => get_unit(parsed.arguments),
        "faf_units_compare" => compare(parsed.arguments),
        "faf_units_naive_dps" => naive_dps(parsed.arguments),
        _ => serde_json::json!({"error": format!("Unknown tool: {}", parsed.tool) }),
    };

    Ok(serde_json::to_string(&result)?)
}

#[derive(Debug, Clone, Deserialize)]
struct SearchArgs {
    query: String,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    10
}

fn search(args: serde_json::Value) -> serde_json::Value {
    let args: SearchArgs = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => return serde_json::json!({"error": format!("invalid arguments: {e}") }),
    };

    let index = match load_index() {
        Ok(i) => i,
        Err(e) => return serde_json::json!({"error": e }),
    };

    let terms: Vec<String> = args
        .query
        .split_whitespace()
        .map(|s| s.to_lowercase())
        .collect();

    let mut results: Vec<serde_json::Value> = index
        .units
        .iter()
        .filter(|u| {
            if terms.is_empty() {
                return false;
            }
            terms.iter().all(|term| {
                u.id.to_lowercase().contains(term)
                    || u.description.to_lowercase().contains(term)
                    || u.name()
                        .map(|n| n.to_lowercase().contains(term))
                        .unwrap_or(false)
                    || u.name_zh()
                        .map(|n| n.to_lowercase().contains(term))
                        .unwrap_or(false)
                    || u.description_zh()
                        .map(|d| d.to_lowercase().contains(term))
                        .unwrap_or(false)
                    || u.categories.iter().any(|c| c.to_lowercase().contains(term))
                    || u.faction()
                        .map(|f| f.to_lowercase().contains(term))
                        .unwrap_or(false)
            })
        })
        .take(args.limit)
        .map(|u| {
            serde_json::json!({
                "id": u.id,
                "name": u.name(),
                "name_zh": u.name_zh(),
                "faction": u.faction(),
                "description": u.description,
                "description_zh": u.description_zh(),
                "tech_level": u.tech_level(),
                "categories": u.categories,
            })
        })
        .collect();

    results.truncate(args.limit);
    serde_json::json!({ "count": results.len(), "results": results })
}

#[derive(Debug, Clone, Deserialize)]
struct GetArgs {
    id: String,
}

fn get_unit(args: serde_json::Value) -> serde_json::Value {
    let args: GetArgs = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => return serde_json::json!({"error": format!("invalid arguments: {e}") }),
    };

    let index = match load_index() {
        Ok(i) => i,
        Err(e) => return serde_json::json!({"error": e }),
    };

    match index.find_unit(&args.id) {
        Some(u) => {
            serde_json::to_value(u).unwrap_or_else(|e| serde_json::json!({"error": e.to_string() }))
        }
        None => serde_json::json!({"error": format!("unit not found: {}", args.id) }),
    }
}

#[derive(Debug, Clone, Deserialize)]
struct CompareArgs {
    id_a: String,
    id_b: String,
}

fn compare(args: serde_json::Value) -> serde_json::Value {
    let args: CompareArgs = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => return serde_json::json!({"error": format!("invalid arguments: {e}") }),
    };

    let index = match load_index() {
        Ok(i) => i,
        Err(e) => return serde_json::json!({"error": e }),
    };

    let a = match index.find_unit(&args.id_a) {
        Some(u) => u,
        None => return serde_json::json!({"error": format!("unit not found: {}", args.id_a) }),
    };
    let b = match index.find_unit(&args.id_b) {
        Some(u) => u,
        None => return serde_json::json!({"error": format!("unit not found: {}", args.id_b) }),
    };

    serde_json::json!({
        "a": unit_summary(a),
        "b": unit_summary(b),
    })
}

fn unit_summary(u: &Unit) -> serde_json::Value {
    let health = u.defense.as_ref().and_then(|d| d.health);
    let regen = u.defense.as_ref().and_then(|d| d.regen_rate);
    let target_stats = u.build_target_stats();
    let mass_cost = target_stats.map(|s| s.build_cost_mass);
    let energy_cost = target_stats.map(|s| s.build_cost_energy);
    let build_time = target_stats.map(|s| s.build_time);
    let speed = u
        .physics
        .as_ref()
        .and_then(|p| p.max_speed)
        .or_else(|| u.air.as_ref().and_then(|a| a.max_airspeed));

    let total_naive_dps: f64 = u.weapon.iter().filter_map(|w| w.naive_dps()).sum();

    serde_json::json!({
        "id": u.id,
        "name": u.name(),
        "name_zh": u.name_zh(),
        "faction": u.faction(),
        "tech_level": u.tech_level(),
        "health": health,
        "regen_rate": regen,
        "mass_cost": mass_cost,
        "energy_cost": energy_cost,
        "build_time": build_time,
        "max_speed": speed,
        "total_naive_dps": if total_naive_dps > 0.0 { Some(total_naive_dps) } else { None },
        "weapon_count": u.weapon.len(),
    })
}

#[derive(Debug, Clone, Deserialize)]
struct NaiveDpsArgs {
    id: String,
}

fn naive_dps(args: serde_json::Value) -> serde_json::Value {
    let args: NaiveDpsArgs = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => return serde_json::json!({"error": format!("invalid arguments: {e}") }),
    };

    let index = match load_index() {
        Ok(i) => i,
        Err(e) => return serde_json::json!({"error": e }),
    };

    let u = match index.find_unit(&args.id) {
        Some(u) => u,
        None => return serde_json::json!({"error": format!("unit not found: {}", args.id) }),
    };

    let weapons: Vec<serde_json::Value> = u
        .weapon
        .iter()
        .map(|w| {
            serde_json::json!({
                "display_name": w.display_name,
                "label": w.label,
                "category": w.weapon_category,
                "damage": w.damage,
                "rate_of_fire": w.rate_of_fire,
                "naive_dps": w.naive_dps(),
                "max_radius": w.max_radius,
            })
        })
        .collect();

    let total: f64 = u.weapon.iter().filter_map(|w| w.naive_dps()).sum();

    serde_json::json!({
        "id": u.id,
        "name": u.name(),
        "name_zh": u.name_zh(),
        "weapons": weapons,
        "total_naive_dps": total,
    })
}
