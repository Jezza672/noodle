//! The editor driven like a user would: pointer and keyboard events into a
//! real [`Session`], checking the project that results.

use egui::{Event, Key, Modifiers, MouseWheelUnit, PointerButton, Pos2, TouchPhase, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use noodle_core::{Command, Connection, Endpoint, Frame, FrameId, Node, NodeId, Position};
use noodle_engine::OUTPUT_ID;
use noodle_nodes::REROUTE_ID;

use super::layout::{NODE_WIDTH, Scene, Side};
use super::{EditorState, show};
use crate::session::{Edit, Session};

struct Rig {
    session: Session,
    editor: EditorState,
    /// Every edit the editor has returned.
    log: Vec<Edit>,
}

type H = Harness<'static, Rig>;

fn rig() -> H {
    rig_with(false)
}

/// `with_button` puts another focusable widget beside the canvas, as the
/// real window has, so Tab has somewhere else to go.
fn rig_with(with_button: bool) -> H {
    let rig = Rig {
        session: Session::new(crate::session::Nodes::all()),
        editor: EditorState::default(),
        log: Vec::new(),
    };
    let mut h = Harness::builder()
        .with_size(Vec2::new(1000.0, 700.0))
        // Like a real display, so two clicks can make a double click.
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            move |ui, rig: &mut Rig| {
                if with_button {
                    let _ = ui.button("Elsewhere");
                }
                let edits = show(ui, &mut rig.editor, &rig.session);
                rig.log.extend(edits.iter().cloned());
                rig.session.edit(edits);
                rig.editor.retain_existing(&rig.session);
            },
            rig,
        );
    h.run();
    h
}

fn add(h: &mut H, node: Node) -> NodeId {
    let session = &mut h.state_mut().session;
    let id = session.new_node_id();
    session.edit([Edit::Apply(Command::AddNode { id, node })]);
    id
}

fn connect(h: &mut H, from: NodeId, from_port: &str, to: NodeId, to_port: &str) {
    h.state_mut()
        .session
        .edit([Edit::Apply(Command::Connect(Connection {
            from: Endpoint::new(from, from_port),
            to: Endpoint::new(to, to_port),
        }))]);
}

fn source(h: &H, node: NodeId, port: &str) -> Option<Endpoint> {
    let graph = h.state().session.project().graph();
    graph.source(&Endpoint::new(node, port)).cloned()
}

fn position(h: &H, node: NodeId) -> Position {
    h.state()
        .session
        .project()
        .graph()
        .node(node)
        .unwrap()
        .position
}

/// Where a graph point is on screen.
fn screen(h: &H, p: Pos2) -> Pos2 {
    let editor = &h.state().editor;
    editor.view.on(editor.canvas).to_screen(p)
}

fn scene(h: &H) -> Scene {
    let session = &h.state().session;
    Scene::build(
        session.project(),
        session.registry(),
        h.state().editor.group,
    )
}

fn socket(h: &H, node: NodeId, side: Side, key: &str) -> Pos2 {
    let scene = scene(h);
    let socket = scene.node(node).unwrap().port(side, key).unwrap().socket;
    screen(h, socket)
}

/// A point inside a node's header.
fn title(h: &H, node: NodeId) -> Pos2 {
    let rect = scene(h).node(node).unwrap().rect;
    screen(h, rect.min + Vec2::new(rect.width() / 2.0, 8.0))
}

/// Presses `button` at the first point, moves through the rest, and releases
/// at the last, holding `modifiers` throughout.
fn drag(h: &mut H, button: PointerButton, modifiers: Modifiers, path: &[Pos2]) {
    h.event(Event::ModifiersChanged(modifiers));
    h.event(Event::PointerMoved(path[0]));
    h.step();
    h.event(Event::PointerButton {
        pos: path[0],
        button,
        pressed: true,
        modifiers,
    });
    h.step();
    for &p in &path[1..] {
        // In small steps, as a real pointer moves.
        h.event(Event::PointerMoved(p));
        h.step();
    }
    let last = *path.last().unwrap();
    h.event(Event::PointerButton {
        pos: last,
        button,
        pressed: false,
        modifiers,
    });
    h.step();
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.run();
}

fn click(h: &mut H, p: Pos2, modifiers: Modifiers) {
    drag(h, PointerButton::Primary, modifiers, &[p]);
}

fn press(h: &mut H, at: Pos2, modifiers: Modifiers, key: Key) {
    h.hover_at(at);
    h.step();
    h.key_press_modifiers(modifiers, key);
    h.run();
}

fn empty_space(h: &H) -> Pos2 {
    screen(h, Pos2::new(700.0, 550.0))
}

#[test]
fn draws_every_kind_of_thing() {
    let mut h = rig();
    let sine = add(&mut h, Node::new("noodle.osc.sine").at(0.0, 0.0));
    let reroute = add(&mut h, Node::new(REROUTE_ID).at(220.0, 40.0));
    let unknown = add(&mut h, Node::new("no.such.type").at(300.0, 0.0));
    connect(&mut h, sine, "out", reroute, "in");
    connect(&mut h, reroute, "out", unknown, "in");
    h.state_mut().session.edit([Edit::Apply(Command::AddFrame {
        id: FrameId(1),
        frame: Frame {
            label: "Synth".into(),
            position: Position { x: -20.0, y: -50.0 },
            width: 600.0,
            height: 300.0,
        },
    })]);
    h.run();
    // The canvas and three nodes, a frame, two wires and a problem are all
    // drawn without panicking, and the editor changed nothing by itself.
    assert!(h.state().log.is_empty());
    assert_eq!(h.state().session.diagnostics().len(), 1);
}

#[test]
fn dragging_from_an_output_to_an_input_connects_them() {
    let mut h = rig();
    let sine = add(&mut h, Node::new("noodle.osc.sine").at(0.0, 0.0));
    let gain = add(&mut h, Node::new("noodle.util.gain").at(300.0, 0.0));
    h.run();
    let from = socket(&h, sine, Side::Output, "out");
    let to = socket(&h, gain, Side::Input, "in");
    drag(
        &mut h,
        PointerButton::Primary,
        Modifiers::NONE,
        &[from, from.lerp(to, 0.5), to],
    );
    assert_eq!(source(&h, gain, "in"), Some(Endpoint::new(sine, "out")));
    assert_eq!(
        h.state().log,
        [Edit::Apply(Command::Connect(Connection {
            from: Endpoint::new(sine, "out"),
            to: Endpoint::new(gain, "in"),
        }))]
    );
}

#[test]
fn dragging_from_an_input_to_an_output_connects_them_too() {
    let mut h = rig();
    let sine = add(&mut h, Node::new("noodle.osc.sine").at(0.0, 0.0));
    let gain = add(&mut h, Node::new("noodle.util.gain").at(300.0, 0.0));
    h.run();
    let from = socket(&h, gain, Side::Input, "gain");
    // Dropped anywhere on the node: its only output.
    let to = title(&h, sine);
    drag(
        &mut h,
        PointerButton::Primary,
        Modifiers::NONE,
        &[from, from.lerp(to, 0.5), to],
    );
    assert_eq!(source(&h, gain, "gain"), Some(Endpoint::new(sine, "out")));
}

#[test]
fn a_connected_input_picks_its_wire_up() {
    let mut h = rig();
    let sine = add(&mut h, Node::new("noodle.osc.sine").at(0.0, 0.0));
    let gain = add(&mut h, Node::new("noodle.util.gain").at(300.0, 0.0));
    connect(&mut h, sine, "out", gain, "in");
    h.run();

    // Move the wire's end to another input.
    let from = socket(&h, gain, Side::Input, "in");
    let to = socket(&h, gain, Side::Input, "gain");
    drag(
        &mut h,
        PointerButton::Primary,
        Modifiers::NONE,
        &[from, from + Vec2::new(-40.0, 30.0), to],
    );
    assert_eq!(source(&h, gain, "in"), None);
    assert_eq!(source(&h, gain, "gain"), Some(Endpoint::new(sine, "out")));
    h.state_mut().session.undo();
    assert_eq!(
        source(&h, gain, "in"),
        Some(Endpoint::new(sine, "out")),
        "one undo step"
    );

    // Drop it on nothing to disconnect.
    let from = socket(&h, gain, Side::Input, "in");
    let away = empty_space(&h);
    drag(
        &mut h,
        PointerButton::Primary,
        Modifiers::NONE,
        &[from, from.lerp(away, 0.5), away],
    );
    assert_eq!(source(&h, gain, "in"), None);
    assert_eq!(h.state().session.project().graph().connections().count(), 0);
}

#[test]
fn an_audio_output_cant_connect_to_its_own_node() {
    let mut h = rig();
    let gain = add(&mut h, Node::new("noodle.util.gain").at(0.0, 0.0));
    h.run();
    let from = socket(&h, gain, Side::Output, "out");
    let to = socket(&h, gain, Side::Input, "in");
    drag(
        &mut h,
        PointerButton::Primary,
        Modifiers::NONE,
        &[from, from + Vec2::new(30.0, 50.0), to],
    );
    assert!(h.state().log.is_empty());
}

