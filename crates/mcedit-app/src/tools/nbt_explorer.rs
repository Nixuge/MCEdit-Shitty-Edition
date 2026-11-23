use crate::editor::EditorState;
use egui::Ui;
use mcedit_core::{UndoEntry, UndoKind, World};
use mcedit_nbt::{Compression, Endianness, NbtCompound, NbtList, NbtTag, tag_id, tag_type_name};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NbtSource {
    LevelDat,
    Player(String),
    ExternalFile(PathBuf),
}

pub struct NbtExplorerState {
    pub source: NbtSource,
    pub root_name: String,
    pub root_compound: NbtCompound,
    pub filter_text: String,
    pub new_key: String,
    pub new_tag_type: u8,
    pub clipboard_tag: Option<NbtTag>,
    pub dirty: bool,
}

impl Default for NbtExplorerState {
    fn default() -> Self {
        Self {
            source: NbtSource::LevelDat,
            root_name: String::new(),
            root_compound: NbtCompound::new(),
            filter_text: String::new(),
            new_key: "NewTag".to_string(),
            new_tag_type: tag_id::INT,
            clipboard_tag: None,
            dirty: false,
        }
    }
}

impl NbtExplorerState {
    pub fn reset_to_world(&mut self, world: &World) {
        self.source = NbtSource::LevelDat;
        self.root_name = String::new();
        self.root_compound = world.metadata.raw_nbt.clone();
        self.dirty = false;
    }

    pub fn load_player(&mut self, world: &World, player_id: &str) {
        if let Some(p) = world.players.iter().find(|p| p.id == player_id) {
            self.source = NbtSource::Player(player_id.to_string());
            self.root_name = p.display_name.clone();
            self.root_compound = p.raw_nbt.clone();
            self.dirty = false;
        }
    }
}

pub fn draw_panel(ui: &mut Ui, editor: &mut EditorState) {
    ui.heading("NBT Explorer");
    ui.separator();

    ui.horizontal(|ui| {
        if ui.button("level.dat").clicked()
            && let Some(w) = &editor.world
        {
            editor.nbt.reset_to_world(w);
        }
        if ui.button("Open File...").clicked()
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("NBT Files", &["dat", "nbt", "schematic"])
                .pick_file()
        {
            match mcedit_nbt::read_nbt_file(&path, Endianness::Big) {
                Ok((rname, comp)) => {
                    editor.nbt.source = NbtSource::ExternalFile(path);
                    editor.nbt.root_name = rname;
                    editor.nbt.root_compound = comp;
                    editor.nbt.dirty = false;
                }
                Err(e) => editor.set_toast(format!("Failed to read NBT: {e}")),
            }
        }
    });

    ui.horizontal(|ui| {
        ui.label("Search:");
        ui.text_edit_singleline(&mut editor.nbt.filter_text);
    });

    if editor.nbt.dirty
        && ui.button("Apply Changes (Undoable)").clicked()
    {
        apply_nbt_changes(editor);
    }

    ui.separator();

    let filter = editor.nbt.filter_text.trim().to_lowercase();
    let mut clipboard = editor.nbt.clipboard_tag.clone();
    let mut mutated = false;
    let new_key = editor.nbt.new_key.clone();
    let new_tid = editor.nbt.new_tag_type;

    ui.horizontal(|ui| {
        ui.label("Add key:");
        ui.add(egui::TextEdit::singleline(&mut editor.nbt.new_key).desired_width(80.0));
        egui::ComboBox::from_id_salt("nbt_new_type")
            .selected_text(tag_type_name(editor.nbt.new_tag_type))
            .show_ui(ui, |ui| {
                for tid in tag_id::BYTE..=tag_id::LONG_ARRAY {
                    ui.selectable_value(&mut editor.nbt.new_tag_type, tid, tag_type_name(tid));
                }
            });
    });

    egui::ScrollArea::vertical().max_height(460.0).show(ui, |ui| {
        mutated |= draw_compound_children(
            ui,
            &mut editor.nbt.root_compound,
            &filter,
            &new_key,
            new_tid,
            &mut clipboard,
        );
    });

    editor.nbt.clipboard_tag = clipboard;
    if mutated {
        editor.nbt.dirty = true;
    }
}

