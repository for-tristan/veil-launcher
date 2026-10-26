"""Generate the full champion + skin catalog for Veil.

Inputs:
  - /tmp/leagueskins-tree.json   ← GitHub Trees API response (recursive=1)
  - /tmp/champion-full.json        ← Data Dragon championFull.json (all champions + skins)

Outputs:
  - /home/z/my-project/download/lol-skin-launcher/frontend/mock-data.js
  - /home/z/my-project/download/lol-skin-launcher/src-tauri/src/skin_data.rs

Catalog shape:
  - Champion: { id, name, role, tile, skins[] }
  - Skin: { id, name, rarity, tier, art, download }
  - For each base skin in the LeagueSkins repo (chromas excluded), we
    record its GitHub raw download URL so the launcher can fetch it.

Performance notes:
  - 22k tree entries filtered to ~1.5k base skins
  - In-memory matching by skin id (no per-skin HTTP)
  - Single Data Dragon fetch for all champion data
"""

import json
import re
from pathlib import Path

PROJECT_DIR = Path("/home/z/my-project/download/lol-skin-launcher")
JS_OUT = PROJECT_DIR / "frontend" / "mock-data.js"
RS_OUT = PROJECT_DIR / "src-tauri" / "src" / "skin_data.rs"

SPLASH_BASE = "https://ddragon.leagueoflegends.com/cdn/img/champion/splash"
TILE_BASE = "https://ddragon.leagueoflegends.com/cdn/img/champion/tiles"
RAW_BASE = "https://raw.githubusercontent.com/Alban1911/LeagueSkins/main"
DATA_DRAGON_VERSION = "16.21.1"
MOCK_LOL_PATH = r"C:\\Riot Games\\League of Legends\\Game"
APP_VERSION = "0.1.0"

