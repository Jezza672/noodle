//! The config field: edits one config setting.
//!
//! Changing config rebuilds the node, so unlike a parameter, a config value
//! is only committed once the user lets go: when a drag ends, or typing
//! finishes. Until then the field keeps the value being edited to itself.

use egui::{Checkbox, DragValue, Id, Response, TextEdit, Ui};
use noodle_core::{Command, NodeId, Value};
use noodle_engine::ConfigInfo;

use crate::session::Edit;

#[derive(Clone, Debug, PartialEq)]
pub enum ConfigEdit {
    Set(Value),
    /// Put the setting back to its default (`SetConfig` with `None`).
    Reset,
}

#[derive(Debug)]
pub struct ConfigOutput {
    pub response: Response,
    pub edit: Option<ConfigEdit>,
}

impl ConfigOutput {
    /// The session edit for this frame, if any, for setting `key` of `node`.
    pub fn edit(&self, node: NodeId, key: &str) -> Option<Edit> {
        let value = match self.edit.as_ref()? {
            ConfigEdit::Set(value) => Some(value.clone()),
            ConfigEdit::Reset => None,
        };
        Some(Edit::Apply(Command::SetConfig {
            node,
            key: key.to_owned(),
            value,
        }))
    }
}

#[must_use = "call `show` to draw the field"]
pub struct ConfigField<'a> {
    id_salt: Id,
    info: &'a ConfigInfo,
    value: Option<&'a Value>,
}

impl<'a> ConfigField<'a> {
    /// `value` is the node's setting, or `None` if it uses the default.
    pub fn new(info: &'a ConfigInfo, value: Option<&'a Value>) -> Self {
        Self {
            id_salt: Id::new(info.key),
            info,
            value,
        }
    }

    /// Distinguishes this field from the same setting on other nodes, e.g.
    /// `(node, key)`. Without it, an edit in progress on one node would carry
    /// over to the next node shown in the same place.
    pub fn id_salt(mut self, salt: impl egui::AsId) -> Self {
        self.id_salt = Id::new(salt);
        self
    }

    pub fn show(self, ui: &mut Ui) -> ConfigOutput {
        let id = ui.make_persistent_id(self.id_salt);
        let committed = self.current();
        // Scoping by `id` keys the editor's own state (focus, drag) too.
        let inner = ui.push_id(id, |ui| {
            ui.horizontal(|ui| {
                ui.label(self.info.name);
                edit_value(ui, id, &committed)
            })
            .inner
        });
        let (mut response, value) = inner.inner;

        let mut edit = value
            .filter(|value| *value != committed)
            .map(ConfigEdit::Set);
        if edit.is_some() {
            response.mark_changed();
        }
        response.context_menu(|ui| {
            if ui.button("Reset to default").clicked() {
                edit = Some(ConfigEdit::Reset);
                ui.close();
            }
        });
        ConfigOutput { response, edit }
    }

    /// The node's value, if it has the default's type, else the default.
    fn current(&self) -> Value {
        match self.value {
            Some(value)
                if std::mem::discriminant(value) == std::mem::discriminant(&self.info.default) =>
            {
                value.clone()
            }
            _ => self.info.default.clone(),
        }
    }
}

/// Draws the editor for `committed`'s type, and returns the value to commit,
/// if the user has finished editing this frame.
fn edit_value(ui: &mut Ui, id: Id, committed: &Value) -> (Response, Option<Value>) {
    let pending_id = id.with("pending");
    let mut value = ui
        .data(|d| d.get_temp::<Value>(pending_id))
        .filter(|v| std::mem::discriminant(v) == std::mem::discriminant(committed))
        .unwrap_or_else(|| committed.clone());

    let response = match &mut value {
        Value::Bool(b) => ui.add(Checkbox::without_text(b)),
        Value::Int(i) => ui.add(DragValue::new(i).speed(0.05)),
        Value::Float(f) => ui.add(DragValue::new(f).speed(0.01)),
        Value::Text(text) => ui.add(TextEdit::singleline(text).id(id.with("text"))),
    };

    // A checkbox commits as soon as it's toggled, even while it has focus.
    let typing = response.has_focus() && !matches!(committed, Value::Bool(_));
    if response.dragged() || typing {
        ui.data_mut(|d| d.insert_temp(pending_id, value));
        return (response, None);
    }
    ui.data_mut(|d| d.remove::<Value>(pending_id));
    // Only a gesture finishing commits. A value left pending by a field that
    // stopped being drawn mid-edit is dropped, not committed later.
    let finished = match committed {
        Value::Bool(_) => response.changed(),
        _ => response.drag_stopped() || response.lost_focus(),
    };
    (response, finished.then_some(value))
}
