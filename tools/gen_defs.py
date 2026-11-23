#!/usr/bin/env python3
"""Generate rewrite/assets/definitions/java_pre113_blocks.json from legacy JSONs."""
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "rewrite" / "assets" / "definitions" / "java_pre113_blocks.json"

TEX_DIRS = {
    "RIGHT": 0,
    "LEFT": 1,
    "TOP": 2,
    "BOTTOM": 3,
    "BACKWARD": 4,
    "FORWARD": 5,
}
DEFAULT_TILE = [0, 21]  # NOTEX (0, 336) / 16


def resolve_face_tiles(base_tex, tex_dir_map):
    tile = [int(base_tex[0]), int(base_tex[1])]
    faces = [tile[:] for _ in range(6)]
    if not tex_dir_map:
        return faces
    for dirname, dirtex in tex_dir_map.items():
        t = [int(dirtex[0]), int(dirtex[1])]
        if dirname == "SIDES":
            faces[TEX_DIRS["LEFT"]] = t[:]
            faces[TEX_DIRS["RIGHT"]] = t[:]
        elif dirname in TEX_DIRS:
            faces[TEX_DIRS[dirname]] = t[:]
    return faces


def main():
    with open(ROOT / "mcver" / "Java" / "1.11" / "blocks.json", "r", encoding="utf-8") as f:
        b11 = json.load(f)["blocks"]
    with open(ROOT / "mcver" / "Java" / "1.12" / "blocks.json", "r", encoding="utf-8") as f:
        b12 = json.load(f)["blocks"]
    with open(ROOT / "pymclevel" / "pc_blockstates.json", "r", encoding="utf-8") as f:
        bs_root = json.load(f)["minecraft"]

    # Map (id, data) -> (id_str, sorted_properties)
    bs_by_id_data = {}
    bs_by_id = {}
    for name, info in bs_root.items():
        bid = int(info["id"])
        bs_by_id[bid] = name
        props_list = info.get("properties") or []
        for entry in props_list:
            d = int(entry.get("<data>", 0))
            props = {k: str(v) for k, v in sorted(entry.items()) if k != "<data>"}
            bs_by_id_data.setdefault((bid, d), (name, props))

    # Merge 1.11 + 1.12 blocks by id (1.12 overrides if same id)
    blocks_by_id = {}
    for blk in b11 + b12:
        blocks_by_id[int(blk["id"])] = blk

    records = [
        {
            "id": 0,
            "data": 0,
            "name": "minecraft:air",
            "properties": {},
            "display_name": "Air",
            "opacity": 0,
            "brightness": 0,
            "mapcolor": [0, 0, 0],
            "block_type": "AIR",
            "face_tiles": [[0, 21] for _ in range(6)],
            "aka": "",
            "search": "",
        }
    ]

    seen = {(0, 0)}

    for bid in sorted(blocks_by_id.keys()):
        if bid == 0:
            continue
        blk = blocks_by_id[bid]
        data_dict = blk.get("data")
        if not isinstance(data_dict, dict) or not data_dict:
            data_items = [("0", {})]
        else:
            data_items = []
            for k, v in data_dict.items():
                try:
                    int(k)
                except ValueError:
                    continue
                if isinstance(v, dict):
                    data_items.append((k, v))
            if not data_items:
                data_items = [("0", {})]

        # Also pull any blockstate data values defined in pc_blockstates.json
        base_id_str = blk.get("idStr") or bs_by_id.get(bid) or f"block_{bid}"
        base_tex = blk.get("tex", DEFAULT_TILE)
        base_tex_dir = blk.get("tex_direction", {})
        base_opacity = int(blk.get("opacity", 15))
        base_brightness = int(blk.get("brightness", 0))
        base_mapcolor = [int(c) for c in blk.get("mapcolor", [201, 119, 240])[:3]]
        base_type = str(blk.get("type", "NORMAL")).upper()
        base_name = str(blk.get("name", base_id_str))
        base_aka = str(blk.get("aka", ""))
        base_search = str(blk.get("search", ""))

        data_by_val = {}
        for k, v in data_items:
            data_by_val[int(k)] = v

        # Ensure all blockstate entries for this bid also get generated if not in data_by_val
        for (bs_id, bs_d) in list(bs_by_id_data.keys()):
            if bs_id == bid and bs_d not in data_by_val:
                data_by_val[bs_d] = {}

        for dval in sorted(data_by_val.keys()):
            if not (0 <= dval <= 15):
                continue
            if (bid, dval) in seen:
                continue
            seen.add((bid, dval))
            dv = data_by_val[dval]

            tex = dv.get("tex", base_tex)
            tex_dir = dict(base_tex_dir)
            if isinstance(dv.get("tex_direction"), dict):
                tex_dir.update(dv["tex_direction"])

            bs_name, bs_props = bs_by_id_data.get((bid, dval), (base_id_str, {}))
            if not bs_props and dval != 0:
                bs_props = {"data": str(dval)}

            rec = {
                "id": bid,
                "data": dval,
                "name": f"minecraft:{bs_name}",
                "properties": bs_props,
                "display_name": str(dv.get("name", base_name)),
                "opacity": int(dv.get("opacity", base_opacity)),
                "brightness": int(dv.get("brightness", base_brightness)),
                "mapcolor": [int(c) for c in dv.get("mapcolor", base_mapcolor)[:3]],
                "block_type": str(dv.get("type", base_type)).upper(),
                "face_tiles": resolve_face_tiles(tex, tex_dir),
                "aka": str(dv.get("aka", base_aka)),
                "search": str(dv.get("search", base_search)),
            }
            records.append(rec)

    OUT.parent.mkdir(parents=True, exist_ok=True)
    with open(OUT, "w", encoding="utf-8") as f:
        json.dump({"version": "java_pre113", "blocks": records}, f, indent=2)
    print(f"Generated {len(records)} block entries to {OUT}")


if __name__ == "__main__":
    main()
