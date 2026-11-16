# The browser: Sounds and Nodes

Follows the Sounds panel (ADR 0012, PR #524), whose roadmap line said Nodes,
Groups and Presets would join it as tabs of the same browser. The first cut
(#533) had four tabs; the follow-up merged Nodes and Groups, and moved Presets
into the properties panel.

**What landed.** The Sounds panel is no longer a floating window. It is a tab of
the browser, one panel at the left of the window (`noodle-app/src/browser.rs`),
shown from View > Browser. The whole panel collapses to a thin strip (◀ at its
top right) and opens from the strip (▶), so it can stay out of the canvas's way;
Cmd/Ctrl+B shows or hides it, and the palette has "Show or Hide Browser" and "Collapse or Expand Browser". It is shown the first time Noodle starts, and afterwards as it was left (`Prefs::browser_open`; off when no preferences are loaded, so tests and plugin hosts start without it). The
panel's width can be dragged.

- **Sounds:** unchanged (search, filters, audition, drag onto a Sampler, pad,
  canvas or timeline). File > Browse Sounds…, the Sampler's "Browse sounds…"
  button and the palette open the panel on this tab, out of its strip. The
  sources are first asked what they have when the tab is first on screen, not
  when the panel is hidden.
- **Nodes** (nodes and groups in one tab): every node type by category, then
  the library's saved groups and an empty Group, with one search box. It lists
  what the Add-node search (Shift+A) lists, from the same function
  (`editor::search_entries`). A click adds the item at the centre of the canvas;
  a drag adds it where it is dropped. Both go through `EditorState::request_add`
  and the `add_choice` function the search also uses, so a node added here is
  made exactly as one added there (initial config, an Automation node's lane, the
  Track Input notice, one undo step).
- **Presets of a node or group** unfold from its row (the ▸ arrow). A node row
  lists the node presets made for its type, a group row the group presets with
  its topology (`Session::presets_for_type`, `presets_for_template`). Clicking or
  dragging a preset adds the node or group and then applies the preset, as the
  Presets section does, so adding with a preset is two undo steps (the add, then
  the preset).
- **Presets in the properties panel:** a "Presets" section (open by default)
  replaces the old dropdown. It lists the presets that fit the selected node or
  group under System and User, with a search box once there are more than six,
  ticks the one applied ("modified" once its values are edited), and keeps Save
  and Save as…. A click applies one as one undo step.

Tests: `app.rs` (the panel shows and collapses and opens again, a request for
sounds opens it out of the strip, a click on a node and on a group, a preset
unfolded from a group and from a node adds it with the preset applied, both
lists scroll with the wheel, the Sounds tab's Add source menu opens Settings on Manage sources) and
`properties.rs` (the section). Story routes S31 to S33 open the sounds from
View > Browser. The lists' scroll bars are always shown, so they can be dragged.

**Owed.** Whether the panel is shown is remembered between runs, but whether it is collapsed and which tab is open are not. A preset can't be dragged onto a node already on the canvas (use the
properties section).

## Fold arrows

The ▸/▾ arrows that unfold a row's presets are painted triangles
(`browser::fold_arrow`), not text, so they don't depend on the font having the
glyphs.