#[test]
fn dragging_a_node_moves_it_as_one_undo_step() {
    let mut h = rig();
    let sine = add(&mut h, Node::new("noodle.osc.sine").at(10.0, 20.0));
    let other = add(&mut h, Node::new("noodle.osc.sine").at(300.0, 20.0));
    h.run();
    let start = title(&h, sine);
    drag(
        &mut h,
        PointerButton::Primary,
        Modifiers::NONE,
        &[
            start,
            start + Vec2::new(20.0, 10.0),
            start + Vec2::new(50.0, 40.0),
        ],
    );
    assert_eq!(position(&h, sine), Position { x: 60.0, y: 60.0 });
    assert_eq!(position(&h, other), Position { x: 300.0, y: 20.0 });
    assert!(matches!(h.state().log.last(), Some(Edit::EndDrag)));
    assert!(h.state().log.iter().any(|e| matches!(e, Edit::Drag(_))));
    assert_eq!(
        h.state().editor.selected.iter().collect::<Vec<_>>(),
        [&sine]
    );
    assert_eq!(h.state().editor.active, Some(sine));

    h.state_mut().session.undo();
    assert_eq!(position(&h, sine), Position { x: 10.0, y: 20.0 });
}

#[test]
fn moving_follows_the_zoom() {
    let mut h = rig();
    let sine = add(&mut h, Node::new("noodle.osc.sine").at(0.0, 0.0));
    h.state_mut().editor.view.zoom = 2.0;
    h.run();
    let start = title(&h, sine);
    drag(
        &mut h,
        PointerButton::Primary,
        Modifiers::NONE,
        &[
            start,
            start + Vec2::new(30.0, 0.0),
            start + Vec2::new(100.0, 0.0),
        ],
    );
    assert_eq!(position(&h, sine), Position { x: 50.0, y: 0.0 });
}

#[test]
fn box_select_takes_the_nodes_it_touches() {
    let mut h = rig();
    let a = add(&mut h, Node::new("noodle.osc.sine").at(0.0, 0.0));
    let b = add(&mut h, Node::new("noodle.osc.sine").at(250.0, 0.0));
    let c = add(&mut h, Node::new("noodle.osc.sine").at(0.0, 300.0));
    h.run();
    let start = screen(&h, Pos2::new(-20.0, -20.0));
    // Across the first node and into the corner of the second.
    let end = screen(&h, Pos2::new(260.0, 30.0));
    drag(
        &mut h,
        PointerButton::Primary,
        Modifiers::NONE,
        &[start, start.lerp(end, 0.5), end],
    );
    let selected: Vec<_> = h.state().editor.selected.iter().copied().collect();
    assert_eq!(selected, [a, b]);

    // Shift adds.
    let start = screen(&h, Pos2::new(-20.0, 280.0));
    let end = screen(&h, Pos2::new(20.0, 320.0));
    drag(
        &mut h,
        PointerButton::Primary,
        Modifiers::SHIFT,
        &[start, start.lerp(end, 0.5), end],
    );
    assert_eq!(h.state().editor.selected.len(), 3);

    // Ctrl removes.
    let start = screen(&h, Pos2::new(240.0, -20.0));
    let end = screen(&h, Pos2::new(270.0, 20.0));
    drag(
        &mut h,
        PointerButton::Primary,
        Modifiers::COMMAND,
        &[start, start.lerp(end, 0.5), end],
    );
    let selected: Vec<_> = h.state().editor.selected.iter().copied().collect();
    assert_eq!(selected, [a, c]);
}

#[test]
fn clicking_selects_and_shift_toggles() {
    let mut h = rig();
    let a = add(&mut h, Node::new("noodle.osc.sine").at(0.0, 0.0));
    let b = add(&mut h, Node::new("noodle.osc.sine").at(250.0, 0.0));
    h.run();
    let p = title(&h, a);
    click(&mut h, p, Modifiers::NONE);
    assert_eq!(h.state().editor.active, Some(a));
    let p = title(&h, b);
    click(&mut h, p, Modifiers::SHIFT);
    assert_eq!(h.state().editor.selected.len(), 2);
    assert_eq!(h.state().editor.active, Some(b));
    let p = title(&h, a);
    click(&mut h, p, Modifiers::SHIFT);
    assert_eq!(h.state().editor.selected.iter().collect::<Vec<_>>(), [&b]);
    let p = empty_space(&h);
    click(&mut h, p, Modifiers::NONE);
    assert!(h.state().editor.selected.is_empty());
    assert_eq!(h.state().editor.active, None);
}

/// A sine into a gain, and a straight vertical stroke across the wire.
fn wired(h: &mut H) -> (NodeId, NodeId, [Pos2; 3]) {
    let sine = add(h, Node::new("noodle.osc.sine").at(0.0, 0.0));
    let gain = add(h, Node::new("noodle.util.gain").at(400.0, 0.0));
    connect(h, sine, "out", gain, "in");
    h.run();
    let from = socket(h, sine, Side::Output, "out");
    let to = socket(h, gain, Side::Input, "in");
    let x = (from.x + to.x) / 2.0;
    let mid = (from.y + to.y) / 2.0;
    (
        sine,
        gain,
        [
            Pos2::new(x, mid - 60.0),
            Pos2::new(x, mid),
            Pos2::new(x, mid + 60.0),
        ],
    )
}

#[test]
fn ctrl_right_drag_cuts_the_wires_it_crosses() {
    let mut h = rig();
    let (_, gain, stroke) = wired(&mut h);
    // A plain right-drag does nothing.
    drag(&mut h, PointerButton::Secondary, Modifiers::NONE, &stroke);
    assert!(source(&h, gain, "in").is_some());
    drag(
        &mut h,
        PointerButton::Secondary,
        Modifiers::COMMAND,
        &stroke,
    );
    assert_eq!(source(&h, gain, "in"), None);
}

#[test]
fn a_cut_that_misses_changes_nothing() {
    let mut h = rig();
    let (_, gain, stroke) = wired(&mut h);
    let beside = stroke.map(|p| p + Vec2::new(0.0, 200.0));
    drag(
        &mut h,
        PointerButton::Secondary,
        Modifiers::COMMAND,
        &beside,
    );
    assert!(source(&h, gain, "in").is_some());
    assert!(h.state().log.is_empty());
}

#[test]
fn shift_right_drag_puts_a_reroute_on_the_wire() {
    let mut h = rig();
    let (sine, gain, stroke) = wired(&mut h);
    drag(&mut h, PointerButton::Secondary, Modifiers::SHIFT, &stroke);
    let reroute = source(&h, gain, "in").unwrap().node;
    let graph = h.state().session.project().graph();
    assert_eq!(graph.node(reroute).unwrap().type_id, REROUTE_ID);
    assert_eq!(source(&h, reroute, "in"), Some(Endpoint::new(sine, "out")));
    assert!(h.state().session.diagnostics().is_empty());
    // Centred where the stroke crossed.
    let rect = scene(&h).node(reroute).unwrap().rect;
    assert!((screen(&h, rect.center()) - stroke[1]).length() < 2.0);
    h.state_mut().session.undo();
    assert_eq!(source(&h, gain, "in"), Some(Endpoint::new(sine, "out")));
}

#[test]
fn x_deletes_the_selection_and_undo_brings_it_back() {
    let mut h = rig();
    let (sine, gain, _) = wired(&mut h);
    let p = title(&h, sine);
    click(&mut h, p, Modifiers::NONE);
    let before = h.state().session.project().clone();
    let p = empty_space(&h);
    press(&mut h, p, Modifiers::NONE, Key::X);
    let graph = h.state().session.project().graph();
    assert!(graph.node(sine).is_none());
    assert!(graph.node(gain).is_some());
    assert_eq!(graph.connections().count(), 0);
    assert!(h.state().editor.selected.is_empty());
    h.state_mut().session.undo();
    assert_eq!(h.state().session.project(), &before);
}

#[test]
fn backspace_deletes_the_selection_like_delete() {
    let mut h = rig();
    let (sine, _, _) = wired(&mut h);
    let p = title(&h, sine);
    click(&mut h, p, Modifiers::NONE);
    let p = empty_space(&h);
    press(&mut h, p, Modifiers::NONE, Key::Backspace);
    assert!(h.state().session.project().graph().node(sine).is_none());
}

