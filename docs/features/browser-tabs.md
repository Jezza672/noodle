# The browser: Sounds, Nodes, Groups and Presets

Follows the Sounds panel (ADR 0012, PR #524), whose roadmap line said Nodes,
Groups and Presets would join it as tabs of the same browser.

**What landed.** The Sounds panel is no longer a floating window. It is a tab of
the browser, one panel at the left of the window (`noodle-app/src/browser.rs`),
shown from View > Browser. The whole panel collapses to a thin strip (◀ at its
top right) and opens from the strip (▶), so it can stay out of the canvas's way;
the palette has "Show or Hide Browser" and "Collapse or Expand Browser". The
panel's width can be dragged.

- **Sounds:** unchanged (search, filters, audition, drag onto a Sampler, pad,
  canvas or timeline). File > Browse Sounds…, the Sampler's "Browse sounds…"
  button and the palette open the panel on this tab, out of its strip. The
  sources are first asked what they have when the tab is first on screen, not
  when the panel is hidden.
- **Nodes:** every node type, by category, with a search box. It lists what the
  Add-node search (Shift+A) lists, from the same function
  (`editor::search_entries`). A click adds the node at the centre of the canvas;
  a drag adds it where it is dropped. Both go through `EditorState::request_add`
  and the `add_choice` function the search also uses, so a node added here is
  made exactly as one added there (initial config, an Automation node's lane, the
  Track Input notice, one undo step).
- **Groups:** the library's group templates (system and the user's own) and an
  empty Group, added the same way.
- **Presets:** the presets that fit the selected node or group, with a search
  box. A click applies one as one undo step; the one applied is ticked, and
  marked "modified" once its values are edited.

Tests: `app.rs` (the panel shows and collapses and opens again, a request for
sounds opens it out of the strip, a click in each of Nodes, Groups and Presets).
Story S31's route now opens the sounds from View > Browser.

**Owed.** The panel's open, collapsed and tab state is not remembered between
runs. Presets can be applied only to the selected node; dragging one onto a node
on the canvas, and saving a preset from the tab (it is in the properties panel),
are not built.
