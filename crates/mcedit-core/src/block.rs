use ahash::AHashMap;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

pub const DEFAULT_PRE113_JSON: &str =
    include_str!("../../../assets/definitions/java_pre113_blocks.json");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BlockStateId(pub u32);

impl BlockStateId {
    pub const AIR: BlockStateId = BlockStateId(0);
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BlockState {
    pub name: String,
    pub properties: BTreeMap<String, String>,
}

impl BlockState {
    pub fn new(name: impl Into<String>) -> Self {
        let mut name = name.into();
        if !name.contains(':') {
            name = format!("minecraft:{name}");
        }
        Self {
            name,
            properties: BTreeMap::new(),
        }
    }

    pub fn with_prop(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.properties.insert(k.into(), v.into());
        self
    }

    pub fn parse(input: &str) -> Self {
        let s = input.trim();
        if let Some((base, rest)) = s.split_once('[') {
            let props_str = rest.trim_end_matches(']');
            let mut properties = BTreeMap::new();
            for pair in props_str.split(',').filter(|p| !p.trim().is_empty()) {
                if let Some((k, v)) = pair.split_once('=') {
                    properties.insert(k.trim().to_string(), v.trim().to_string());
                }
            }
            let mut bs = Self::new(base.trim());
            bs.properties = properties;
            bs
        } else {
            Self::new(s)
        }
    }
}

impl fmt::Display for BlockState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)?;
        if !self.properties.is_empty() {
            write!(f, "[")?;
            for (i, (k, v)) in self.properties.iter().enumerate() {
                if i > 0 {
                    write!(f, ",")?;
                }
                write!(f, "{k}={v}")?;
            }
            write!(f, "]")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BlockShape {
    Air,
    Cube,
    Liquid,
    Leaves,
    Glass,
    Slab,
    Stairs,
    Torch,
    CrossPlant,
    Fence,
    FenceGate,
    Door,
    Trapdoor,
    Rail,
    Redstone,
    Pane,
    Carpet,
    SnowLayer,
    Ladder,
    Vine,
    SignPost,
    WallSign,
    Button,
    Lever,
    PressurePlate,
    Bed,
    Cactus,
    Cake,
    EnchantTable,
    EndPortalFrame,
    PistonBody,
    PistonHead,
    Chest,
}

impl BlockShape {
    pub fn from_block_type(bt: &str, name: &str) -> Self {
        if name == "minecraft:air" || bt == "AIR" {
            return BlockShape::Air;
        }
        match bt {
            "WATER" => BlockShape::Liquid,
            "LEAVES" => BlockShape::Leaves,
            "GLASS" => BlockShape::Glass,
            "HALFHEIGHT" => BlockShape::Slab,
            "STAIRS" => BlockShape::Stairs,
            "TORCH" => BlockShape::Torch,
            "DECORATION_CROSS" | "CROPS" | "NETHER_WART" | "STEM" => BlockShape::CrossPlant,
            "FENCE" => BlockShape::Fence,
            "FENCE_GATE" => BlockShape::FenceGate,
            "DOOR" => BlockShape::Door,
            "TRAPDOOR" => BlockShape::Trapdoor,
            "SIMPLE_RAIL" => BlockShape::Rail,
            "SOLID_PANE" => BlockShape::Pane,
            "THINSLICE" => {
                if name.contains("snow") {
                    BlockShape::SnowLayer
                } else {
                    BlockShape::Carpet
                }
            }
            "LADDER" => BlockShape::Ladder,
            "VINE" => BlockShape::Vine,
            "SIGNPOST" => BlockShape::SignPost,
            "WALLSIGN" => BlockShape::WallSign,
            "BUTTON" => BlockShape::Button,
            "LEVER" => BlockShape::Lever,
            "PRESSURE_PLATE" => BlockShape::PressurePlate,
            "BED" => BlockShape::Bed,
            "CAKE" => BlockShape::Cake,
            "ENCHANTMENT_TABLE" => BlockShape::EnchantTable,
            "ENDER_PORTAL_FRAME" => BlockShape::EndPortalFrame,
            "PISTON_BODY" => BlockShape::PistonBody,
            "PISTON_HEAD" => BlockShape::PistonHead,
            "CHEST" => BlockShape::Chest,
            _ => {
                if name == "minecraft:cactus" {
                    BlockShape::Cactus
                } else if name == "minecraft:redstone_wire" {
                    BlockShape::Redstone
                } else if name == "minecraft:lava" || name == "minecraft:flowing_lava" {
                    BlockShape::Liquid
                } else {
                    BlockShape::Cube
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TintKind {
    None,
    Grass,
    FoliageOak,
    FoliageSpruce,
    FoliageBirch,
    Redstone(u8),
}

impl TintKind {
    pub fn rgb_multiplier(self) -> [f32; 3] {
        match self {
            TintKind::None => [1.0, 1.0, 1.0],
            TintKind::Grass => [0.47, 0.75, 0.35],
            TintKind::FoliageOak => [0.39, 0.71, 0.23],
            TintKind::FoliageSpruce => [0.38, 0.60, 0.38],
            TintKind::FoliageBirch => [0.50, 0.66, 0.33],
            TintKind::Redstone(power) => {
                let p = (power as f32) / 15.0;
                [0.3 + 0.7 * p, 0.05 * p, 0.02]
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockMeta {
    pub display_name: String,
    pub opacity: u8,
    pub light_emission: u8,
    pub is_air: bool,
    pub is_barrier: bool,
    pub is_liquid: bool,
    pub is_translucent: bool,
    pub is_alpha_cutout: bool,
    pub is_full_opaque_cube: bool,
    pub shape: BlockShape,
    pub map_color: [u8; 4],
    pub face_tiles: [[u8; 2]; 6],
    pub tint: TintKind,
    pub legacy_id: Option<(u16, u8)>,
    pub visual_base_id: Option<BlockStateId>,
    pub search_text: String,
}

impl Default for BlockMeta {
    fn default() -> Self {
        Self {
            display_name: "Unknown Block".to_string(),
            opacity: 15,
            light_emission: 0,
            is_air: false,
            is_barrier: false,
            is_liquid: false,
            is_translucent: false,
            is_alpha_cutout: false,
            is_full_opaque_cube: true,
            shape: BlockShape::Cube,
            map_color: [201, 119, 240, 255],
            face_tiles: [[0, 21]; 6],
            tint: TintKind::None,
            legacy_id: None,
            visual_base_id: None,
            search_text: String::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct JsonBlockDefFile {
    blocks: Vec<JsonBlockDef>,
}

#[derive(Debug, Deserialize)]
struct JsonBlockDef {
    id: u16,
    data: u8,
    name: String,
    properties: BTreeMap<String, String>,
    display_name: String,
    opacity: u8,
    brightness: u8,
    mapcolor: [u8; 3],
    block_type: String,
    face_tiles: [[u8; 2]; 6],
    #[serde(default)]
    aka: String,
    #[serde(default)]
    search: String,
}

#[derive(Debug, Clone)]
pub struct BlockRegistry {
    states: Vec<BlockState>,
    metas: Vec<BlockMeta>,
    by_state: AHashMap<BlockState, BlockStateId>,
    by_legacy: AHashMap<(u16, u8), BlockStateId>,
    default_by_name: AHashMap<String, BlockStateId>,
}

impl Default for BlockRegistry {
    fn default() -> Self {
        Self::with_pre113_defaults()
    }
}

impl BlockRegistry {
    pub fn new_empty() -> Self {
        let mut reg = Self {
            states: Vec::new(),
            metas: Vec::new(),
            by_state: AHashMap::new(),
            by_legacy: AHashMap::new(),
            default_by_name: AHashMap::new(),
        };
        let air = BlockState::new("minecraft:air");
        let air_meta = BlockMeta {
            display_name: "Air".to_string(),
            opacity: 0,
            light_emission: 0,
            is_air: true,
            is_barrier: false,
            is_liquid: false,
            is_translucent: true,
            is_alpha_cutout: false,
            is_full_opaque_cube: false,
            shape: BlockShape::Air,
            map_color: [0, 0, 0, 0],
            face_tiles: [[0, 21]; 6],
            tint: TintKind::None,
            legacy_id: Some((0, 0)),
            visual_base_id: Some(BlockStateId::AIR),
            search_text: "air minecraft:air 0:0".to_string(),
        };
        let id = reg.intern_with_meta(air, air_meta);
        debug_assert_eq!(id, BlockStateId::AIR);
        reg
    }

    pub fn with_pre113_defaults() -> Self {
        Self::from_json_str(DEFAULT_PRE113_JSON).expect("embedded java_pre113_blocks.json must parse")
    }

    pub fn from_json_str(json_str: &str) -> Result<Self, serde_json::Error> {
        let parsed: JsonBlockDefFile = serde_json::from_str(json_str)?;
        let mut reg = Self::new_empty();

        for def in parsed.blocks {
            if def.id == 0 && def.data == 0 {
                continue;
            }
            let state = BlockState {
                name: def.name.clone(),
                properties: def.properties,
            };
            let shape = BlockShape::from_block_type(&def.block_type, &def.name);
            let is_air = shape == BlockShape::Air;
            let is_liquid = shape == BlockShape::Liquid;
            let is_translucent = is_liquid
                || def.name == "minecraft:ice"
                || def.name == "minecraft:stained_glass"
                || def.name == "minecraft:stained_glass_pane"
                || def.name == "minecraft:portal"
                || def.name == "minecraft:slime";
            let is_alpha_cutout = !is_air
                && !is_translucent
                && (def.opacity < 15 || !matches!(shape, BlockShape::Cube));
            let is_full_opaque_cube =
                shape == BlockShape::Cube && def.opacity >= 15 && !is_translucent && !is_alpha_cutout;

            let tint = determine_tint(&def.name, def.data, &state);
            let search_text = format!(
                "{} {} {} {} {}:{}",
                def.display_name.to_lowercase(),
                state.to_string().to_lowercase(),
                def.aka.to_lowercase(),
                def.search.to_lowercase(),
                def.id,
                def.data
            );

            let meta = BlockMeta {
                display_name: def.display_name,
                opacity: def.opacity,
                light_emission: def.brightness,
                is_air,
                is_barrier: def.name == "minecraft:barrier",
                is_liquid,
                is_translucent,
                is_alpha_cutout,
                is_full_opaque_cube,
                shape,
                map_color: [def.mapcolor[0], def.mapcolor[1], def.mapcolor[2], 255],
                face_tiles: def.face_tiles,
                tint,
                legacy_id: Some((def.id, def.data)),
                visual_base_id: None,
                search_text,
            };

            let id = reg.intern_with_meta(state, meta);
            reg.by_legacy.entry((def.id, def.data)).or_insert(id);
        }

        reg.register_post113_aliases_and_blocks();
        Ok(reg)
    }

    /// Registers post-1.13 ("The Flattening" 1.13–1.21+) block names by inheriting visual
    /// textures, shapes, tints, and map colors from their pre-1.13 `(id, data)` equivalents,
    /// plus adding 1.13–1.21 blocks (`deepslate`, `tuff`, `calcite`, `copper_block`, `blackstone`, etc.).
    fn register_post113_aliases_and_blocks(&mut self) {
        let alias_from_legacy: &[(&str, u16, u8, &str)] = &[
            ("minecraft:cave_air", 0, 0, "Cave Air"),
            ("minecraft:void_air", 0, 0, "Void Air"),
            ("minecraft:grass_block", 2, 0, "Grass Block"),
            ("minecraft:coarse_dirt", 3, 1, "Coarse Dirt"),
            ("minecraft:podzol", 3, 2, "Podzol"),
            ("minecraft:rooted_dirt", 3, 0, "Rooted Dirt"),
            ("minecraft:granite", 1, 1, "Granite"),
            ("minecraft:polished_granite", 1, 2, "Polished Granite"),
            ("minecraft:diorite", 1, 3, "Diorite"),
            ("minecraft:polished_diorite", 1, 4, "Polished Diorite"),
            ("minecraft:andesite", 1, 5, "Andesite"),
            ("minecraft:polished_andesite", 1, 6, "Polished Andesite"),
            ("minecraft:oak_planks", 5, 0, "Oak Planks"),
            ("minecraft:spruce_planks", 5, 1, "Spruce Planks"),
            ("minecraft:birch_planks", 5, 2, "Birch Planks"),
            ("minecraft:jungle_planks", 5, 3, "Jungle Planks"),
            ("minecraft:acacia_planks", 5, 4, "Acacia Planks"),
            ("minecraft:dark_oak_planks", 5, 5, "Dark Oak Planks"),
            ("minecraft:mangrove_planks", 5, 4, "Mangrove Planks"),
            ("minecraft:cherry_planks", 5, 2, "Cherry Planks"),
            ("minecraft:bamboo_planks", 5, 2, "Bamboo Planks"),
            ("minecraft:crimson_planks", 5, 4, "Crimson Planks"),
            ("minecraft:warped_planks", 5, 1, "Warped Planks"),
            ("minecraft:oak_log", 17, 0, "Oak Log"),
            ("minecraft:spruce_log", 17, 1, "Spruce Log"),
            ("minecraft:birch_log", 17, 2, "Birch Log"),
            ("minecraft:jungle_log", 17, 3, "Jungle Log"),
            ("minecraft:acacia_log", 162, 0, "Acacia Log"),
            ("minecraft:dark_oak_log", 162, 1, "Dark Oak Log"),
            ("minecraft:mangrove_log", 17, 1, "Mangrove Log"),
            ("minecraft:cherry_log", 17, 1, "Cherry Log"),
            ("minecraft:crimson_stem", 17, 3, "Crimson Stem"),
            ("minecraft:warped_stem", 17, 1, "Warped Stem"),
            ("minecraft:oak_wood", 17, 0, "Oak Wood"),
            ("minecraft:spruce_wood", 17, 1, "Spruce Wood"),
            ("minecraft:birch_wood", 17, 2, "Birch Wood"),
            ("minecraft:jungle_wood", 17, 3, "Jungle Wood"),
            ("minecraft:acacia_wood", 162, 0, "Acacia Wood"),
            ("minecraft:dark_oak_wood", 162, 1, "Dark Oak Wood"),
            ("minecraft:oak_leaves", 18, 0, "Oak Leaves"),
            ("minecraft:spruce_leaves", 18, 1, "Spruce Leaves"),
            ("minecraft:birch_leaves", 18, 2, "Birch Leaves"),
            ("minecraft:jungle_leaves", 18, 3, "Jungle Leaves"),
            ("minecraft:acacia_leaves", 161, 0, "Acacia Leaves"),
            ("minecraft:dark_oak_leaves", 161, 1, "Dark Oak Leaves"),
            ("minecraft:mangrove_leaves", 18, 0, "Mangrove Leaves"),
            ("minecraft:azalea_leaves", 18, 0, "Azalea Leaves"),
            ("minecraft:flowering_azalea_leaves", 18, 0, "Flowering Azalea Leaves"),
            ("minecraft:short_grass", 31, 1, "Short Grass"),
            ("minecraft:tall_grass", 31, 1, "Tall Grass"),
            ("minecraft:fern", 31, 2, "Fern"),
            ("minecraft:large_fern", 31, 2, "Large Fern"),
            ("minecraft:dead_bush", 32, 0, "Dead Bush"),
            ("minecraft:seagrass", 31, 1, "Seagrass"),
            ("minecraft:tall_seagrass", 31, 1, "Tall Seagrass"),
            ("minecraft:kelp", 31, 1, "Kelp"),
            ("minecraft:kelp_plant", 31, 1, "Kelp Plant"),
            ("minecraft:dandelion", 37, 0, "Dandelion"),
            ("minecraft:poppy", 38, 0, "Poppy"),
            ("minecraft:blue_orchid", 38, 1, "Blue Orchid"),
            ("minecraft:allium", 38, 2, "Allium"),
            ("minecraft:azure_bluet", 38, 3, "Azure Bluet"),
            ("minecraft:red_tulip", 38, 4, "Red Tulip"),
            ("minecraft:orange_tulip", 38, 5, "Orange Tulip"),
            ("minecraft:white_tulip", 38, 6, "White Tulip"),
            ("minecraft:pink_tulip", 38, 7, "Pink Tulip"),
            ("minecraft:oxeye_daisy", 38, 8, "Oxeye Daisy"),
            ("minecraft:lily_pad", 111, 0, "Lily Pad"),
            ("minecraft:sugar_cane", 83, 0, "Sugar Cane"),
            ("minecraft:cobweb", 30, 0, "Cobweb"),
            ("minecraft:bricks", 45, 0, "Bricks"),
            ("minecraft:stone_bricks", 98, 0, "Stone Bricks"),
            ("minecraft:mossy_stone_bricks", 98, 1, "Mossy Stone Bricks"),
            ("minecraft:cracked_stone_bricks", 98, 2, "Cracked Stone Bricks"),
            ("minecraft:chiseled_stone_bricks", 98, 3, "Chiseled Stone Bricks"),
            ("minecraft:smooth_stone", 43, 8, "Smooth Stone"),
            ("minecraft:smooth_sandstone", 24, 2, "Smooth Sandstone"),
            ("minecraft:chiseled_sandstone", 24, 1, "Chiseled Sandstone"),
            ("minecraft:cut_sandstone", 24, 2, "Cut Sandstone"),
            ("minecraft:terracotta", 172, 0, "Terracotta"),
            ("minecraft:snow", 78, 0, "Snow"),
            ("minecraft:snow_block", 80, 0, "Snow Block"),
            ("minecraft:powder_snow", 80, 0, "Powder Snow"),
            ("minecraft:crimson_roots", 31, 1, "Crimson Roots"),
            ("minecraft:warped_roots", 31, 1, "Warped Roots"),
            ("minecraft:pale_oak_log", 17, 2, "Pale Oak Log"),
            ("minecraft:pale_oak_wood", 17, 2, "Pale Oak Wood"),
            ("minecraft:pale_oak_leaves", 18, 2, "Pale Oak Leaves"),
            ("minecraft:pale_oak_planks", 5, 2, "Pale Oak Planks"),
            ("minecraft:stripped_oak_log", 17, 0, "Stripped Oak Log"),
            ("minecraft:stripped_spruce_log", 17, 1, "Stripped Spruce Log"),
            ("minecraft:stripped_birch_log", 17, 2, "Stripped Birch Log"),
            ("minecraft:stripped_jungle_log", 17, 3, "Stripped Jungle Log"),
            ("minecraft:stripped_acacia_log", 162, 0, "Stripped Acacia Log"),
            ("minecraft:stripped_dark_oak_log", 162, 1, "Stripped Dark Oak Log"),
            ("minecraft:andesite_slab", 44, 0, "Andesite Slab"),
            ("minecraft:diorite_slab", 44, 7, "Diorite Slab"),
            ("minecraft:granite_slab", 44, 4, "Granite Slab"),
            ("minecraft:mossy_cobblestone_slab", 44, 3, "Mossy Cobblestone Slab"),
            ("minecraft:mossy_cobblestone_stairs", 67, 0, "Mossy Cobblestone Stairs"),
            ("minecraft:andesite_wall", 139, 0, "Andesite Wall"),
            ("minecraft:diorite_wall", 139, 0, "Diorite Wall"),
            ("minecraft:granite_wall", 139, 0, "Granite Wall"),
            ("minecraft:mossy_cobblestone_wall", 139, 1, "Mossy Cobblestone Wall"),
            ("minecraft:polished_deepslate_wall", 139, 0, "Polished Deepslate Wall"),
            ("minecraft:spruce_fence", 188, 0, "Spruce Fence"),
            ("minecraft:birch_fence", 189, 0, "Birch Fence"),
            ("minecraft:jungle_fence", 190, 0, "Jungle Fence"),
            ("minecraft:dark_oak_fence", 191, 0, "Dark Oak Fence"),
            ("minecraft:acacia_fence", 192, 0, "Acacia Fence"),
            ("minecraft:magma_block", 213, 0, "Magma Block"),
            ("minecraft:nether_bricks", 112, 0, "Nether Bricks"),
            ("minecraft:red_nether_bricks", 215, 0, "Red Nether Bricks"),
            ("minecraft:end_stone_bricks", 206, 0, "End Stone Bricks"),
            ("minecraft:nether_quartz_ore", 153, 0, "Nether Quartz Ore"),
            ("minecraft:quartz_pillar", 155, 2, "Quartz Pillar"),
            ("minecraft:chiseled_quartz_block", 155, 1, "Chiseled Quartz Block"),
            ("minecraft:smooth_quartz", 155, 0, "Smooth Quartz"),
            ("minecraft:prismarine_bricks", 168, 1, "Prismarine Bricks"),
            ("minecraft:dark_prismarine", 168, 2, "Dark Prismarine"),
            ("minecraft:wet_sponge", 19, 1, "Wet Sponge"),
            ("minecraft:oak_slab", 126, 0, "Oak Slab"),
            ("minecraft:spruce_slab", 126, 1, "Spruce Slab"),
            ("minecraft:birch_slab", 126, 2, "Birch Slab"),
            ("minecraft:jungle_slab", 126, 3, "Jungle Slab"),
            ("minecraft:acacia_slab", 126, 4, "Acacia Slab"),
            ("minecraft:dark_oak_slab", 126, 5, "Dark Oak Slab"),
            ("minecraft:stone_slab", 44, 0, "Stone Slab"),
            ("minecraft:smooth_stone_slab", 44, 0, "Smooth Stone Slab"),
            ("minecraft:sandstone_slab", 44, 1, "Sandstone Slab"),
            ("minecraft:cobblestone_slab", 44, 3, "Cobblestone Slab"),
            ("minecraft:brick_slab", 44, 4, "Brick Slab"),
            ("minecraft:stone_brick_slab", 44, 5, "Stone Brick Slab"),
            ("minecraft:nether_brick_slab", 44, 6, "Nether Brick Slab"),
            ("minecraft:quartz_slab", 44, 7, "Quartz Slab"),
            ("minecraft:cobblestone_stairs", 67, 0, "Cobblestone Stairs"),
            ("minecraft:stone_brick_stairs", 109, 0, "Stone Brick Stairs"),
            ("minecraft:oak_fence", 85, 0, "Oak Fence"),
            ("minecraft:oak_fence_gate", 107, 0, "Oak Fence Gate"),
            ("minecraft:oak_door", 64, 0, "Oak Door"),
            ("minecraft:oak_trapdoor", 96, 0, "Oak Trapdoor"),
            ("minecraft:oak_sign", 63, 0, "Oak Sign"),
            ("minecraft:oak_wall_sign", 68, 0, "Oak Wall Sign"),
            ("minecraft:oak_pressure_plate", 72, 0, "Oak Pressure Plate"),
            ("minecraft:stone_pressure_plate", 70, 0, "Stone Pressure Plate"),
            ("minecraft:oak_button", 143, 0, "Oak Button"),
            ("minecraft:wall_torch", 50, 1, "Wall Torch"),
            ("minecraft:redstone_wall_torch", 76, 1, "Redstone Wall Torch"),
            ("minecraft:Bubble_column", 9, 0, "Bubble Column"),
            ("minecraft:bubble_column", 9, 0, "Bubble Column"),
            // 1.14–1.21 blocks mapped to closest visual tile + custom map colors
            ("minecraft:deepslate", 1, 5, "Deepslate"),
            ("minecraft:cobbled_deepslate", 4, 0, "Cobbled Deepslate"),
            ("minecraft:polished_deepslate", 1, 6, "Polished Deepslate"),
            ("minecraft:deepslate_bricks", 98, 0, "Deepslate Bricks"),
            ("minecraft:deepslate_tiles", 98, 2, "Deepslate Tiles"),
            ("minecraft:reinforced_deepslate", 7, 0, "Reinforced Deepslate"),
            ("minecraft:tuff", 1, 5, "Tuff"),
            ("minecraft:calcite", 1, 3, "Calcite"),
            ("minecraft:dripstone_block", 172, 0, "Dripstone Block"),
            ("minecraft:pointed_dripstone", 172, 0, "Pointed Dripstone"),
            ("minecraft:amethyst_block", 35, 10, "Block of Amethyst"),
            ("minecraft:budding_amethyst", 35, 10, "Budding Amethyst"),
            ("minecraft:copper_ore", 15, 0, "Copper Ore"),
            ("minecraft:deepslate_coal_ore", 16, 0, "Deepslate Coal Ore"),
            ("minecraft:deepslate_iron_ore", 15, 0, "Deepslate Iron Ore"),
            ("minecraft:deepslate_copper_ore", 15, 0, "Deepslate Copper Ore"),
            ("minecraft:deepslate_gold_ore", 14, 0, "Deepslate Gold Ore"),
            ("minecraft:deepslate_redstone_ore", 73, 0, "Deepslate Redstone Ore"),
            ("minecraft:deepslate_emerald_ore", 129, 0, "Deepslate Emerald Ore"),
            ("minecraft:deepslate_lapis_ore", 21, 0, "Deepslate Lapis Ore"),
            ("minecraft:deepslate_diamond_ore", 56, 0, "Deepslate Diamond Ore"),
            ("minecraft:copper_block", 172, 1, "Block of Copper"),
            ("minecraft:exposed_copper", 159, 1, "Exposed Copper"),
            ("minecraft:weathered_copper", 159, 9, "Weathered Copper"),
            ("minecraft:oxidized_copper", 168, 1, "Oxidized Copper"),
            ("minecraft:raw_iron_block", 15, 0, "Block of Raw Iron"),
            ("minecraft:raw_gold_block", 41, 0, "Block of Raw Gold"),
            ("minecraft:raw_copper_block", 172, 1, "Block of Raw Copper"),
            ("minecraft:blackstone", 49, 0, "Blackstone"),
            ("minecraft:polished_blackstone", 49, 0, "Polished Blackstone"),
            ("minecraft:polished_blackstone_bricks", 98, 0, "Polished Blackstone Bricks"),
            ("minecraft:gilded_blackstone", 14, 0, "Gilded Blackstone"),
            ("minecraft:basalt", 1, 5, "Basalt"),
            ("minecraft:polished_basalt", 1, 6, "Polished Basalt"),
            ("minecraft:smooth_basalt", 1, 5, "Smooth Basalt"),
            ("minecraft:crying_obsidian", 49, 0, "Crying Obsidian"),
            ("minecraft:ancient_debris", 87, 0, "Ancient Debris"),
            ("minecraft:nether_gold_ore", 14, 0, "Nether Gold Ore"),
            ("minecraft:soul_soil", 88, 0, "Soul Soil"),
            ("minecraft:crimson_nylium", 87, 0, "Crimson Nylium"),
            ("minecraft:warped_nylium", 168, 2, "Warped Nylium"),
            ("minecraft:warped_wart_block", 168, 2, "Warped Wart Block"),
            ("minecraft:shroomlight", 89, 0, "Shroomlight"),
            ("minecraft:mud", 159, 7, "Mud"),
            ("minecraft:packed_mud", 159, 12, "Packed Mud"),
            ("minecraft:mud_bricks", 98, 0, "Mud Bricks"),
            ("minecraft:moss_block", 2, 0, "Moss Block"),
            ("minecraft:moss_carpet", 171, 5, "Moss Carpet"),
            ("minecraft:cherry_leaves", 35, 6, "Cherry Leaves"),
            ("minecraft:bamboo_block", 17, 2, "Block of Bamboo"),
            ("minecraft:sculk", 159, 15, "Sculk"),
            ("minecraft:lantern", 50, 0, "Lantern"),
            ("minecraft:soul_lantern", 50, 0, "Soul Lantern"),
            ("minecraft:campfire", 89, 0, "Campfire"),
            ("minecraft:barrel", 5, 1, "Barrel"),
            ("minecraft:dirt_path", 208, 0, "Dirt Path"),
            ("minecraft:glow_lichen", 106, 0, "Glow Lichen"),
            ("minecraft:leaf_litter", 32, 0, "Leaf Litter"),
            ("minecraft:bush", 31, 1, "Bush"),
            ("minecraft:firefly_bush", 31, 1, "Firefly Bush"),
            ("minecraft:short_dry_grass", 32, 0, "Short Dry Grass"),
            ("minecraft:tall_dry_grass", 32, 0, "Tall Dry Grass"),
            ("minecraft:wildflowers", 37, 0, "Wildflowers"),
            ("minecraft:cactus_flower", 38, 7, "Cactus Flower"),
            ("minecraft:cornflower", 38, 1, "Cornflower"),
            ("minecraft:lily_of_the_valley", 38, 6, "Lily of the Valley"),
            ("minecraft:wither_rose", 38, 0, "Wither Rose"),
            ("minecraft:amethyst_cluster", 35, 10, "Amethyst Cluster"),
            ("minecraft:large_amethyst_bud", 35, 10, "Large Amethyst Bud"),
            ("minecraft:medium_amethyst_bud", 35, 10, "Medium Amethyst Bud"),
            ("minecraft:small_amethyst_bud", 35, 10, "Small Amethyst Bud"),
            ("minecraft:spawner", 52, 0, "Monster Spawner"),
            ("minecraft:trial_spawner", 52, 0, "Trial Spawner"),
            ("minecraft:vault", 52, 0, "Vault"),
            ("minecraft:beehive", 5, 0, "Beehive"),
            ("minecraft:bee_nest", 5, 2, "Bee Nest"),
            ("minecraft:tuff_bricks", 98, 0, "Tuff Bricks"),
            ("minecraft:polished_tuff", 1, 6, "Polished Tuff"),
            ("minecraft:chiseled_tuff", 98, 3, "Chiseled Tuff"),
            ("minecraft:chiseled_tuff_bricks", 98, 3, "Chiseled Tuff Bricks"),
            ("minecraft:cracked_deepslate_bricks", 98, 2, "Cracked Deepslate Bricks"),
            ("minecraft:cracked_deepslate_tiles", 98, 2, "Cracked Deepslate Tiles"),
            ("minecraft:chiseled_deepslate", 98, 3, "Chiseled Deepslate"),
            ("minecraft:infested_deepslate", 1, 5, "Infested Deepslate"),
            ("minecraft:smooth_stone_slab", 44, 0, "Smooth Stone Slab"),
        ];

        for &(name, leg_id, leg_data, display) in alias_from_legacy {
            if self.default_by_name.contains_key(name) {
                continue;
            }
            if let Some(src_id) = self.find_by_legacy(leg_id, leg_data) {
                let mut meta = self.get_meta(src_id).clone();
                meta.display_name = display.to_string();
                meta.visual_base_id = Some(src_id);
                meta.search_text = format!("{} {}", display.to_lowercase(), name.to_lowercase());
                if name == "minecraft:moss_block" {
                    // Use grass top tile on all 6 faces with grass tint
                    let top_tile = meta.face_tiles[2];
                    meta.face_tiles = [top_tile; 6];
                    meta.tint = TintKind::Grass;
                } else if name == "minecraft:cherry_leaves" {
                    meta.shape = BlockShape::Leaves;
                    meta.is_alpha_cutout = true;
                    meta.is_full_opaque_cube = false;
                }
                self.intern_with_meta(BlockState::new(name), meta);
            }
        }

        // Register all 16 flattened dye colors for wool, stained_glass, stained_glass_pane,
        // terracotta (stained_hardened_clay), concrete, concrete_powder, carpet, bed, shulker_box
        let dye_colors: [(u8, &str, &str); 16] = [
            (0, "white", "White"),
            (1, "orange", "Orange"),
            (2, "magenta", "Magenta"),
            (3, "light_blue", "Light Blue"),
            (4, "yellow", "Yellow"),
            (5, "lime", "Lime"),
            (6, "pink", "Pink"),
            (7, "gray", "Gray"),
            (8, "light_gray", "Light Gray"),
            (9, "cyan", "Cyan"),
            (10, "purple", "Purple"),
            (11, "blue", "Blue"),
            (12, "brown", "Brown"),
            (13, "green", "Green"),
            (14, "red", "Red"),
            (15, "black", "Black"),
        ];
        let colored_families: &[(u16, &str, &str)] = &[
            (35, "wool", "Wool"),
            (95, "stained_glass", "Stained Glass"),
            (160, "stained_glass_pane", "Stained Glass Pane"),
            (159, "terracotta", "Terracotta"),
            (251, "concrete", "Concrete"),
            (252, "concrete_powder", "Concrete Powder"),
            (171, "carpet", "Carpet"),
            (26, "bed", "Bed"),
        ];
        for &(leg_id, suffix, family_display) in colored_families {
            for (data, col_slug, col_display) in dye_colors {
                let name = format!("minecraft:{col_slug}_{suffix}");
                if self.default_by_name.contains_key(&name) {
                    continue;
                }
                if let Some(src_id) = self
                    .find_by_legacy(leg_id, data)
                    .or_else(|| self.find_by_legacy(leg_id, 0))
                {
                    let mut meta = self.get_meta(src_id).clone();
                    meta.display_name = format!("{col_display} {family_display}");
                    meta.visual_base_id = Some(src_id);
                    meta.search_text = format!(
                        "{} {}",
                        meta.display_name.to_lowercase(),
                        name.to_lowercase()
                    );
                    self.intern_with_meta(BlockState::new(name), meta);
                }
            }
        }
    }

    pub fn intern(&mut self, state: BlockState) -> BlockStateId {
        if let Some(&id) = self.by_state.get(&state) {
            return id;
        }
        if state.properties.is_empty()
            && let Some(&id) = self.default_by_name.get(&state.name)
        {
            return id;
        }
        // Inherit visual metadata from the base block name if known (essential for 1.13+
        // property variants like `minecraft:grass_block[snowy=false]` or `minecraft:oak_leaves[distance=3]`)
        if let Some(base_id) = self
            .default_by_name
            .get(&state.name)
            .copied()
            .or_else(|| self.fallback_base_for_name(&state.name))
        {
            let mut meta = self.metas[base_id.0 as usize].clone();
            let display_name = format!("{} ({})", meta.display_name, state);
            meta.search_text = format!(
                "{} {}",
                meta.display_name.to_lowercase(),
                state.to_string().to_lowercase()
            );
            meta.display_name = display_name;
            meta.visual_base_id = Some(meta.visual_base_id.unwrap_or(base_id));
            return self.intern_with_meta(state, meta);
        }
        let display_name = state.to_string();
        let search_text = display_name.to_lowercase();
        let meta = BlockMeta {
            display_name,
            search_text,
            ..Default::default()
        };
        self.intern_with_meta(state, meta)
    }

    pub fn intern_with_meta(&mut self, state: BlockState, meta: BlockMeta) -> BlockStateId {
        if let Some(&existing) = self.by_state.get(&state) {
            return existing;
        }
        let id = BlockStateId(self.states.len() as u32);
        if let Some(leg) = meta.legacy_id {
            self.by_legacy.entry(leg).or_insert(id);
        }
        let is_data_zero = meta.legacy_id.is_some_and(|(_, d)| d == 0);
        if is_data_zero || !self.default_by_name.contains_key(&state.name) {
            self.default_by_name.insert(state.name.clone(), id);
        }
        self.by_state.insert(state.clone(), id);
        self.states.push(state);
        self.metas.push(meta);
        id
    }

    #[inline]
    pub fn get_state(&self, id: BlockStateId) -> &BlockState {
        self.states.get(id.0 as usize).unwrap_or(&self.states[0])
    }

    #[inline]
    pub fn get_meta(&self, id: BlockStateId) -> &BlockMeta {
        self.metas.get(id.0 as usize).unwrap_or(&self.metas[0])
    }

    pub fn find(&self, state: &BlockState) -> Option<BlockStateId> {
        self.by_state.get(state).copied()
    }

    pub fn find_by_name(&self, name: &str) -> Option<BlockStateId> {
        let normalized = if name.contains(':') {
            name.to_string()
        } else {
            format!("minecraft:{name}")
        };
        if let Some(&id) = self.default_by_name.get(&normalized) {
            return Some(id);
        }
        if let Some(&id) = self.by_state.get(&BlockState::new(&normalized)) {
            return Some(id);
        }
        self.states
            .iter()
            .enumerate()
            .find(|(_, s)| s.name == normalized)
            .map(|(i, _)| BlockStateId(i as u32))
    }

    /// Resolves `state` without mutating the registry: returns the exact `BlockStateId` if
    /// already interned, or falls back to the base block name's default `BlockStateId`
    /// (or pattern-matched family visual base for newer 1.13-26.x blocks).
    #[inline]
    pub fn find_exact(&self, state: &BlockState) -> Option<BlockStateId> {
        if let Some(&id) = self.by_state.get(state) {
            return Some(id);
        }
        if state.properties.is_empty()
            && let Some(&id) = self.default_by_name.get(&state.name)
        {
            return Some(id);
        }
        None
    }

    pub fn resolve_readonly(&self, state: &BlockState) -> Option<BlockStateId> {
        self.by_state
            .get(state)
            .copied()
            .or_else(|| self.default_by_name.get(&state.name).copied())
            .or_else(|| self.fallback_base_for_name(&state.name))
    }

    fn fallback_base_for_name(&self, name: &str) -> Option<BlockStateId> {
        let slug = name.strip_prefix("minecraft:").unwrap_or(name);
        if slug.ends_with("_leaves") {
            return self.default_by_name.get("minecraft:oak_leaves").copied();
        }
        if slug.ends_with("_log") || slug.ends_with("_wood") || slug.ends_with("_stem") || slug.ends_with("_hyphae") {
            return self.default_by_name.get("minecraft:oak_log").copied();
        }
        if slug.ends_with("_planks") {
            return self.default_by_name.get("minecraft:oak_planks").copied();
        }
        if slug.ends_with("_slab") {
            return self.default_by_name.get("minecraft:stone_slab").copied();
        }
        if slug.ends_with("_stairs") {
            return self.default_by_name.get("minecraft:cobblestone_stairs").copied();
        }
        if slug.ends_with("_wall") || slug.ends_with("_fence") || slug.ends_with("_fence_gate") {
            return self.default_by_name.get("minecraft:oak_fence").copied();
        }
        if slug.ends_with("_door") {
            return self.default_by_name.get("minecraft:oak_door").copied();
        }
        if slug.ends_with("_trapdoor") {
            return self.default_by_name.get("minecraft:oak_trapdoor").copied();
        }
        if slug.ends_with("_roots") || slug.ends_with("_sprouts") || slug.ends_with("_bush") || slug.ends_with("_grass") {
            return self.default_by_name.get("minecraft:short_grass").copied();
        }
        if slug.ends_with("_ore") {
            return self.default_by_name.get("minecraft:iron_ore").copied();
        }
        if slug.contains("copper") {
            if slug.contains("oxidized") {
                return self.default_by_name.get("minecraft:oxidized_copper").copied();
            }
            if slug.contains("weathered") {
                return self.default_by_name.get("minecraft:weathered_copper").copied();
            }
            if slug.contains("exposed") {
                return self.default_by_name.get("minecraft:exposed_copper").copied();
            }
            return self.default_by_name.get("minecraft:copper_block").copied();
        }
        if slug.contains("tuff") {
            return self.default_by_name.get("minecraft:tuff").copied();
        }
        if slug.contains("deepslate") {
            return self.default_by_name.get("minecraft:deepslate").copied();
        }
        if slug.contains("blackstone") {
            return self.default_by_name.get("minecraft:blackstone").copied();
        }
        None
    }

    pub fn find_by_legacy(&self, id: u16, data: u8) -> Option<BlockStateId> {
        self.by_legacy.get(&(id, data)).copied()
    }

    pub fn len(&self) -> usize {
        self.states.len()
    }

    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    pub fn all_ids(&self) -> impl Iterator<Item = BlockStateId> {
        (0..self.states.len() as u32).map(BlockStateId)
    }

    pub fn search(&self, query: &str) -> Vec<BlockStateId> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return self.all_ids().collect();
        }
        let terms: Vec<&str> = q.split_whitespace().collect();
        self.metas
            .iter()
            .enumerate()
            .filter(|(_, m)| terms.iter().all(|t| m.search_text.contains(t)))
            .map(|(i, _)| BlockStateId(i as u32))
            .collect()
    }

    pub fn matches(&self, candidate: BlockStateId, target: BlockStateId, match_base_only: bool) -> bool {
        if !match_base_only {
            return candidate == target;
        }
        self.get_state(candidate).name == self.get_state(target).name
    }
}

fn determine_tint(name: &str, data: u8, state: &BlockState) -> TintKind {
    match name {
        "minecraft:grass" | "minecraft:tallgrass" | "minecraft:double_plant" | "minecraft:vine" => {
            if name == "minecraft:double_plant" {
                let d = data & 0x07;
                if d == 2 || d == 3 {
                    TintKind::Grass
                } else {
                    TintKind::None
                }
            } else if name == "minecraft:tallgrass" && data == 0 {
                TintKind::None // dead bush
            } else {
                TintKind::Grass
            }
        }
        "minecraft:leaves" => match data & 0x03 {
            1 => TintKind::FoliageSpruce,
            2 => TintKind::FoliageBirch,
            _ => TintKind::FoliageOak,
        },
        "minecraft:leaves2" => TintKind::FoliageOak,
        "minecraft:redstone_wire" => {
            let p = state
                .properties
                .get("power")
                .and_then(|s| s.parse::<u8>().ok())
                .unwrap_or(data & 0x0f);
            TintKind::Redstone(p)
        }
        _ => TintKind::None,
    }
}