fn double_click(h: &mut H, at: Pos2) {
    h.event(Event::PointerMoved(at));
    for pressed in [true, false, true, false] {
        h.event(Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    h.step();
    h.run();
}

#[test]
fn double_clicking_a_wire_breaks_it_as_one_undo_step() {
    let mut h = rig();
    let (sine, gain, stroke) = wired(&mut h);
    double_click(&mut h, stroke[1]);
    assert_eq!(source(&h, gain, "in"), None);
    h.state_mut().session.undo();
    assert_eq!(source(&h, gain, "in"), Some(Endpoint::new(sine, "out")));
}

#[test]
fn double_clicking_beside_a_wire_changes_nothing() {
    let mut h = rig();
    let (_, gain, stroke) = wired(&mut h);
    double_click(&mut h, stroke[1] + Vec2::new(0.0, 200.0));
    assert!(source(&h, gain, "in").is_some());
}

#[test]
fn delete_with_nothing_selected_does_nothing() {
    let mut h = rig();
    wired(&mut h);
    let p = empty_space(&h);
    press(&mut h, p, Modifiers::NONE, Key::Delete);
    assert!(h.state().log.is_empty());
}

#[test]
fn shift_d_duplicates_with_the_wires_between_the_copies() {
    let mut h = rig();
    let (sine, gain, _) = wired(&mut h);
    let output = add(&mut h, Node::new(OUTPUT_ID).at(700.0, 0.0));
    connect(&mut h, gain, "out", output, "in");
    h.run();
    // Select the sine and the gain, but not the output.
    let p = title(&h, sine);
    click(&mut h, p, Modifiers::NONE);
    let p = title(&h, gain);
    click(&mut h, p, Modifiers::SHIFT);
    let p = empty_space(&h);
    press(&mut h, p, Modifiers::SHIFT, Key::D);

    let graph = h.state().session.project().graph();
    assert_eq!(graph.nodes().count(), 5);
    let copies: Vec<NodeId> = h.state().editor.selected.iter().copied().collect();
    assert_eq!(copies.len(), 2);
    assert!(!copies.contains(&sine) && !copies.contains(&gain));
    let (sine_copy, gain_copy) = (copies[0], copies[1]);
    assert_eq!(graph.node(sine_copy).unwrap().type_id, "noodle.osc.sine");
    assert_eq!(
        source(&h, gain_copy, "in"),
        Some(Endpoint::new(sine_copy, "out"))
    );
    // The output still listens to the original.
    assert_eq!(source(&h, output, "in"), Some(Endpoint::new(gain, "out")));
    assert_eq!(position(&h, sine_copy), Position { x: 30.0, y: 30.0 });
    assert_eq!(h.state().editor.active, Some(gain_copy));

    // One undo removes both copies.
    h.state_mut().session.undo();
    assert_eq!(h.state().session.project().graph().nodes().count(), 3);
}

#[test]
fn shift_d_on_a_frame_copies_what_is_in_it() {
    let mut h = rig();
    let (sine, gain, _) = wired(&mut h);
    let outside = add(&mut h, Node::new("noodle.osc.sine").at(0.0, 400.0));
    h.run();
    let p = title(&h, sine);
    click(&mut h, p, Modifiers::NONE);
    let p = title(&h, gain);
    click(&mut h, p, Modifiers::SHIFT);
    let p = empty_space(&h);
    press(&mut h, p, Modifiers::COMMAND, Key::J);
    let (frame_id, frame) = {
        let (id, frame) = h.state().session.project().frames().next().unwrap();
        (id, frame.clone())
    };

    // Select just the frame, by its title, and copy it.
    let p = empty_space(&h);
    click(&mut h, p, Modifiers::NONE);
    let label = screen(
        &h,
        Pos2::new(frame.position.x + 30.0, frame.position.y + 8.0),
    );
    click(&mut h, label, Modifiers::NONE);
    assert!(h.state().editor.selected.is_empty());
    let p = empty_space(&h);
    press(&mut h, p, Modifiers::SHIFT, Key::D);

    let project = h.state().session.project();
    assert_eq!(project.frames().count(), 2);
    // The sine and gain are copied, with the wire between them, and the
    // sine outside the frame isn't.
    assert_eq!(project.graph().nodes().count(), 5);
    assert_eq!(project.graph().connections().count(), 2);
    let (copy_id, copy) = project.frames().find(|(id, _)| *id != frame_id).unwrap();
    // Below the original, not overlapping it.
    assert_eq!(copy.position.x, frame.position.x);
    assert_eq!(copy.position.y, frame.position.y + frame.height + 20.0);
    assert_eq!(copy.width, frame.width);
    let copies: Vec<NodeId> = project
        .graph()
        .nodes()
        .map(|(id, _)| id)
        .filter(|id| ![sine, gain, outside].contains(id))
        .collect();
    assert_eq!(copies.len(), 2);
    for id in &copies {
        let node = project.graph().node(*id).unwrap();
        let original = if node.type_id == "noodle.osc.sine" {
            sine
        } else {
            gain
        };
        let moved = project.graph().node(original).unwrap().position;
        assert_eq!(node.position.x, moved.x);
        assert_eq!(node.position.y, moved.y + frame.height + 20.0);
    }
    assert_eq!(position(&h, sine), Position::default());
    // The copy of the frame is what's selected, and dragging it carries
    // the copied nodes.
    assert_eq!(
        h.state().editor.selected_frames.iter().collect::<Vec<_>>(),
        [&copy_id]
    );
    assert!(h.state().editor.selected.is_empty());

    // Dragging the copy by its title moves the copied nodes and leaves the
    // originals where they were.
    let (sine_at, gain_at) = (position(&h, sine), position(&h, gain));
    let copies_at: Vec<Position> = copies.iter().map(|&id| position(&h, id)).collect();
    let label = screen(&h, Pos2::new(copy.position.x + 30.0, copy.position.y + 8.0));
    drag(
        &mut h,
        PointerButton::Primary,
        Modifiers::NONE,
        &[
            label,
            label + Vec2::new(10.0, 0.0),
            label + Vec2::new(10.0, 150.0),
        ],
    );
    assert_eq!(position(&h, sine), sine_at);
    assert_eq!(position(&h, gain), gain_at);
    assert_eq!(position(&h, outside), Position { x: 0.0, y: 400.0 });
    for (&id, at) in copies.iter().zip(&copies_at) {
        let now = position(&h, id);
        assert_eq!((now.x, now.y), (at.x + 10.0, at.y + 150.0));
    }
    h.state_mut().session.undo();

    // One more undo removes all of the copy.
    h.state_mut().session.undo();
    let project = h.state().session.project();
    assert_eq!(project.frames().count(), 1);
    assert_eq!(project.graph().nodes().count(), 3);
}

#[test]
fn shift_a_searches_and_adds_at_the_pointer() {
    let mut h = rig();
    let at = screen(&h, Pos2::new(120.0, 80.0));
    press(&mut h, at, Modifiers::SHIFT, Key::A);
    assert!(h.state().editor.search.is_some());
    // Typing doesn't trigger the editor's shortcuts, such as X.
    h.event(Event::Text("x gain".into()));
    h.run();
    h.event(Event::Key {
        key: Key::Backspace,
        pressed: true,
        modifiers: Modifiers::NONE,
        repeat: false,
        physical_key: None,
    });
    h.run();
    h.key_press(Key::Enter);
    h.run();
    // "x gai" still finds nothing, so Enter just closes it.
    assert!(h.state().editor.search.is_none());
    assert!(h.state().log.is_empty());

    press(&mut h, at, Modifiers::SHIFT, Key::A);
    h.event(Event::Text("gain".into()));
    h.run();
    h.key_press(Key::Enter);
    h.run();
    let graph = h.state().session.project().graph();
    let (id, node) = graph.nodes().next().expect("a node was added");
    assert_eq!(node.type_id, "noodle.util.gain");
    assert_eq!(node.position, Position { x: 120.0, y: 80.0 });
    assert_eq!(h.state().editor.active, Some(id));
    assert!(h.state().editor.search.is_none());
}

#[test]
fn escape_closes_the_search() {
    let mut h = rig();
    let p = empty_space(&h);
    press(&mut h, p, Modifiers::SHIFT, Key::A);
    assert!(h.state().editor.search.is_some());
    h.key_press(Key::Escape);
    h.run();
    assert!(h.state().editor.search.is_none());
    assert!(h.state().log.is_empty());
}

#[test]
fn the_search_box_can_be_driven_with_arrows() {
    let mut h = rig();
    let p = empty_space(&h);
    press(&mut h, p, Modifiers::SHIFT, Key::A);
    // "osc" matches Saw then Sine.
    h.event(Event::Text("osc".into()));
    h.run();
    h.key_press(Key::ArrowDown);
    h.run();
    h.key_press(Key::Enter);
    h.run();
    let graph = h.state().session.project().graph();
    assert_eq!(graph.nodes().next().unwrap().1.type_id, "noodle.osc.sine");
}

fn wheel(h: &mut H, at: Pos2, unit: MouseWheelUnit, delta: Vec2) {
    h.hover_at(at);
    h.event(Event::MouseWheel {
        unit,
        delta,
        phase: TouchPhase::Move,
        modifiers: Modifiers::NONE,
    });
    // Not run(): egui keeps repainting while it smooths the scroll, which the
    // editor doesn't use.
    h.step();
}

#[test]
fn the_wheel_zooms_around_the_pointer_and_trackpads_pan() {
    let mut h = rig();
    let anchor = Pos2::new(400.0, 300.0);
    let under = {
        let editor = &h.state().editor;
        editor.view.on(editor.canvas).to_graph(anchor)
    };
    wheel(&mut h, anchor, MouseWheelUnit::Line, Vec2::new(0.0, 2.0));
    let editor = &h.state().editor;
    assert!(editor.view.zoom > 1.2, "{}", editor.view.zoom);
    assert!((screen(&h, under) - anchor).length() < 0.01);

    let zoom = h.state().editor.view.zoom;
    let before = screen(&h, Pos2::ZERO);
    wheel(
        &mut h,
        anchor,
        MouseWheelUnit::Point,
        Vec2::new(30.0, -20.0),
    );
    assert_eq!(h.state().editor.view.zoom, zoom);
    assert!((screen(&h, Pos2::ZERO) - (before + Vec2::new(30.0, -20.0))).length() < 0.01);
}

#[test]
fn middle_drag_pans() {
    let mut h = rig();
    let sine = add(&mut h, Node::new("noodle.osc.sine").at(0.0, 0.0));
    h.run();
    let before = screen(&h, Pos2::ZERO);
    // Starting on a node still pans, and doesn't move it.
    let start = title(&h, sine);
    drag(
        &mut h,
        PointerButton::Middle,
        Modifiers::NONE,
        &[
            start,
            start + Vec2::new(20.0, 0.0),
            start + Vec2::new(60.0, 25.0),
        ],
    );
    assert!((screen(&h, Pos2::ZERO) - (before + Vec2::new(60.0, 25.0))).length() < 0.01);
    assert_eq!(position(&h, sine), Position::default());
    assert!(h.state().log.is_empty());
}

#[test]
fn home_fits_everything_in_view() {
    let mut h = rig();
    let far = add(&mut h, Node::new("noodle.osc.sine").at(2000.0, 1200.0));
    add(&mut h, Node::new("noodle.osc.sine").at(-1000.0, 0.0));
    h.run();
    let p = empty_space(&h);
    press(&mut h, p, Modifiers::NONE, Key::Home);
    let canvas = h.state().editor.canvas;
    let rect = scene(&h).node(far).unwrap().rect;
    assert!(canvas.contains(screen(&h, rect.max)));
}

#[test]
fn ctrl_j_frames_the_selection_and_the_frame_carries_its_nodes() {
    let mut h = rig();
    let (sine, gain, _) = wired(&mut h);
    let outside = add(&mut h, Node::new("noodle.osc.sine").at(0.0, 400.0));
    h.run();
    let p = title(&h, sine);
    click(&mut h, p, Modifiers::NONE);
    let p = title(&h, gain);
    click(&mut h, p, Modifiers::SHIFT);
    let p = empty_space(&h);
    press(&mut h, p, Modifiers::COMMAND, Key::J);

    let project = h.state().session.project();
    let (id, frame) = project.frames().next().expect("a frame");
    let (id, frame) = (id, frame.clone());
    assert!(frame.position.x < 0.0 && frame.position.y < 0.0);
    assert!(frame.width > 400.0 + NODE_WIDTH);
    assert_eq!(
        h.state().editor.selected_frames.iter().collect::<Vec<_>>(),
        [&id]
    );

    // Drag the frame by its title: the nodes in it come along.
    let p = empty_space(&h);
    click(&mut h, p, Modifiers::NONE);
    let label = screen(
        &h,
        Pos2::new(frame.position.x + 30.0, frame.position.y + 8.0),
    );
    drag(
        &mut h,
        PointerButton::Primary,
        Modifiers::NONE,
        &[
            label,
            label + Vec2::new(10.0, 0.0),
            label + Vec2::new(100.0, 50.0),
        ],
    );
    assert_eq!(position(&h, sine), Position { x: 100.0, y: 50.0 });
    assert_eq!(position(&h, gain), Position { x: 500.0, y: 50.0 });
    assert_eq!(position(&h, outside), Position { x: 0.0, y: 400.0 });
    h.state_mut().session.undo();
    assert_eq!(position(&h, sine), Position::default());
}

#[test]
fn frames_resize_from_the_corner_and_rename_on_double_click() {
    let mut h = rig();
    let frame = Frame {
        label: "Frame".into(),
        position: Position { x: 0.0, y: 0.0 },
        width: 300.0,
        height: 200.0,
    };
    h.state_mut().session.edit([Edit::Apply(Command::AddFrame {
        id: FrameId(1),
        frame,
    })]);
    h.run();
    let corner = screen(&h, Pos2::new(295.0, 195.0));
    drag(
        &mut h,
        PointerButton::Primary,
        Modifiers::NONE,
        &[
            corner,
            corner + Vec2::new(10.0, 10.0),
            corner + Vec2::new(100.0, -500.0),
        ],
    );
    let frame = h
        .state()
        .session
        .project()
        .frame(FrameId(1))
        .unwrap()
        .clone();
    assert_eq!(
        (frame.width, frame.height),
        (400.0, 60.0),
        "clamped to the minimum"
    );

    let label = screen(&h, Pos2::new(40.0, 10.0));
    h.event(Event::PointerMoved(label));
    for pressed in [true, false, true, false] {
        h.event(Event::PointerButton {
            pos: label,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    h.step();
    h.run();
    assert!(h.state().editor.rename.is_some());
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.event(Event::Text("Drums".into()));
    h.run();
    h.key_press(Key::Enter);
    h.run();
    assert!(h.state().editor.rename.is_none());
    assert_eq!(
        h.state().session.project().frame(FrameId(1)).unwrap().label,
        "Drums"
    );
}

#[test]
fn shift_a_can_add_a_frame_and_x_deletes_it() {
    let mut h = rig();
    let p = empty_space(&h);
    press(&mut h, p, Modifiers::SHIFT, Key::A);
    h.event(Event::Text("frame".into()));
    h.run();
    h.key_press(Key::Enter);
    h.run();
    assert_eq!(h.state().session.project().frames().count(), 1);
    let p = empty_space(&h);
    press(&mut h, p, Modifiers::NONE, Key::X);
    assert_eq!(h.state().session.project().frames().count(), 0);
}

#[test]
fn a_and_alt_a_select_all_and_none() {
    let mut h = rig();
    wired(&mut h);
    let p = empty_space(&h);
    press(&mut h, p, Modifiers::NONE, Key::A);
    assert_eq!(h.state().editor.selected.len(), 2);
    let p = empty_space(&h);
    press(&mut h, p, Modifiers::ALT, Key::A);
    assert!(h.state().editor.selected.is_empty());
}

#[test]
fn problems_show_on_the_wire_they_belong_to() {
    let mut h = rig();
    let a = add(&mut h, Node::new("noodle.util.gain").at(0.0, 0.0));
    let b = add(&mut h, Node::new("noodle.util.gain").at(400.0, 0.0));
    connect(&mut h, a, "out", b, "in");
    // Closes a loop, which the compiler drops with a diagnostic on this wire.
    connect(&mut h, b, "out", a, "in");
    h.run();
    let scene = scene(&h);
    let wire = scene
        .wires
        .iter()
        .find(|w| {
            h.state()
                .session
                .diagnostics()
                .iter()
                .any(|d| d.location == noodle_engine::Location::Wire(w.connection.to.clone()))
        })
        .expect("a wire with a problem");
    let middle = super::wire::flatten(super::wire::curve(wire.from, wire.to))[12];
    h.hover_at(screen(&h, middle));
    // Tooltips wait for the pointer to rest.
    for _ in 0..60 {
        h.step();
    }
    h.run();
    assert!(h.query_by_label_contains("loop").is_some());
}

#[test]
fn problems_show_on_the_node_they_belong_to() {
    let mut h = rig();
    // Its ports are fine, but the compiler can't run it yet.
    let node = add(&mut h, Node::new("noodle.offline.reverse").at(0.0, 0.0));
    let unknown = add(&mut h, Node::new("no.such.type").at(0.0, 200.0));
    h.run();
    for (node, text) in [(node, "cached renders"), (unknown, "unknown node type")] {
        let p = title(&h, node);
        h.hover_at(p);
        for _ in 0..60 {
            h.step();
        }
        h.run();
        assert!(h.query_by_label_contains(text).is_some(), "{text}");
    }
}

/// Presses `button` at `start`, moves to `via`, then moves to `end` and
/// releases there in the same frame.
fn drag_ending_in_one_frame(
    h: &mut H,
    button: PointerButton,
    modifiers: Modifiers,
    start: Pos2,
    via: Pos2,
    end: Pos2,
) {
    h.event(Event::ModifiersChanged(modifiers));
    h.event(Event::PointerMoved(start));
    h.event(Event::PointerButton {
        pos: start,
        button,
        pressed: true,
        modifiers,
    });
    h.event(Event::PointerMoved(via));
    h.step();
    h.input_mut().events.extend([
        Event::PointerMoved(end),
        Event::PointerButton {
            pos: end,
            button,
            pressed: false,
            modifiers,
        },
    ]);
    h.step();
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.run();
}

#[test]
fn a_move_released_in_the_same_frame_lands_where_released() {
    let mut h = rig();
    let sine = add(&mut h, Node::new("noodle.osc.sine").at(0.0, 0.0));
    h.run();
    let start = title(&h, sine);
    drag_ending_in_one_frame(
        &mut h,
        PointerButton::Primary,
        Modifiers::NONE,
        start,
        start + Vec2::new(20.0, 0.0),
        start + Vec2::new(70.0, 30.0),
    );
    assert_eq!(position(&h, sine), Position { x: 70.0, y: 30.0 });
    assert!(matches!(h.state().log.last(), Some(Edit::EndDrag)));
}

#[test]
fn a_cut_counts_the_last_short_movement() {
    let mut h = rig();
    let (_, gain, stroke) = wired(&mut h);
    let wire_y = stroke[1].y;
    let x = stroke[1].x;
    // The last step is too short to be sampled while dragging, but it's the
    // one that crosses the wire.
    drag_ending_in_one_frame(
        &mut h,
        PointerButton::Secondary,
        Modifiers::COMMAND,
        Pos2::new(x, wire_y - 60.0),
        Pos2::new(x, wire_y - 1.5),
        Pos2::new(x, wire_y + 1.5),
    );
    assert_eq!(source(&h, gain, "in"), None);
}

#[test]
fn zoomed_out_the_nearest_socket_wins() {
    let mut h = rig();
    let sine = add(&mut h, Node::new("noodle.osc.sine").at(0.0, 0.0));
    let gain = add(&mut h, Node::new("noodle.util.gain").at(300.0, 0.0));
    connect(&mut h, sine, "out", gain, "in");
    h.state_mut().editor.view.zoom = 0.4;
    h.run();
    // Start exactly on `gain`, a row below the connected `in`.
    let from = socket(&h, gain, Side::Input, "gain");
    let to = socket(&h, sine, Side::Output, "out");
    drag(
        &mut h,
        PointerButton::Primary,
        Modifiers::NONE,
        &[from, from.lerp(to, 0.5), to],
    );
    assert_eq!(source(&h, gain, "gain"), Some(Endpoint::new(sine, "out")));
    assert_eq!(
        source(&h, gain, "in"),
        Some(Endpoint::new(sine, "out")),
        "untouched"
    );

    // And dropping exactly on `gain` connects there, not to `in`.
    let other = add(&mut h, Node::new("noodle.osc.sine").at(0.0, 300.0));
    h.run();
    let from = socket(&h, other, Side::Output, "out");
    let to = socket(&h, gain, Side::Input, "gain");
    drag(
        &mut h,
        PointerButton::Primary,
        Modifiers::NONE,
        &[from, from.lerp(to, 0.5), to],
    );
    assert_eq!(source(&h, gain, "gain"), Some(Endpoint::new(other, "out")));
    assert_eq!(source(&h, gain, "in"), Some(Endpoint::new(sine, "out")));
}

#[test]
fn zoomed_out_a_reroute_can_still_be_moved() {
    let mut h = rig();
    let reroute = add(&mut h, Node::new(REROUTE_ID).at(100.0, 100.0));
    h.state_mut().editor.view.zoom = 0.5;
    h.run();
    let center = screen(&h, scene(&h).node(reroute).unwrap().rect.center());
    drag(
        &mut h,
        PointerButton::Primary,
        Modifiers::NONE,
        &[
            center,
            center + Vec2::new(10.0, 0.0),
            center + Vec2::new(20.0, 10.0),
        ],
    );
    assert_eq!(position(&h, reroute), Position { x: 140.0, y: 120.0 });
}

#[test]
fn a_node_in_front_hides_the_sockets_behind_it() {
    let mut h = rig();
    let back = add(&mut h, Node::new("noodle.osc.sine").at(0.0, 0.0));
    // Drawn on top, covering the back node's output socket.
    let front = add(
        &mut h,
        Node::new("noodle.util.gain").at(NODE_WIDTH - 40.0, 10.0),
    );
    h.run();
    let hidden = socket(&h, back, Side::Output, "out");
    drag(
        &mut h,
        PointerButton::Primary,
        Modifiers::NONE,
        &[
            hidden,
            hidden + Vec2::new(10.0, 0.0),
            hidden + Vec2::new(30.0, 0.0),
        ],
    );
    assert_eq!(
        position(&h, front),
        Position {
            x: NODE_WIDTH - 10.0,
            y: 10.0
        }
    );
    assert_eq!(h.state().session.project().graph().connections().count(), 0);
}

fn param(h: &H, node: NodeId, key: &str) -> Option<f32> {
    let graph = h.state().session.project().graph();
    graph.node(node).unwrap().params.get(key).copied()
}

/// The middle of a parameter's field on its node.
fn field(h: &H, node: NodeId, key: &str) -> Pos2 {
    let scene = scene(h);
    let row = scene
        .node(node)
        .unwrap()
        .port(Side::Input, key)
        .unwrap()
        .row;
    screen(h, row.center())
}

#[test]
fn a_parameter_is_dragged_on_its_node_as_one_undo_step() {
    let mut h = rig();
    let gain = add(&mut h, Node::new("noodle.util.gain").at(0.0, 0.0));
    h.run();
    let start = field(&h, gain, "gain");
    drag(
        &mut h,
        PointerButton::Primary,
        Modifiers::NONE,
        &[
            start,
            start + Vec2::new(10.0, 0.0),
            start + Vec2::new(40.0, 0.0),
        ],
    );
    let value = param(&h, gain, "gain").expect("the drag set the gain");
    assert!(value > 0.0, "dragged right, so up from 0 dB: {value}");
    // The node stayed put, and nothing else was selected.
    assert_eq!(position(&h, gain), Position { x: 0.0, y: 0.0 });
    assert!(h.state().editor.selected.is_empty());
    assert!(matches!(h.state().log.last(), Some(Edit::EndDrag)));

    h.state_mut().session.undo();
    assert_eq!(param(&h, gain, "gain"), None);
}

#[test]
fn a_wired_parameter_has_no_field() {
    let mut h = rig();
    let sine = add(&mut h, Node::new("noodle.osc.sine").at(0.0, 0.0));
    let gain = add(&mut h, Node::new("noodle.util.gain").at(300.0, 0.0));
    h.run();
    assert_eq!(h.query_all_by_label("Gain").count(), 1);
    connect(&mut h, sine, "out", gain, "gain");
    h.run();
    assert_eq!(h.query_all_by_label("Gain").count(), 0);
}

#[test]
fn a_node_in_front_takes_the_clicks_on_the_fields_behind_it() {
    let mut h = rig();
    let back = add(&mut h, Node::new("noodle.util.gain").at(0.0, 0.0));
    h.run();
    let start = field(&h, back, "gain");
    let row = scene(&h)
        .node(back)
        .unwrap()
        .port(Side::Input, "gain")
        .unwrap()
        .row;
    // Selected, so drawn on top, with its header over the back node's field.
    let front = add(
        &mut h,
        Node::new("noodle.osc.sine").at(-20.0, row.top() - 8.0),
    );
    h.state_mut().editor.selected.insert(front);
    h.run();
    drag(
        &mut h,
        PointerButton::Primary,
        Modifiers::NONE,
        &[
            start,
            start + Vec2::new(10.0, 0.0),
            start + Vec2::new(40.0, 0.0),
        ],
    );
    assert_eq!(param(&h, back, "gain"), None);
    assert_eq!(position(&h, front).x, 20.0);
}

#[test]
fn a_field_drag_hidden_by_zooming_out_still_ends_its_undo_step() {
    let mut h = rig();
    let gain = add(&mut h, Node::new("noodle.util.gain").at(0.0, 0.0));
    h.run();
    let start = field(&h, gain, "gain");
    h.event(Event::PointerMoved(start));
    h.step();
    h.event(Event::PointerButton {
        pos: start,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    h.step();
    for dx in [10.0, 40.0] {
        h.event(Event::PointerMoved(start + Vec2::new(dx, 0.0)));
        h.step();
    }
    assert!(param(&h, gain, "gain").is_some());
    // Too small for fields, so the dragged one disappears.
    h.state_mut().editor.view.zoom = 0.3;
    h.event(Event::PointerMoved(start + Vec2::new(50.0, 0.0)));
    h.step();
    h.event(Event::PointerButton {
        pos: start + Vec2::new(50.0, 0.0),
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    h.run();
    assert!(matches!(h.state().log.last(), Some(Edit::EndDrag)));

    // So a node move afterwards is an undo step of its own.
    h.state_mut().editor.view.zoom = 1.0;
    h.run();
    let start = title(&h, gain);
    drag(
        &mut h,
        PointerButton::Primary,
        Modifiers::NONE,
        &[
            start,
            start + Vec2::new(20.0, 0.0),
            start + Vec2::new(40.0, 0.0),
        ],
    );
    h.state_mut().session.undo();
    assert_eq!(position(&h, gain), Position::default());
    assert!(
        param(&h, gain, "gain").is_some(),
        "only the move was undone"
    );
}

#[test]
fn middle_drag_on_a_field_pans() {
    let mut h = rig();
    let gain = add(&mut h, Node::new("noodle.util.gain").at(0.0, 0.0));
    h.run();
    let before = screen(&h, Pos2::ZERO);
    let start = field(&h, gain, "gain");
    drag(
        &mut h,
        PointerButton::Middle,
        Modifiers::NONE,
        &[
            start,
            start + Vec2::new(20.0, 0.0),
            start + Vec2::new(60.0, 25.0),
        ],
    );
    assert!((screen(&h, Pos2::ZERO) - (before + Vec2::new(60.0, 25.0))).length() < 0.01);
    assert_eq!(param(&h, gain, "gain"), None);
    assert!(h.state().log.is_empty(), "{:?}", h.state().log);
}

#[test]
fn ctrl_right_drag_from_a_field_cuts() {
    let mut h = rig();
    let (_, gain, _) = wired(&mut h);
    let start = field(&h, gain, "gain");
    let rect = scene(&h).node(gain).unwrap().rect;
    let past = screen(&h, Pos2::new(rect.left() - 100.0, rect.top() + 20.0));
    drag(
        &mut h,
        PointerButton::Secondary,
        Modifiers::COMMAND,
        &[start, start.lerp(past, 0.3), start.lerp(past, 0.7), past],
    );
    assert_eq!(source(&h, gain, "in"), None);
    assert_eq!(param(&h, gain, "gain"), None);
}

#[test]
fn a_socket_sticking_out_of_a_node_in_front_beats_a_field_behind() {
    let mut h = rig();
    let back = add(&mut h, Node::new("noodle.util.gain").at(0.0, 0.0));
    let sine = add(&mut h, Node::new("noodle.osc.sine").at(0.0, 200.0));
    let front = add(
        &mut h,
        Node::new("noodle.util.gain").at(NODE_WIDTH - 7.0, 22.0),
    );
    h.state_mut().editor.selected.insert(front);
    h.run();
    let socket = socket(&h, front, Side::Input, "in");
    let start = socket - Vec2::new(4.0, 0.0);
    let field = field(&h, back, "gain");
    assert!(
        (start.y - field.y).abs() < 9.0 && start.x < field.x + NODE_WIDTH / 2.0 - 10.0,
        "the start is on the back node's field"
    );
    let to = super::tests::socket(&h, sine, Side::Output, "out");
    drag(
        &mut h,
        PointerButton::Primary,
        Modifiers::NONE,
        &[start, start.lerp(to, 0.5), to],
    );
    assert_eq!(param(&h, back, "gain"), None);
    assert_eq!(source(&h, front, "in"), Some(Endpoint::new(sine, "out")));
}

#[test]
fn a_field_drag_carries_on_over_a_node_in_front() {
    let dragged = |with_front: bool| {
        let mut h = rig();
        let gain = add(
            &mut h,
            Node::new("noodle.util.gain")
                .at(0.0, 0.0)
                .with_param("gain", -60.0),
        );
        if with_front {
            // Over the right half of the field, where the drag ends.
            let front = add(&mut h, Node::new("noodle.osc.sine").at(80.0, 40.0));
            h.state_mut().editor.selected.insert(front);
        }
        h.run();
        let start = field(&h, gain, "gain") - Vec2::new(40.0, 0.0);
        let path: Vec<Pos2> = (0..=6)
            .map(|i| start + Vec2::new(i as f32 * 10.0, 0.0))
            .collect();
        drag(&mut h, PointerButton::Primary, Modifiers::NONE, &path);
        assert!(matches!(h.state().log.last(), Some(Edit::EndDrag)));
        param(&h, gain, "gain").unwrap()
    };
    let unobstructed = dragged(false);
    assert!(
        -60.0 < unobstructed && unobstructed < 24.0,
        "{unobstructed}"
    );
    assert_eq!(dragged(true), unobstructed);
}

/// Selects `ids`, then Ctrl+G; returns the new group.
fn group_selection(h: &mut H, ids: &[NodeId]) -> NodeId {
    let p = title(h, ids[0]);
    click(h, p, Modifiers::NONE);
    for &id in &ids[1..] {
        let p = title(h, id);
        click(h, p, Modifiers::SHIFT);
    }
    let p = empty_space(h);
    press(h, p, Modifiers::COMMAND, Key::G);
    let selected = &h.state().editor.selected;
    assert_eq!(selected.len(), 1, "the new group is selected");
    *selected.first().unwrap()
}

#[test]
fn ctrl_g_groups_the_selection_and_tab_goes_in_and_out() {
    let mut h = rig();
    let (sine, gain, _) = wired(&mut h);
    let group = group_selection(&mut h, &[gain]);
    h.run();

    let graph = h.state().session.project().graph();
    assert_eq!(graph.node(gain).unwrap().parent, Some(group));
    // At the top level: the sine and the group, with the sine wired to the
    // group's input.
    let top = scene(&h);
    assert!(top.node(sine).is_some() && top.node(group).is_some());
    assert!(top.node(gain).is_none());
    assert_eq!(top.wires.len(), 1);
    assert!(top.node(group).unwrap().port(Side::Input, "in1").is_some());

    // Tab goes in: the gain and its group input show, the sine doesn't.
    let p = empty_space(&h);
    press(&mut h, p, Modifiers::NONE, Key::Tab);
    assert_eq!(h.state().editor.group, Some(group));
    let inside = scene(&h);
    assert!(inside.node(gain).is_some());
    assert!(inside.node(sine).is_none());
    assert_eq!(inside.nodes.len(), 2, "the gain and the group input");
    assert_eq!(inside.wires.len(), 1);

    // Tab again goes out, with the group selected, ready to go back in.
    press(&mut h, p, Modifiers::NONE, Key::Tab);
    assert_eq!(h.state().editor.group, None);
    assert!(h.state().editor.selected.contains(&group));
    press(&mut h, p, Modifiers::NONE, Key::Tab);
    assert_eq!(h.state().editor.group, Some(group));
}

#[test]
fn nodes_added_inside_a_group_belong_to_it() {
    let mut h = rig();
    let (_, gain, _) = wired(&mut h);
    let group = group_selection(&mut h, &[gain]);
    let p = empty_space(&h);
    press(&mut h, p, Modifiers::NONE, Key::Tab);

    press(&mut h, p, Modifiers::SHIFT, Key::A);
    h.event(Event::Text("group output".into()));
    h.run();
    h.key_press(Key::Enter);
    h.run();

    let graph = h.state().session.project().graph();
    let ports = graph.group_ports(group);
    assert_eq!(ports.outputs.len(), 1);
    assert_eq!(ports.outputs[0].name, "out1");
    let node = graph.node(ports.outputs[0].node).unwrap();
    assert_eq!(node.parent, Some(group));
}

#[test]
fn double_clicking_a_group_enters_it_and_the_breadcrumb_leads_back() {
    let mut h = rig();
    let (_, gain, _) = wired(&mut h);
    let group = group_selection(&mut h, &[gain]);
    h.run();
    let p = title(&h, group);
    click(&mut h, p, Modifiers::NONE);
    click(&mut h, p, Modifiers::NONE);
    h.run();
    assert_eq!(h.state().editor.group, Some(group));

    h.get_by_label("Project").click();
    h.run();
    assert_eq!(h.state().editor.group, None);
    assert!(h.state().editor.selected.contains(&group));
}

#[test]
fn undoing_the_grouping_while_inside_goes_back_to_the_top() {
    let mut h = rig();
    let (_, gain, _) = wired(&mut h);
    let group = group_selection(&mut h, &[gain]);
    let p = empty_space(&h);
    press(&mut h, p, Modifiers::NONE, Key::Tab);
    assert_eq!(h.state().editor.group, Some(group));
    h.state_mut().session.undo();
    h.run();
    assert_eq!(h.state().editor.group, None);
    assert!(scene(&h).node(gain).is_some());
}

#[test]
fn deleting_a_group_removes_what_is_inside_and_undo_brings_it_back() {
    let mut h = rig();
    let (_, gain, _) = wired(&mut h);
    let group = group_selection(&mut h, &[gain]);
    let p = empty_space(&h);
    press(&mut h, p, Modifiers::NONE, Key::X);
    let graph = h.state().session.project().graph();
    assert!(graph.node(group).is_none() && graph.node(gain).is_none());
    h.state_mut().session.undo();
    h.run();
    let graph = h.state().session.project().graph();
    assert_eq!(graph.node(gain).unwrap().parent, Some(group));
}

/// Tab still enters and leaves a group with another widget in the window.
/// This does not prove the `tab: true` focus filter: kittest delivers Tab to
/// the editor either way, so that line needs a manual Tab press in the real
/// window (see ARCHITECTURE.md).
#[test]
fn tab_enters_and_leaves_a_group_with_another_widget_present() {
    let mut h = rig_with(true);
    let (_, gain, _) = wired(&mut h);
    let group = group_selection(&mut h, &[gain]);
    let p = empty_space(&h);
    press(&mut h, p, Modifiers::NONE, Key::Tab);
    assert_eq!(h.state().editor.group, Some(group), "Tab entered the group");
    press(&mut h, p, Modifiers::NONE, Key::Tab);
    assert_eq!(h.state().editor.group, None, "and left it");
}

#[test]
fn shift_d_skips_groups_and_their_ports() {
    let mut h = rig();
    let (sine, gain, _) = wired(&mut h);
    let group = group_selection(&mut h, &[gain]);
    let before = h.state().session.project().graph().nodes().count();
    // Select the group and the sine, then duplicate.
    let p = title(&h, sine);
    click(&mut h, p, Modifiers::NONE);
    let p = title(&h, group);
    click(&mut h, p, Modifiers::SHIFT);
    let p = empty_space(&h);
    press(&mut h, p, Modifiers::SHIFT, Key::D);
    let after = h.state().session.project().graph().nodes().count();
    assert_eq!(after, before + 1, "only the sine was copied");

    // Inside, a port isn't copied either.
    let p2 = title(&h, group);
    click(&mut h, p2, Modifiers::NONE);
    press(&mut h, p, Modifiers::NONE, Key::Tab);
    assert_eq!(h.state().editor.group, Some(group));
    press(&mut h, p, Modifiers::NONE, Key::A);
    press(&mut h, p, Modifiers::SHIFT, Key::D);
    let inside = h.state().session.project().graph().nodes().count();
    assert_eq!(inside, after + 1, "only the gain was copied");
}

#[test]
fn new_ports_take_the_first_free_name() {
    let mut project = noodle_core::Project::new();
    let mut history = noodle_core::History::new();
    let group = project.new_node_id();
    history
        .apply(
            &mut project,
            Command::AddNode {
                id: group,
                node: Node::new(noodle_core::group::GROUP),
            },
        )
        .unwrap();
    for name in ["in1", "in3"] {
        let id = project.new_node_id();
        let node = Node::new(noodle_core::group::GROUP_INPUT)
            .in_group(group)
            .with_config(noodle_core::Config::new().with(
                noodle_core::group::PORT_NAME,
                noodle_core::Value::Text(name.into()),
            ));
        history
            .apply(&mut project, Command::AddNode { id, node })
            .unwrap();
    }
    use noodle_core::group::{GROUP_INPUT, GROUP_OUTPUT};
    assert_eq!(
        super::free_port_name(&project, Some(group), GROUP_INPUT),
        "in2"
    );
    assert_eq!(
        super::free_port_name(&project, Some(group), GROUP_OUTPUT),
        "out1"
    );
}

#[test]
fn ctrl_j_does_nothing_inside_a_group() {
    let mut h = rig();
    let (_, gain, _) = wired(&mut h);
    group_selection(&mut h, &[gain]);
    let p = empty_space(&h);
    press(&mut h, p, Modifiers::NONE, Key::Tab);
    press(&mut h, p, Modifiers::NONE, Key::A);
    press(&mut h, p, Modifiers::COMMAND, Key::J);
    assert_eq!(h.state().session.project().frames().count(), 0);
}

#[test]
fn undoing_a_nested_group_lands_in_the_group_around_it() {
    let mut h = rig();
    let (_, gain, _) = wired(&mut h);
    let outer = group_selection(&mut h, &[gain]);
    let p = empty_space(&h);
    press(&mut h, p, Modifiers::NONE, Key::Tab);
    assert_eq!(h.state().editor.group, Some(outer));
    let inner = group_selection(&mut h, &[gain]);
    press(&mut h, p, Modifiers::NONE, Key::Tab);
    assert_eq!(h.state().editor.group, Some(inner));
    h.state_mut().session.undo();
    h.run();
    assert_eq!(h.state().editor.group, Some(outer));
}

/// A mixer with `inputs` oscillators wired to its inputs.
fn mix_of(h: &mut H, inputs: i64) -> NodeId {
    let config = noodle_core::Config::new().with("inputs", noodle_core::Value::Int(inputs));
    let mix = add(
        h,
        Node::new("noodle.util.mix")
            .with_config(config)
            .at(0.0, 0.0),
    );
    for i in 1..=inputs {
        let src = add(h, Node::new("noodle.osc.sine").at(-600.0, 150.0 * i as f32));
        connect(h, src, "out", mix, &format!("in{i}"));
    }
    h.run();
    mix
}

/// The labels of a node's input rows, top to bottom, as screen points.
fn input_labels(h: &H, node: NodeId) -> Vec<(String, Pos2)> {
    let scene = scene(h);
    let geom = scene.node(node).unwrap();
    let mut rows: Vec<_> = geom
        .ports
        .iter()
        .filter(|p| p.side == Side::Input && !p.in_header && !p.spare)
        .map(|p| (p.key.clone(), p.row.center() + Vec2::new(-20.0, 0.0)))
        .collect();
    rows.sort_by(|a, b| a.1.y.total_cmp(&b.1.y));
    rows.into_iter().map(|(k, p)| (k, screen(h, p))).collect()
}

#[test]
fn dragging_a_port_label_down_its_column_reorders_it_as_one_undo_step() {
    let mut h = rig();
    let mix = mix_of(&mut h, 3);
    let before = h.state().session.project().clone();
    let rows = input_labels(&h, mix);
    let keys: Vec<_> = rows.iter().map(|r| r.0.clone()).collect();
    let (first, last) = (rows[0].1, rows[2].1);
    drag(
        &mut h,
        PointerButton::Primary,
        Modifiers::NONE,
        &[
            first,
            first + Vec2::new(0.0, 10.0),
            last + Vec2::new(0.0, 8.0),
        ],
    );
    let after: Vec<_> = input_labels(&h, mix).into_iter().map(|r| r.0).collect();
    assert_eq!(after, [keys[1].clone(), keys[2].clone(), keys[0].clone()]);
    // Display only: the node hasn't moved and nothing was recompiled away.
    assert_eq!(position(&h, mix), Position { x: 0.0, y: 0.0 });
    h.state_mut().session.undo();
    assert_eq!(h.state().session.project(), &before);
    let again: Vec<_> = input_labels(&h, mix).into_iter().map(|r| r.0).collect();
    assert_eq!(again, keys);
}

#[test]
fn dropping_a_port_where_it_was_changes_nothing() {
    let mut h = rig();
    let mix = mix_of(&mut h, 3);
    let rows = input_labels(&h, mix);
    let (a, b) = (rows[1].1, rows[1].1 + Vec2::new(0.0, 3.0));
    drag(
        &mut h,
        PointerButton::Primary,
        Modifiers::NONE,
        &[a, a + Vec2::new(0.0, 2.0), b],
    );
    assert!(
        h.state()
            .log
            .iter()
            .all(|e| !matches!(e, Edit::Apply(Command::SetPortOrder { .. }))),
        "{:?}",
        h.state().log
    );
}

#[test]
fn dragging_a_port_label_does_not_move_the_node() {
    let mut h = rig();
    let mix = mix_of(&mut h, 3);
    let rows = input_labels(&h, mix);
    let start = rows[0].1;
    drag(
        &mut h,
        PointerButton::Primary,
        Modifiers::NONE,
        &[
            start,
            start + Vec2::new(30.0, 10.0),
            start + Vec2::new(90.0, 5.0),
        ],
    );
    assert_eq!(position(&h, mix), Position { x: 0.0, y: 0.0 });
}

/// Drags `node` by its title so its middle ends up at screen point `to`.
fn drag_node_to(h: &mut H, node: NodeId, modifiers: Modifiers, to: Pos2) {
    let grab = title(h, node);
    let rect = scene(h).node(node).unwrap().rect;
    let middle = screen(h, rect.center());
    let end = grab + (to - middle);
    drag(
        h,
        PointerButton::Primary,
        modifiers,
        &[
            grab,
            grab + Vec2::new(5.0, 5.0),
            (grab + end.to_vec2()) / 2.0,
            end,
        ],
    );
}

#[test]
fn dropping_a_node_on_a_wire_splices_it_in_as_one_undo_step() {
    let mut h = rig();
    let (sine, gain, stroke) = wired(&mut h);
    let filter = add(&mut h, Node::new("noodle.util.gain").at(0.0, 300.0));
    h.run();
    let before = h.state().session.project().clone();
    drag_node_to(&mut h, filter, Modifiers::NONE, stroke[1]);
    assert_eq!(source(&h, filter, "in"), Some(Endpoint::new(sine, "out")));
    assert_eq!(source(&h, gain, "in"), Some(Endpoint::new(filter, "out")));
    assert!(h.state().session.diagnostics().is_empty());
    h.state_mut().session.undo();
    assert_eq!(h.state().session.project(), &before);
}

#[test]
fn alt_drops_a_node_on_a_wire_without_splicing() {
    let mut h = rig();
    let (sine, gain, stroke) = wired(&mut h);
    let filter = add(&mut h, Node::new("noodle.util.gain").at(0.0, 300.0));
    h.run();
    drag_node_to(&mut h, filter, Modifiers::ALT, stroke[1]);
    assert_eq!(source(&h, gain, "in"), Some(Endpoint::new(sine, "out")));
    assert_eq!(source(&h, filter, "in"), None);
}

#[test]
fn a_node_with_wires_isnt_spliced() {
    let mut h = rig();
    let (sine, gain, stroke) = wired(&mut h);
    let other = add(&mut h, Node::new("noodle.osc.saw").at(0.0, 300.0));
    let filter = add(&mut h, Node::new("noodle.util.gain").at(200.0, 300.0));
    connect(&mut h, other, "out", filter, "in");
    h.run();
    drag_node_to(&mut h, filter, Modifiers::NONE, stroke[1]);
    assert_eq!(source(&h, gain, "in"), Some(Endpoint::new(sine, "out")));
}

#[test]
fn a_node_with_no_matching_ports_isnt_spliced() {
    let mut h = rig();
    let (sine, gain, stroke) = wired(&mut h);
    // The output node has inputs but nothing to carry the signal on.
    let out = add(&mut h, Node::new(OUTPUT_ID).at(0.0, 300.0));
    h.run();
    drag_node_to(&mut h, out, Modifiers::NONE, stroke[1]);
    assert_eq!(source(&h, gain, "in"), Some(Endpoint::new(sine, "out")));
    assert_eq!(source(&h, out, "in"), None);
}

/// Sends a copy, cut or paste event with the pointer at `at`, as the window
/// does for Ctrl/Cmd+C, X and V.
fn clipboard_event(h: &mut H, at: Pos2, event: Event) {
    h.hover_at(at);
    h.step();
    h.event(event);
    h.run();
}

#[test]
fn copy_and_paste_put_the_nodes_and_the_wires_between_them_at_the_pointer() {
    let mut h = rig();
    let (sine, gain, _) = wired(&mut h);
    let before = h.state().session.project().graph().connections().count();
    h.state_mut().editor.select_only([sine, gain]);
    let at = empty_space(&h);
    clipboard_event(&mut h, at, Event::Copy);
    // Copying changes nothing.
    assert_eq!(
        h.state().session.project().graph().connections().count(),
        before
    );
    let before = h.state().session.project().clone();
    let target = screen(&h, Pos2::new(100.0, 400.0));
    clipboard_event(&mut h, target, Event::Paste(String::new()));

    let graph = h.state().session.project().graph();
    assert_eq!(graph.connections().count(), 2);
    let pasted: Vec<NodeId> = h.state().editor.selected.iter().copied().collect();
    assert_eq!(pasted.len(), 2);
    assert!(!pasted.contains(&sine) && !pasted.contains(&gain));
    // The copy of the gain is fed by the copy of the sine, not the original.
    let copy_gain = *pasted
        .iter()
        .find(|id| graph.node(**id).unwrap().type_id == "noodle.util.gain")
        .unwrap();
    let feeder = source(&h, copy_gain, "in").unwrap().node;
    assert!(pasted.contains(&feeder));
    // The top-left of the copies is at the pointer.
    let top_left = pasted
        .iter()
        .map(|id| position(&h, *id))
        .fold((f32::MAX, f32::MAX), |(x, y), p| (x.min(p.x), y.min(p.y)));
    assert!((top_left.0 - 100.0).abs() < 1.0 && (top_left.1 - 400.0).abs() < 1.0);
    h.state_mut().session.undo();
    assert_eq!(h.state().session.project(), &before);
}

#[test]
fn cut_removes_the_selection_and_paste_brings_it_back() {
    let mut h = rig();
    let (sine, gain, _) = wired(&mut h);
    h.state_mut().editor.select_only([sine]);
    let at = empty_space(&h);
    clipboard_event(&mut h, at, Event::Cut);
    assert!(h.state().session.project().graph().node(sine).is_none());
    assert!(h.state().session.project().graph().node(gain).is_some());
    clipboard_event(&mut h, at, Event::Paste(String::new()));
    let graph = h.state().session.project().graph();
    let sines = graph
        .nodes()
        .filter(|(_, n)| n.type_id == "noodle.osc.sine")
        .count();
    assert_eq!(sines, 1);
}

#[test]
fn paste_with_an_empty_clipboard_does_nothing() {
    let mut h = rig();
    wired(&mut h);
    let at = empty_space(&h);
    clipboard_event(&mut h, at, Event::Paste(String::new()));
    assert!(h.state().log.is_empty());
}

fn link(h: &mut H, from: Pos2, to: Pos2) {
    drag(
        h,
        PointerButton::Primary,
        Modifiers::NONE,
        &[
            from,
            from + Vec2::new(12.0, 6.0),
            (from + to.to_vec2()) / 2.0,
            to,
        ],
    );
}

#[test]
fn wiring_to_a_mixers_spare_input_makes_it_real_in_one_undo_step() {
    let mut h = rig();
    let mix = mix_of(&mut h, 2);
    let src = add(&mut h, Node::new("noodle.osc.saw").at(-100.0, 400.0));
    h.run();
    let before = h.state().session.project().clone();
    let spare = socket(&h, mix, Side::Input, "in3");
    let from = socket(&h, src, Side::Output, "out");
    link(&mut h, from, spare);
    assert_eq!(source(&h, mix, "in3"), Some(Endpoint::new(src, "out")));
    let node = h
        .state()
        .session
        .project()
        .graph()
        .node(mix)
        .unwrap()
        .clone();
    assert_eq!(noodle_core::spare::mixer_inputs(&node), 3);
    // And now there's a new spare after it.
    assert!(
        scene(&h)
            .node(mix)
            .unwrap()
            .port(Side::Input, "in4")
            .unwrap()
            .spare
    );
    assert!(h.state().session.diagnostics().is_empty());
    h.state_mut().session.undo();
    assert_eq!(h.state().session.project(), &before);
}

#[test]
fn wiring_into_a_groups_spare_input_adds_a_port() {
    use noodle_core::group::{GROUP, GROUP_INPUT};
    let mut h = rig();
    let group = add(&mut h, Node::new(GROUP).at(400.0, 0.0));
    let src = add(&mut h, Node::new("noodle.osc.sine").at(0.0, 300.0));
    h.run();
    let before = h.state().session.project().clone();
    let spare = socket(&h, group, Side::Input, "in1");
    let from = socket(&h, src, Side::Output, "out");
    link(&mut h, from, spare);
    let graph = h.state().session.project().graph();
    let ports = graph.group_ports(group);
    assert_eq!(ports.inputs.len(), 1);
    assert_eq!(
        graph.node(ports.inputs[0].node).unwrap().type_id,
        GROUP_INPUT
    );
    assert_eq!(source(&h, group, "in1"), Some(Endpoint::new(src, "out")));
    assert!(
        scene(&h)
            .node(group)
            .unwrap()
            .port(Side::Input, "in2")
            .unwrap()
            .spare
    );
    h.state_mut().session.undo();
    assert_eq!(h.state().session.project(), &before);
}

#[test]
fn dragging_from_a_groups_spare_output_adds_a_port_too() {
    use noodle_core::group::GROUP;
    let mut h = rig();
    let group = add(&mut h, Node::new(GROUP).at(0.0, 0.0));
    let gain = add(&mut h, Node::new("noodle.util.gain").at(500.0, 0.0));
    h.run();
    let from = socket(&h, group, Side::Output, "out1");
    let to = socket(&h, gain, Side::Input, "in");
    link(&mut h, from, to);
    assert_eq!(source(&h, gain, "in"), Some(Endpoint::new(group, "out1")));
    let graph = h.state().session.project().graph();
    assert_eq!(graph.group_ports(group).outputs.len(), 1);
}

#[test]
fn a_spare_that_gets_no_wire_stores_nothing() {
    let mut h = rig();
    let mix = mix_of(&mut h, 2);
    let before = h.state().session.project().clone();
    let spare = socket(&h, mix, Side::Input, "in3");
    // Dropped on empty space.
    let empty = empty_space(&h);
    link(&mut h, spare, empty);
    assert_eq!(h.state().session.project(), &before);
}

/// Types `text` into the rename box and presses Enter.
fn type_name(h: &mut H, text: &str) {
    h.run();
    h.event(Event::Key {
        key: Key::A,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::COMMAND,
    });
    if text.is_empty() {
        h.key_press(Key::Backspace);
    } else {
        h.event(Event::Text(text.to_owned()));
    }
    h.run();
    h.key_press(Key::Enter);
    h.run();
}

fn group_name(h: &H, group: NodeId) -> Option<String> {
    match h
        .state()
        .session
        .project()
        .graph()
        .node(group)?
        .config
        .get("name")
    {
        Some(noodle_core::Value::Text(t)) => Some(t.clone()),
        _ => None,
    }
}

#[test]
fn f2_renames_the_selected_group_and_an_empty_name_puts_the_default_back() {
    use noodle_core::group::GROUP;
    let mut h = rig();
    let group = add(&mut h, Node::new(GROUP));
    let p = title(&h, group);
    click(&mut h, p, Modifiers::NONE);
    let at = empty_space(&h);
    press(&mut h, at, Modifiers::NONE, Key::F2);
    assert!(h.state().editor.rename.is_some());
    type_name(&mut h, "Drums");
    assert_eq!(group_name(&h, group).as_deref(), Some("Drums"));
    assert_eq!(scene(&h).node(group).unwrap().title, "Drums");
    h.state_mut().session.undo();
    assert_eq!(group_name(&h, group), None);
    // Empty puts the default back.
    h.state_mut().session.redo();
    press(&mut h, at, Modifiers::NONE, Key::F2);
    type_name(&mut h, "");
    assert_eq!(group_name(&h, group), None);
}

#[test]
fn double_clicking_a_group_ports_label_renames_it_and_keeps_its_wire() {
    use noodle_core::group::GROUP;
    let mut h = rig();
    let group = add(&mut h, Node::new(GROUP).at(400.0, 0.0));
    let src = add(&mut h, Node::new("noodle.osc.sine").at(-100.0, 300.0));
    h.run();
    let from = socket(&h, src, Side::Output, "out");
    let spare = socket(&h, group, Side::Input, "in1");
    link(&mut h, from, spare);
    h.run();
    let row = scene(&h)
        .node(group)
        .unwrap()
        .port(Side::Input, "in1")
        .unwrap()
        .row
        .center();
    let at = screen(&h, row + Vec2::new(-20.0, 0.0));
    double_click(&mut h, at);
    assert!(h.state().editor.rename.is_some(), "the port's box opened");
    assert_eq!(h.state().editor.group, None, "and the group wasn't entered");
    type_name(&mut h, "feed");
    assert_eq!(source(&h, group, "feed"), Some(Endpoint::new(src, "out")));
    assert_eq!(source(&h, group, "in1"), None);
    assert!(h.state().session.diagnostics().is_empty());
}

#[test]
fn a_port_name_already_taken_is_refused() {
    use noodle_core::group::GROUP;
    let mut h = rig();
    let group = add(&mut h, Node::new(GROUP));
    for name in ["x", "y"] {
        let session = &mut h.state_mut().session;
        let id = session.new_node_id();
        let command = noodle_core::spare::add_group_port(
            session.project().graph(),
            group,
            noodle_core::spare::Side::Input,
            name,
            id,
        );
        session.edit([Edit::Apply(command)]);
    }
    h.run();
    let row = scene(&h)
        .node(group)
        .unwrap()
        .port(Side::Input, "x")
        .unwrap()
        .row
        .center();
    let at = screen(&h, row + Vec2::new(-20.0, 0.0));
    double_click(&mut h, at);
    type_name(&mut h, "y");
    let ports = h.state().session.project().graph().group_ports(group);
    assert!(ports.input("x").is_some() && ports.input("y").is_some());
}

#[test]
fn the_add_node_list_offers_group_input_and_output_inside_a_group() {
    use noodle_core::group::{GROUP, GROUP_INPUT, GROUP_OUTPUT};
    let mut h = rig();
    let group = add(&mut h, Node::new(GROUP));
    let p = title(&h, group);
    click(&mut h, p, Modifiers::NONE);
    let at = empty_space(&h);
    press(&mut h, at, Modifiers::NONE, Key::Tab);
    assert_eq!(h.state().editor.group, Some(group));
    for (query, kind) in [("group input", GROUP_INPUT), ("group output", GROUP_OUTPUT)] {
        press(&mut h, at, Modifiers::SHIFT, Key::A);
        h.event(Event::Text(query.into()));
        h.run();
        h.key_press(Key::Enter);
        h.run();
        let graph = h.state().session.project().graph();
        assert!(
            graph.children(Some(group)).any(|(_, n)| n.type_id == kind),
            "{query} was added inside the group"
        );
    }
}