# Rough role mapping by champion id (Riot doesn't expose role in champion.json,
# and the LCS pro role has shifted over the seasons. We use a community-maintained
# best-fit table. Missing champions default to "mid" — the user can tweak.)
ROLE_BY_CHAMPION_ID = {
    # top
    "Aatrox": "top", "Camille": "top", "ChoGath": "top", "Darius": "top",
    "DrMundo": "top", "Fiora": "top", "Gangplank": "top", "Garen": "top",
    "Gnar": "top", "Gragas": "top", "Heimerdinger": "top", "Illaoi": "top",
    "Irelia": "top", "Jax": "top", "Jayce": "top", "Kayle": "top",
    "Kennen": "top", "Kled": "top", "Malphite": "top", "Maokai": "top",
    "Mordekaiser": "top", "Nasus": "top", "Ornn": "top", "Poppy": "top",
    "Quinn": "top", "Renekton": "top", "Riven": "top", "Rumble": "top",
    "Sett": "top", "Shen": "top", "Singed": "top", "Sion": "top",
    "Teemo": "top", "Tryndamere": "top", "Udyr": "top", "Urgot": "top",
    "Vladimir": "top", "Volibear": "top", "Wukong": "top", "Yasuo": "top",
    "Yorick": "top",
    # jungle
    "Amumu": "jungle", "BelVeth": "jungle", "Briar": "jungle", "Diana": "jungle",
    "Ekko": "jungle", "Elise": "jungle", "Evelynn": "jungle", "Fiddlesticks": "jungle",
    "Gragas": "jungle", "Graves": "jungle", "Hecarim": "jungle", "Ivern": "jungle",
    "JarvanIV": "jungle", "Karthus": "jungle", "Kayn": "jungle", "Khazix": "jungle",
    "Kindred": "jungle", "LeeSin": "jungle", "Lillia": "jungle", "MasterYi": "jungle",
    "Nidalee": "jungle", "Nocturne": "jungle", "Nunu": "jungle", "Poppy": "jungle",
    "Rammus": "jungle", "RekSai": "jungle", "Rengar": "jungle", "Sejuani": "jungle",
    "Shaco": "jungle", "Shyvana": "jungle", "Skarner": "jungle", "Taliyah": "jungle",
    "Talon": "jungle", "Trundle": "jungle", "Udyr": "jungle", "Vi": "jungle",
    "Viego": "jungle", "Warwick": "jungle", "Nidalee": "jungle", "Zac": "jungle",
    "Nidalee": "jungle",
    # mid
    "Akali": "mid", "Anivia": "mid", "Annie": "mid", "AurelionSol": "mid",
    "Azir": "mid", "Brand": "mid", "Cassiopeia": "mid", "Corki": "mid",
    "Diana": "mid", "Ekko": "mid", "Fizz": "mid", "Galio": "mid",
    "Gangplank": "mid", "Heimerdinger": "mid", "Karthus": "mid", "Kassadin": "mid",
    "Katarina": "mid", "Kayle": "mid", "Leblanc": "mid", "Lissandra": "mid",
    "Lucian": "mid", "Lux": "mid", "Malzahar": "mid", "Morgana": "mid",
    "Neeko": "mid", "Orianna": "mid", "Pantheon": "mid", "Qiyana": "mid",
    "Ryze": "mid", "Seraphine": "mid", "Swain": "mid", "Sylas": "mid",
    "Syndra": "mid", "Taliyah": "mid", "Talon": "mid", "TwistedFate": "mid",
    "Veigar": "mid", "Velkoz": "mid", "Viktor": "mid", "Vladimir": "mid",
    "Xerath": "mid", "Yasuo": "mid", "Yone": "mid", "Zed": "mid",
    "Ziggs": "mid", "Zilean": "mid", "Zoe": "mid", "Syndra": "mid",
    "Ahri": "mid", "Akshan": "mid", "Vex": "mid", "Hwei": "mid", "Aurora": "mid",
    "Mel": "mid", "Yone": "mid",
    # adc (bot)
    "Aphelios": "adc", "Ashe": "adc", "Caitlyn": "adc", "Draven": "adc",
    "Ezreal": "adc", "Jhin": "adc", "Jinx": "adc", "Kaisa": "adc",
    "Kalista": "adc", "KogMaw": "adc", "Lucian": "adc", "MissFortune": "adc",
    "Quinn": "adc", "Senna": "adc", "Samira": "adc", "Sivir": "adc",
    "Smolder": "adc", "Twitch": "adc", "Varus": "adc", "Vayne": "adc",
    "Xayah": "adc", "Zeri": "adc", "Tristana": "adc", "Corki": "adc",
    "Nilah": "adc", "Graves": "adc", "Kindred": "adc",
    # support
    "Alistar": "support", "Bard": "support", "Braum": "support",
    "Janna": "support", "Karma": "support", "Leona": "support", "Lulu": "support",
    "Milio": "support", "Morgana": "support", "Nami": "support", "Nautilus": "support",
    "Pyke": "support", "Rakan": "support", "Renata": "support", "Rell": "support",
    "Senna": "support", "Seraphine": "support", "Sona": "support", "Soraka": "support",
    "Swain": "support", "Thresh": "support", "TahmKench": "support", "Taric": "support",
    "Velkoz": "support", "Yuumi": "support", "Zyra": "support", "Blitzcrank": "support",
    "Lulu": "support", "Shaco": "support", "Zilean": "support", "Pantheon": "support",
    "Bard": "support", "Hwei": "support",
}

RARITY_MAP = {
    0: "Default",
    1: "Standard", 2: "Standard", 3: "Standard",
    4: "Epic", 5: "Epic", 6: "Epic",
    7: "Legendary", 8: "Legendary",
    9: "Ultimate",
}

TIER_MAP = {
    "Default": None,
    "Standard": "750-975 RP",
    "Epic": "1350 RP",
    "Legendary": "1820 RP",
    "Ultimate": "3250 RP",
}


def pretty_skin_name(champ_display_name, skin_name):
    """Clean up a skin name. 'default' → 'Classic'. Strip 'ChampionName' suffix
    if the skin name ends with the champion's name."""
    if skin_name.lower() == "default":
        return "Classic"
    # Strip champion suffix
    suffix = " " + champ_display_name
    if skin_name.endswith(suffix):
        return skin_name[: -len(suffix)]
    return skin_name