fn apply_nbt_changes(editor: &mut EditorState) {
    match &editor.nbt.source {
        NbtSource::LevelDat => {
            if let Some(w) = &mut editor.world {
                let before = w.metadata.clone();
                w.metadata.raw_nbt = editor.nbt.root_compound.clone();
                w.metadata.sync_from_nbt();
                let after = w.metadata.clone();
                w.metadata_dirty = true;
                w.undo_stack.push(UndoEntry {
                    label: "Edit level.dat NBT".to_string(),
                    dirty_box: None,
                    kind: UndoKind::Metadata { before, after },
                });
                editor.nbt.dirty = false;
                editor.set_toast("Applied level.dat NBT changes.");
            }
        }
        NbtSource::Player(pid) => {
            if let Some(w) = &mut editor.world {
                let before = w.players.clone();
                if let Some(p) = w.players.iter_mut().find(|p| &p.id == pid) {
                    p.raw_nbt = editor.nbt.root_compound.clone();
                    p.sync_from_nbt();
                    p.dirty = true;
                }
                let after = w.players.clone();
                w.undo_stack.push(UndoEntry {
                    label: format!("Edit Player {pid} NBT"),
                    dirty_box: None,
                    kind: UndoKind::Players { before, after },
                });
                editor.nbt.dirty = false;
                editor.set_toast(format!("Applied player {pid} NBT changes."));
            }
        }
        NbtSource::ExternalFile(path) => {
            match mcedit_nbt::write_nbt_file(
                path,
                &editor.nbt.root_name,
                &editor.nbt.root_compound,
                Endianness::Big,
                Compression::Gzip,
            ) {
                Ok(()) => {
                    editor.nbt.dirty = false;
                    editor.set_toast(format!("Saved {}", path.display()));
                }
                Err(e) => editor.set_toast(format!("Failed to save NBT: {e}")),
            }
        }
    }
}

fn draw_compound_children(
    ui: &mut Ui,
    comp: &mut NbtCompound,
    filter: &str,
    new_key: &str,
    new_tid: u8,
    clipboard: &mut Option<NbtTag>,
) -> bool {
    let mut mutated = false;
    let keys: Vec<String> = comp.keys().cloned().collect();
    let mut to_delete: Option<String> = None;

    ui.horizontal(|ui| {
        if ui.small_button("+ Add Child").clicked()
            && !new_key.trim().is_empty()
            && !comp.contains_key(new_key.trim())
            && let Some(tag) = NbtTag::default_for_tag_id(new_tid)
        {
            comp.insert(new_key.trim(), tag);
            mutated = true;
        }
        if let Some(clip) = clipboard
            && ui.small_button("Paste Child").clicked()
        {
            let mut candidate = new_key.trim().to_string();
            if candidate.is_empty() {
                candidate = "Pasted".to_string();
            }
            while comp.contains_key(&candidate) {
                candidate.push('_');
            }
            comp.insert(candidate, clip.clone());
            mutated = true;
        }
    });

    for key in keys {
        if !filter.is_empty() && !key.to_lowercase().contains(filter) {
            // Still allow expanding compounds when filtering
            if !matches!(comp.get(&key), Some(NbtTag::Compound(_) | NbtTag::List(_))) {
                continue;
            }
        }
        let Some(tag) = comp.get_mut(&key) else {
            continue;
        };
        let mut delete_this = false;
        let mut copy_this = false;

        match tag {
            NbtTag::Compound(sub) => {
                egui::CollapsingHeader::new(format!("{} ({{ {} }})", key, sub.len()))
                    .id_salt(format!("comp_{key}"))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui.small_button("Copy").clicked() {
                                copy_this = true;
                            }
                            if ui.small_button("Delete").clicked() {
                                delete_this = true;
                            }
                        });
                        mutated |=
                            draw_compound_children(ui, sub, filter, new_key, new_tid, clipboard);
                    });
            }
            NbtTag::List(list) => {
                egui::CollapsingHeader::new(format!("{} ([ {} ])", key, list.len()))
                    .id_salt(format!("list_{key}"))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui.small_button("Copy").clicked() {
                                copy_this = true;
                            }
                            if ui.small_button("Delete").clicked() {
                                delete_this = true;
                            }
                        });
                        mutated |=
                            draw_list_children(ui, list, filter, new_key, new_tid, clipboard);
                    });
            }
            _ => {
                ui.horizontal(|ui| {
                    ui.label(format!("{key} ({}):", tag.type_name()));
                    mutated |= edit_scalar_tag(ui, tag);
                    if ui.small_button("C").on_hover_text("Copy").clicked() {
                        copy_this = true;
                    }
                    if ui.small_button("X").on_hover_text("Delete").clicked() {
                        delete_this = true;
                    }
                });
            }
        }

        if copy_this && let Some(v) = comp.get(&key) {
            *clipboard = Some(v.clone());
        }
        if delete_this {
            to_delete = Some(key);
        }
    }

    if let Some(k) = to_delete {
        comp.remove(&k);
        mutated = true;
    }
    mutated
}

