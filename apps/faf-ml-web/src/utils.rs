/// Display order for unit category panels in the browser.
pub const CATEGORY_ORDER: &[&str] = &[
    "Land",
    "Air",
    "Naval",
    "Structures - Factories",
    "Structures - Economy",
    "Structures - Weapons",
    "Structures - Support",
    "Structures - Intelligence",
    "Construction - Buildpower",
    "Experimental",
];

/// Canonical faction order used inside each tech cell.
pub const FACTION_ORDER: &[&str] = &["UEF", "Cybran", "Aeon", "Seraphim"];

use faf_blueprints::TechLevel;

/// Short label for a typed tech level.
pub fn tech_level_short(tech: TechLevel) -> &'static str {
    match tech {
        TechLevel::T1 => "T1",
        TechLevel::T2 => "T2",
        TechLevel::T3 => "T3",
        TechLevel::T4 => "EXP",
    }
}

/// Faction spelling used in `public/strategic/` overlay filenames
/// (`UEF_icon_*.png`, `Aeon_icon_*.png`, ...).
pub fn faction_file_prefix(faction: &str) -> &str {
    match faction.to_lowercase().as_str() {
        "uef" => "UEF",
        "cybran" => "Cybran",
        "aeon" => "Aeon",
        "seraphim" => "Seraphim",
        _ => faction,
    }
}

/// Hex faction color for borders and highlights.
pub fn faction_color(faction: &str) -> &'static str {
    match faction.to_lowercase().as_str() {
        "uef" => "#2d78b2",
        "cybran" => "#df2d0e",
        "aeon" => "#19b340",
        "seraphim" => "#fcb419",
        _ => "#888",
    }
}

/// Tailwind-aware glow class for portrait buttons.
pub fn faction_glow_class(faction: &str) -> &'static str {
    match faction.to_lowercase().as_str() {
        "uef" => "border-[rgba(148,193,227,0.33)] shadow-[inset_0_0_4px_rgba(70,174,255,0.43)] bg-[rgba(45,120,178,0.13)]",
        "cybran" => "border-[rgba(247,157,142,0.3)] shadow-[inset_0_0_4px_rgba(255,109,84,0.4)] bg-[rgba(223,45,14,0.1)]",
        "aeon" => "border-[rgba(120,236,150,0.33)] shadow-[inset_0_0_4px_rgba(51,255,103,0.43)] bg-[rgba(25,179,64,0.13)]",
        "seraphim" => "border-[rgba(253,229,176,0.3)] shadow-[inset_0_0_4px_rgba(255,213,124,0.4)] bg-[rgba(252,180,25,0.1)]",
        _ => "border-neutral-600",
    }
}