def main():
    # ── Load both inputs ──
    print("Loading inputs...")
    tree = json.load(open("/tmp/leagueskins-tree.json"))
    champ_full = json.load(open("/tmp/champion-full.json"))

    # ── Build champion lookup: champ_key (numeric str) → champion data ──
    # Data Dragon championFull.json is keyed by champion id (string like "Ahri").
    # Each champion has 'key' (numeric string like "103") and a 'skins' array.
    champ_by_key = {}
    skin_by_id = {}  # skin_id (str) → (champ_id, champ_name, skin_name, num)
    for champ_id_str, champ in champ_full["data"].items():
        champ_key = champ["key"]  # e.g. "103"
        champ_name = champ["name"]  # e.g. "Ahri"
        champ_by_key[champ_key] = champ
        for skin in champ.get("skins", []):
            skin_by_id[skin["id"]] = {
                "champ_id_str": champ_id_str,
                "champ_name": champ_name,
                "skin_name": skin["name"],
                "num": skin["num"],
                "rarity_int": skin.get("rarity", 4),
                "chromas": skin.get("chromas", []),
            }
    print(f"Loaded {len(champ_by_key)} champions, {len(skin_by_id)} skins from Data Dragon.")

    # ── Parse the LeagueSkins repo tree ──
    # Pattern: skins/{champ_key}/{skin_id}/{skin_id}.fantome
    # We extract only BASE skin packages (depth=3). Chroma variants live
    # at depth=4 inside their parent skin folder and are excluded by the
    # regex (which requires exactly 3 path segments under `skins/`).
    pattern = re.compile(
        r"^skins/(?P<champ_key>\d+)/(?P<skin_id>\d+)/(?P<skin_id2>\d+)\.fantome$"
    )

    base_skins_by_champ = {}  # champ_key → set of skin_id
    for entry in tree["tree"]:
        if entry["type"] != "blob":
            continue
        path = entry["path"]
        m = pattern.match(path)
        if not m:
            continue
        champ_key = m.group("champ_key")
        skin_id = m.group("skin_id")
        # Sanity check: the file name inside should match the skin_id folder
        if m.group("skin_id2") != skin_id:
            continue
        base_skins_by_champ.setdefault(champ_key, set()).add(skin_id)

    total_skins = sum(len(s) for s in base_skins_by_champ.values())
    print(f"Found {len(base_skins_by_champ)} champions with skins in LeagueSkins repo.")
    print(f"Total base skins: {total_skins}")

    # ── Build catalog ──
    catalog = []  # list of champion dicts
    matched_skins = 0
    unmatched_skins = 0
    for champ_key, skin_ids in base_skins_by_champ.items():
        champ = champ_by_key.get(champ_key)
        if not champ:
            # Champion exists in LeagueSkins repo but not in Data Dragon — skip
            continue
        champ_id_str = champ["id"]  # e.g. "Ahri"
        champ_name = champ["name"]
        role = ROLE_BY_CHAMPION_ID.get(champ_id_str, "mid")
        skins_out = []
        for skin_id in sorted(skin_ids, key=lambda x: int(x)):
            skin_info = skin_by_id.get(skin_id)
            if not skin_info:
                # Couldn't find this skin in Data Dragon — include with placeholder name
                unmatched_skins += 1
                skins_out.append({
                    "id": f"{champ_id_str}_{skin_id}",
                    "name": f"Skin #{skin_id}",
                    "rarity": "Standard",
                    "tier": "750-975 RP",
                    "art": f"{SPLASH_BASE}/{champ_id_str}_0.jpg",  # fallback to base splash
                    "download": f"{RAW_BASE}/skins/{champ_key}/{skin_id}/{skin_id}.fantome",
                })
                continue
            matched_skins += 1
            skin_name = pretty_skin_name(champ_name, skin_info["skin_name"])
            rarity_str = RARITY_MAP.get(skin_info["rarity_int"], "Epic")
            tier = TIER_MAP.get(rarity_str, "1350 RP")
            num = skin_info["num"]
            skins_out.append({
                "id": f"{champ_id_str}_{skin_id}",
                "name": skin_name,
                "rarity": rarity_str,
                "tier": tier,
                "art": f"{SPLASH_BASE}/{champ_id_str}_{num}.jpg",
                "download": f"{RAW_BASE}/skins/{champ_key}/{skin_id}/{skin_id}.fantome",
            })
        catalog.append({
            "id": champ_id_str,
            "name": champ_name,
            "role": role,
            "tile": f"{TILE_BASE}/{champ_id_str}_0.jpg",
            "skins": skins_out,
        })

    # Sort champions by name
    catalog.sort(key=lambda c: c["name"].lower())
    print(f"Catalog: {len(catalog)} champions, {matched_skins} matched + {unmatched_skins} unmatched skins.")

    # ── Write JS mock-data.js ──
    write_js(catalog)
    # ── Write Rust skin_data.rs ──
    write_rust(catalog)
    print()
    print("DONE.")