fn draw_list_children(
    ui: &mut Ui,
    list: &mut NbtList,
    filter: &str,
    new_key: &str,
    new_tid: u8,
    clipboard: &mut Option<NbtTag>,
) -> bool {
    let mut mutated = false;
    let mut remove_idx = None;
    for (idx, item) in list.items_mut().iter_mut().enumerate() {
        match item {
            NbtTag::Compound(sub) => {
                egui::CollapsingHeader::new(format!("[{idx}] ({{ {} }})", sub.len()))
                    .id_salt(format!("lcomp_{idx}"))
                    .show(ui, |ui| {
                        if ui.small_button("Remove Item").clicked() {
                            remove_idx = Some(idx);
                        }
                        mutated |=
                            draw_compound_children(ui, sub, filter, new_key, new_tid, clipboard);
                    });
            }
            NbtTag::List(sub_list) => {
                egui::CollapsingHeader::new(format!("[{idx}] ([ {} ])", sub_list.len()))
                    .id_salt(format!("llist_{idx}"))
                    .show(ui, |ui| {
                        mutated |= draw_list_children(
                            ui, sub_list, filter, new_key, new_tid, clipboard,
                        );
                    });
            }
            _ => {
                ui.horizontal(|ui| {
                    ui.label(format!("[{idx}]:"));
                    mutated |= edit_scalar_tag(ui, item);
                    if ui.small_button("X").clicked() {
                        remove_idx = Some(idx);
                    }
                });
            }
        }
    }
    if let Some(i) = remove_idx {
        list.remove(i);
        mutated = true;
    }
    mutated
}

fn edit_scalar_tag(ui: &mut Ui, tag: &mut NbtTag) -> bool {
    match tag {
        NbtTag::Byte(v) => ui.add(egui::DragValue::new(v)).changed(),
        NbtTag::Short(v) => ui.add(egui::DragValue::new(v)).changed(),
        NbtTag::Int(v) => ui.add(egui::DragValue::new(v)).changed(),
        NbtTag::Long(v) => ui.add(egui::DragValue::new(v)).changed(),
        NbtTag::Float(v) => ui.add(egui::DragValue::new(v).speed(0.1)).changed(),
        NbtTag::Double(v) => ui.add(egui::DragValue::new(v).speed(0.1)).changed(),
        NbtTag::String(s) => ui.text_edit_singleline(s).changed(),
        NbtTag::ByteArray(arr) => {
            ui.label(format!("[{} bytes]", arr.len()));
            false
        }
        NbtTag::IntArray(arr) => {
            ui.label(format!("[{} ints]", arr.len()));
            false
        }
        NbtTag::LongArray(arr) => {
            ui.label(format!("[{} longs]", arr.len()));
            false
        }
        _ => false,
    }
}
