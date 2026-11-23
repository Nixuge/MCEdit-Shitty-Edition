use crate::block::{BlockRegistry, BlockState};
use crate::world::SchematicVolume;
use glam::UVec3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransformOp {
    RotateCW,
    RotateCCW,
    Roll,
    FlipX,
    FlipY,
    FlipZ,
}

fn rotate_facing_cw(v: &str) -> &str {
    match v {
        "north" => "east",
        "east" => "south",
        "south" => "west",
        "west" => "north",
        other => other,
    }
}

fn rotate_facing_ccw(v: &str) -> &str {
    match v {
        "north" => "west",
        "west" => "south",
        "south" => "east",
        "east" => "north",
        other => other,
    }
}

fn flip_facing_x(v: &str) -> &str {
    match v {
        "east" => "west",
        "west" => "east",
        other => other,
    }
}

fn flip_facing_z(v: &str) -> &str {
    match v {
        "north" => "south",
        "south" => "north",
        other => other,
    }
}

fn flip_facing_y(v: &str) -> &str {
    match v {
        "up" => "down",
        "down" => "up",
        other => other,
    }
}

pub fn transform_block_state(state: &BlockState, op: TransformOp) -> BlockState {
    let mut out = state.clone();
    let props = &mut out.properties;

    if let Some(facing) = props.get("facing").cloned() {
        let next = match op {
            TransformOp::RotateCW => rotate_facing_cw(&facing),
            TransformOp::RotateCCW => rotate_facing_ccw(&facing),
            TransformOp::FlipX => flip_facing_x(&facing),
            TransformOp::FlipZ => flip_facing_z(&facing),
            TransformOp::FlipY => flip_facing_y(&facing),
            TransformOp::Roll => match facing.as_str() {
                "up" => "east",
                "east" => "down",
                "down" => "west",
                "west" => "up",
                other => other,
            },
        };
        props.insert("facing".to_string(), next.to_string());
    }

    if let Some(axis) = props.get("axis").cloned() {
        let next = match op {
            TransformOp::RotateCW | TransformOp::RotateCCW => match axis.as_str() {
                "x" => "z",
                "z" => "x",
                other => other,
            },
            TransformOp::Roll => match axis.as_str() {
                "x" => "y",
                "y" => "x",
                other => other,
            },
            _ => axis.as_str(),
        };
        props.insert("axis".to_string(), next.to_string());
    }

    if matches!(op, TransformOp::FlipY)
        && let Some(half) = props.get("half").cloned()
    {
        let next = match half.as_str() {
            "top" => "bottom",
            "bottom" => "top",
            "upper" => "lower",
            "lower" => "upper",
            other => other,
        };
        props.insert("half".to_string(), next.to_string());
    }

    if let Some(rot) = props.get("rotation").and_then(|s| s.parse::<i32>().ok()) {
        let next = match op {
            TransformOp::RotateCW => (rot + 4).rem_euclid(16),
            TransformOp::RotateCCW => (rot - 4).rem_euclid(16),
            TransformOp::FlipX => (16 - rot).rem_euclid(16),
            TransformOp::FlipZ => (8 - rot).rem_euclid(16),
            _ => rot,
        };
        props.insert("rotation".to_string(), next.to_string());
    }

    if let Some(shape) = props.get("shape").cloned() {
        let next = transform_rail_shape(&shape, op);
        props.insert("shape".to_string(), next.to_string());
    }

    // Rotate cardinal boolean connections (fences/panes/redstone/etc.)
    let has_cardinals = ["north", "east", "south", "west"]
        .iter()
        .any(|k| props.contains_key(*k));
    if has_cardinals {
        let n = props.get("north").cloned();
        let e = props.get("east").cloned();
        let s = props.get("south").cloned();
        let w = props.get("west").cloned();
        match op {
            TransformOp::RotateCW => {
                set_opt(props, "north", w);
                set_opt(props, "east", n);
                set_opt(props, "south", e);
                set_opt(props, "west", s);
            }
            TransformOp::RotateCCW => {
                set_opt(props, "north", e);
                set_opt(props, "east", s);
                set_opt(props, "south", w);
                set_opt(props, "west", n);
            }
            TransformOp::FlipX => {
                set_opt(props, "east", w);
                set_opt(props, "west", e);
            }
            TransformOp::FlipZ => {
                set_opt(props, "north", s);
                set_opt(props, "south", n);
            }
            _ => {}
        }
    }

    if matches!(op, TransformOp::FlipX | TransformOp::FlipZ)
        && let Some(hinge) = props.get("hinge").cloned()
    {
        let next = match hinge.as_str() {
            "left" => "right",
            "right" => "left",
            other => other,
        };
        props.insert("hinge".to_string(), next.to_string());
    }

    out
}

fn set_opt(props: &mut std::collections::BTreeMap<String, String>, k: &str, v: Option<String>) {
    if let Some(val) = v {
        props.insert(k.to_string(), val);
    }
}

