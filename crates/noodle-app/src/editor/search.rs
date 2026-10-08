//! Shift+A: search for a node type, and add it where the pointer was.

use egui::{Key, Pos2};
use noodle_core::group::{GROUP, GROUP_INPUT, GROUP_OUTPUT, GROUP_STAGE};
use noodle_engine::Registry;

/// The add-node search box.
pub struct Search {
    /// Where the new node goes, in graph coordinates.
    pub at: Pos2,
    /// Where the box is drawn.
    screen: Pos2,
    query: String,
    highlighted: usize,
    focused: bool,
    /// Whether the editor is inside a group, which is where group inputs and
    /// outputs make sense.
    in_group: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Node(&'static str),
    Frame,
}

pub enum Outcome {
    Open,
    Closed,
    Chosen(Choice),
}

#[derive(Debug, PartialEq)]
pub struct Entry {
    pub name: &'static str,
    pub category: &'static str,
    pub choice: Choice,
}

/// Everything that can be added that matches every word of `query`, by name,
/// category or ID. Those whose name matches come first, then they're ordered
/// by category and name.
pub fn entries(registry: &Registry, query: &str, in_group: bool) -> Vec<Entry> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut entries: Vec<Entry> = registry
        .iter()
        .filter(|t| t.info().id != GROUP_STAGE)
        .map(|t| {
            let info = t.info();
            Entry {
                name: info.name,
                category: info.category,
                choice: Choice::Node(info.id),
            }
        })
        .chain([Entry {
            name: "Group",
            category: "Group",
            choice: Choice::Node(GROUP),
        }])
        // Frames belong to the top level for now, so inside a group there
        // would be nowhere to see or delete one.
        .chain((!in_group).then_some(Entry {
            name: "Frame",
            category: "Layout",
            choice: Choice::Frame,
        }))
        .chain(
            in_group
                .then_some([
                    Entry {
                        name: "Group Input",
                        category: "Group",
                        choice: Choice::Node(GROUP_INPUT),
                    },
                    Entry {
                        name: "Group Output",
                        category: "Group",
                        choice: Choice::Node(GROUP_OUTPUT),
                    },
                ])
                .into_iter()
                .flatten(),
        )
        .filter(|entry| {
            let id = match entry.choice {
                Choice::Node(id) => id,
                Choice::Frame => "",
            };
            let haystack = format!("{} {} {id}", entry.name, entry.category).to_lowercase();
            words.iter().all(|word| haystack.contains(word.as_str()))
        })
        .collect();
    // Names that match every word come first, so "output" finds Output
    // before Input, whose category is "Input/Output".
    entries.sort_by_key(|e| {
        let name = e.name.to_lowercase();
        let by_name = words.iter().all(|word| name.contains(word.as_str()));
        (!by_name, e.category, e.name)
    });
    entries
}

impl Search {
    pub fn new(at: Pos2, screen: Pos2, in_group: bool) -> Self {
        Self {
            at,
            screen,
            query: String::new(),
            highlighted: 0,
            focused: false,
            in_group,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, registry: &Registry) -> Outcome {
        let mut outcome = Outcome::Open;
        let response = egui::Area::new(egui::Id::new("noodle-editor-add-node"))
            .fixed_pos(self.screen)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(220.0);
                    ui.strong("Add");
                    let (up, down, enter, escape) = ui.input_mut(|i| {
                        (
                            i.consume_key(egui::Modifiers::NONE, Key::ArrowUp),
                            i.consume_key(egui::Modifiers::NONE, Key::ArrowDown),
                            i.key_pressed(Key::Enter),
                            i.key_pressed(Key::Escape),
                        )
                    });
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut self.query)
                            .hint_text("Search")
                            .desired_width(f32::INFINITY),
                    );
                    if !self.focused {
                        edit.request_focus();
                        self.focused = true;
                    }
                    if edit.changed() {
                        self.highlighted = 0;
                    }

                    let entries = entries(registry, &self.query, self.in_group);
                    let last = entries.len().saturating_sub(1);
                    if down {
                        self.highlighted = (self.highlighted + 1).min(last);
                    }
                    if up {
                        self.highlighted = self.highlighted.saturating_sub(1);
                    }
                    self.highlighted = self.highlighted.min(last);

                    if escape {
                        outcome = Outcome::Closed;
                    } else if enter {
                        outcome = entries
                            .get(self.highlighted)
                            .map_or(Outcome::Closed, |e| Outcome::Chosen(e.choice));
                    }

                    ui.separator();
                    if entries.is_empty() {
                        ui.weak("Nothing matches");
                    }
                    egui::ScrollArea::vertical()
                        .max_height(300.0)
                        .show(ui, |ui| {
                            for (i, entry) in entries.iter().enumerate() {
                                let text = format!("{}  ·  {}", entry.name, entry.category);
                                let label = ui.selectable_label(i == self.highlighted, text);
                                if i == self.highlighted && (up || down) {
                                    label.scroll_to_me(None);
                                }
                                if label.clicked() {
                                    outcome = Outcome::Chosen(entry.choice);
                                }
                            }
                        });
                });
            })
            .response;

        // A click anywhere else closes it.
        let clicked_outside = ctx.input(|i| {
            i.pointer.any_pressed()
                && i.pointer
                    .interact_pos()
                    .is_some_and(|p| !response.rect.contains(p))
        });
        if clicked_outside && matches!(outcome, Outcome::Open) {
            outcome = Outcome::Closed;
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> Registry {
        crate::session::Nodes::all().registry
    }

    #[test]
    fn every_word_must_match_the_name_category_or_id() {
        let registry = registry();
        let names = |query| -> Vec<&str> {
            entries(&registry, query, false)
                .iter()
                .map(|e| e.name)
                .collect()
        };
        assert_eq!(names("gain"), ["Gain"]);
        assert_eq!(names("GEN sine"), ["Sine"]);
        // By ID.
        assert_eq!(names("util.mix"), ["Mix"]);
        assert!(names("frame").contains(&"Frame"));
        assert!(names("sine zzz").is_empty());
    }

    #[test]
    fn group_ports_are_offered_only_inside_a_group() {
        let registry = registry();
        let outside = entries(&registry, "group", false);
        assert_eq!(outside.len(), 1);
        let inside = entries(&registry, "group", true);
        assert_eq!(inside.len(), 3);
        // Frames are only offered at the top level.
        assert!(
            entries(&registry, "frame", false)
                .iter()
                .any(|e| e.name == "Frame")
        );
        assert!(
            entries(&registry, "frame", true)
                .iter()
                .all(|e| e.name != "Frame")
        );
    }

    #[test]
    fn name_matches_come_first() {
        let registry = registry();
        let names = entries(&registry, "output", false);
        assert_eq!(names[0].name, "Output");
        assert!(names.iter().any(|e| e.name == "Input"), "by category");
    }

    #[test]
    fn everything_is_listed_by_category() {
        let registry = registry();
        let all = entries(&registry, "", false);
        assert_eq!(
            all.len(),
            registry.iter().count() + 1,
            "the types less the internal group stage, plus Frame and Group"
        );
        assert!(
            all.windows(2)
                .all(|w| (w[0].category, w[0].name) <= (w[1].category, w[1].name))
        );
    }
}