def write_js(catalog):
    print(f"Writing {JS_OUT}...")
    lines = []
    lines.append("/* ─────────────────────────────────────────────────────────────")
    lines.append("   Veil — full champion + skin catalog")
    lines.append("   Auto-generated by scripts/fetch_leagueskins.py")
    lines.append(f"   Data Dragon version: {DATA_DRAGON_VERSION}")
    lines.append("   Source: https://github.com/Alban1911/LeagueSkins (zip packages)")
    lines.append("   Source: https://developer.riotgames.com/docs/lol (skin names + splash arts)")
    lines.append("   ───────────────────────────────────────────────────────────── */")
    lines.append("")
    lines.append('const SPLASH_BASE = "https://ddragon.leagueoflegends.com/cdn/img/champion/splash";')
    lines.append('const TILE_BASE = "https://ddragon.leagueoflegends.com/cdn/img/champion/tiles";')
    lines.append("")
    lines.append(f'const MOCK_LOL_PATH = "{MOCK_LOL_PATH}";')
    lines.append(f'const APP_VERSION = "{APP_VERSION}";')
    lines.append("")
    lines.append("const MOCK_CHAMPIONS = [")
    for champ in catalog:
        lines.append("  {")
        lines.append(f'    id: {js_str(champ["id"])},')
        lines.append(f'    name: {js_str(champ["name"])},')
        lines.append(f'    role: {js_str(champ["role"])},')
        lines.append(f'    tile: `{champ["tile"]}`,')
        lines.append("    skins: [")
        for skin in champ["skins"]:
            tier_repr = "null" if skin["tier"] is None else js_str(skin["tier"])
            lines.append(
                f'      {{ id: {js_str(skin["id"])}, name: {js_str(skin["name"])}, '
                f'rarity: {js_str(skin["rarity"])}, tier: {tier_repr}, '
                f'art: {js_str(skin["art"])}, download: {js_str(skin["download"])} }},'
            )
        lines.append("    ],")
        lines.append("  },")
    lines.append("];")
    lines.append("")
    lines.append("// Make available globally")
    lines.append("window.VEIL_DATA = { champions: MOCK_CHAMPIONS, lolPath: MOCK_LOL_PATH, version: APP_VERSION };")
    lines.append("")
    JS_OUT.write_text("\n".join(lines))
    print(f"  → {JS_OUT.stat().st_size:,} bytes")


def write_rust(catalog):
    print(f"Writing {RS_OUT}...")
    lines = []
    lines.append("// Veil — full champion + skin catalog (Rust mirror of mock-data.js)")
    lines.append("// Auto-generated by scripts/fetch_leagueskins.py")
    lines.append(f"// Data Dragon version: {DATA_DRAGON_VERSION}")
    lines.append("//")
    lines.append("// Source: https://github.com/Alban1911/LeagueSkins (zip packages)")
    lines.append("// Source: https://developer.riotgames.com/docs/lol (skin names + splash arts)")
    lines.append("")
    lines.append("use crate::{Champion, Skin};")
    lines.append("")
    lines.append("#[rustfmt::skip]")
    lines.append("pub fn champions() -> Vec<Champion> {")
    lines.append("    vec![")
    for champ in catalog:
        lines.append("        Champion {")
        lines.append(f'            id: {rust_str(champ["id"])}.into(),')
        lines.append(f'            name: {rust_str(champ["name"])}.into(),')
        lines.append(f'            role: {rust_str(champ["role"])}.into(),')
        lines.append(f'            tile: {rust_str(champ["tile"])}.into(),')
        lines.append("            skins: vec![")
        for skin in champ["skins"]:
            if skin["tier"] is None:
                tier_repr = "None"
            else:
                tier_repr = f'Some({rust_str(skin["tier"])}.into())'
            lines.append(
                f'                Skin {{ id: {rust_str(skin["id"])}.into(), '
                f'name: {rust_str(skin["name"])}.into(), '
                f'rarity: {rust_str(skin["rarity"])}.into(), '
                f'tier: {tier_repr}, '
                f'art: {rust_str(skin["art"])}.into(), '
                f'download: {rust_str(skin["download"])}.into() }},'
            )
        lines.append("            ],")
        lines.append("        },")
    lines.append("    ]")
    lines.append("}")
    lines.append("")
    RS_OUT.write_text("\n".join(lines))
    print(f"  → {RS_OUT.stat().st_size:,} bytes")


def js_str(s):
    return json.dumps(s, ensure_ascii=False)


def rust_str(s):
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


if __name__ == "__main__":
    main()
