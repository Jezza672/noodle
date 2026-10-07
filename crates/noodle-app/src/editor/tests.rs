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
    let rig = Rig {
        session: Session::new(crate::registry()),
        editor: EditorState::default(),
        log: Vec::new(),
    };
    let mut h = Harness::builder()
        .with_size(Vec2::new(1000.0, 700.0))
        // Like a real display, so two clicks can make a double click.
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, rig: &mut Rig| {
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
    Scene::build(session.project(), session.registry())
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
    let node = add(&mut h, Node::new("no.such.type").at(0.0, 0.0));
    h.run();
    h.hover_at(title(&h, node));
    for _ in 0..60 {
        h.step();
    }
    h.run();
    assert!(h.query_by_label_contains("unknown node type").is_some());
}