fn transform_rail_shape(shape: &str, op: TransformOp) -> &str {
    match op {
        TransformOp::RotateCW => match shape {
            "north_south" => "east_west",
            "east_west" => "north_south",
            "ascending_east" => "ascending_south",
            "ascending_south" => "ascending_west",
            "ascending_west" => "ascending_north",
            "ascending_north" => "ascending_east",
            "south_east" => "south_west",
            "south_west" => "north_west",
            "north_west" => "north_east",
            "north_east" => "south_east",
            other => other,
        },
        TransformOp::RotateCCW => match shape {
            "north_south" => "east_west",
            "east_west" => "north_south",
            "ascending_east" => "ascending_north",
            "ascending_north" => "ascending_west",
            "ascending_west" => "ascending_south",
            "ascending_south" => "ascending_east",
            "south_east" => "north_east",
            "north_east" => "north_west",
            "north_west" => "south_west",
            "south_west" => "south_east",
            other => other,
        },
        TransformOp::FlipX => match shape {
            "ascending_east" => "ascending_west",
            "ascending_west" => "ascending_east",
            "south_east" => "south_west",
            "south_west" => "south_east",
            "north_east" => "north_west",
            "north_west" => "north_east",
            other => other,
        },
        TransformOp::FlipZ => match shape {
            "ascending_north" => "ascending_south",
            "ascending_south" => "ascending_north",
            "south_east" => "north_east",
            "north_east" => "south_east",
            "south_west" => "north_west",
            "north_west" => "south_west",
            other => other,
        },
        _ => shape,
    }
}

pub fn transform_volume(
    vol: &SchematicVolume,
    op: TransformOp,
    registry: &mut BlockRegistry,
    blocks_only: bool,
) -> SchematicVolume {
    let w = vol.size.x;
    let h = vol.size.y;
    let l = vol.size.z;

    let new_size = match op {
        TransformOp::RotateCW | TransformOp::RotateCCW => UVec3::new(l, h, w),
        TransformOp::Roll => UVec3::new(h, w, l),
        TransformOp::FlipX | TransformOp::FlipY | TransformOp::FlipZ => vol.size,
    };

    let mut out = SchematicVolume::new(new_size);

    for y in 0..h {
        for z in 0..l {
            for x in 0..w {
                let src_id = vol.get_block(x, y, z);
                let dst_id = if blocks_only || src_id == crate::block::BlockStateId::AIR {
                    src_id
                } else {
                    let st = registry.get_state(src_id).clone();
                    let transformed = transform_block_state(&st, op);
                    registry.intern(transformed)
                };

                let (nx, ny, nz) = match op {
                    TransformOp::RotateCW => (l - 1 - z, y, x),
                    TransformOp::RotateCCW => (z, y, w - 1 - x),
                    TransformOp::Roll => (y, w - 1 - x, z),
                    TransformOp::FlipX => (w - 1 - x, y, z),
                    TransformOp::FlipY => (x, h - 1 - y, z),
                    TransformOp::FlipZ => (x, y, l - 1 - z),
                };
                out.set_block(nx, ny, nz, dst_id);
            }
        }
    }

    if let Some(src_biomes) = &vol.biomes
        && src_biomes.len() == (w as usize) * (l as usize)
    {
        let mut dst_biomes = vec![1u8; (new_size.x as usize) * (new_size.z as usize)];
        for z in 0..l {
            for x in 0..w {
                let b = src_biomes[(z as usize) * (w as usize) + (x as usize)];
                let (nx, nz) = match op {
                    TransformOp::RotateCW => (l - 1 - z, x),
                    TransformOp::RotateCCW => (z, w - 1 - x),
                    TransformOp::FlipX => (w - 1 - x, z),
                    TransformOp::FlipZ => (x, l - 1 - z),
                    _ => (x, z),
                };
                if nx < new_size.x && nz < new_size.z {
                    dst_biomes[(nz as usize) * (new_size.x as usize) + (nx as usize)] = b;
                }
            }
        }
        out.biomes = Some(dst_biomes);
    }

    // Transform block entity coordinates
    for be in &vol.block_entities {
        let mut c = be.clone();
        if let (Some(x), Some(y), Some(z)) = (c.get_int("x"), c.get_int("y"), c.get_int("z")) {
            let (nx, ny, nz) = transform_coord(x, y, z, w as i32, h as i32, l as i32, op);
            c.insert("x", nx);
            c.insert("y", ny);
            c.insert("z", nz);
        }
        out.block_entities.push(c);
    }

    out.entities = vol.entities.clone();
    out.tile_ticks = vol.tile_ticks.clone();
    out
}

fn transform_coord(
    x: i32,
    y: i32,
    z: i32,
    w: i32,
    h: i32,
    l: i32,
    op: TransformOp,
) -> (i32, i32, i32) {
    match op {
        TransformOp::RotateCW => (l - 1 - z, y, x),
        TransformOp::RotateCCW => (z, y, w - 1 - x),
        TransformOp::Roll => (y, w - 1 - x, z),
        TransformOp::FlipX => (w - 1 - x, y, z),
        TransformOp::FlipY => (x, h - 1 - y, z),
        TransformOp::FlipZ => (x, y, l - 1 - z),
    }
}
