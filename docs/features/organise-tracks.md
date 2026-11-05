# Organising a large session (#367, story S29)

What landed:

- **Track colours.** `Organisation` (`noodle-core/src/organise.rs`) stores a palette slot per track group, not RGB and not a lane index, so reordering never recolours a track and a scheme change is one table (`theme::timeline::PALETTE`). A track with no choice takes its folder's colour, else one by when it was made. Set from the header menu.
- **Folders.** Display folders: a folder gathers tracks under a header strip that folds. Tracks stay in `track_order`; the arrangement draws a folder's tracks together. One command, `Command::SetOrganisation`, replaces the whole structure, so regrouping is a single undo step and never touches routing. Dragging a header between two tracks of a folder puts it in. Saved in the project only when used.
- **No track hiding.** A lane drags (or double-clicks on its bottom edge) down to just its name, with no controls. A track out of the way goes in a folder that is folded.
- **Command palette (Cmd+K) and keymap.** Every menu and transport action is registered once in `app/commands.rs` with a name. The palette searches them; "Edit shortcuts" rebinds any of them. Only changed chords are saved (`keymap` in the preferences). A chord belongs to one command: binding it takes it from the old owner. This also makes features that are not nodes findable by name (#337).
- **Project templates.** File > "Save as Project Template…" writes the project without its clips (and its samples) to the user's `templates` folder; File > "New from Template" starts an untitled copy. A template never updates the projects made from it, not even by a command.
- Macros are dropped; the goal became "key commands for repetitive edits".

Owed: a colour-scheme preference that swaps the palette (the slots are stored, so it is one table), multi-select then colour in one gesture, and routing the "regroup the folders" late change.
