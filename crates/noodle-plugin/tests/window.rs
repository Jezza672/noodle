//! The editor window, through the CLAP `gui` extension from a host of our
//! own: an X11 window plays the host's, the plugin is asked to embed in it,
//! and the test looks at what the plugin made there.
//!
//! Needs an X display and a Vulkan driver (`xvfb-run`, and Mesa's lavapipe in
//! `mesa-vulkan-drivers`). Without them the tests say so and pass, unless
//! `NOODLE_REQUIRE_WINDOW` is set, as it is in CI, so a change to the runner
//! can't quietly skip them.

#![cfg(target_os = "linux")]

mod common;

use std::time::{Duration, Instant};

use clack_extensions::gui::{
    GuiApiType, GuiConfiguration, GuiSize, PluginGui, Window as ClapWindow,
};
use clack_extensions::state::PluginState;
use clack_host::prelude::*;
use common::plugin_path;
use noodle_plugin::{EFFECT_ID, INSTRUMENT_ID};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    ConnectionExt, CreateWindowAux, ImageFormat, MapState, Window as XWindow, WindowClass,
};
use x11rb::rust_connection::RustConnection;

struct Host;
struct Shared;
struct Main;

impl SharedHandler<'_> for Shared {
    fn request_restart(&self) {}
    fn request_process(&self) {}
    fn request_callback(&self) {}
}

impl<'a> MainThreadHandler<'a> for Main {}

impl HostHandlers for Host {
    type Shared<'a> = Shared;
    type MainThread<'a> = Main;
    type AudioProcessor<'a> = ();
}

/// The host's side of the X connection: a top-level window the editor goes
/// in.
struct Display {
    connection: RustConnection,
    root: XWindow,
}

impl Display {
    fn open() -> Option<Self> {
        let (connection, screen) = RustConnection::connect(None).ok()?;
        let root = connection.setup().roots[screen].root;
        Some(Self { connection, root })
    }

    fn window(&self, width: u16, height: u16) -> XWindow {
        let id = self.connection.generate_id().unwrap();
        self.connection
            .create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                id,
                self.root,
                0,
                0,
                width,
                height,
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new(),
            )
            .unwrap();
        self.connection.map_window(id).unwrap();
        self.connection.flush().unwrap();
        id
    }

    fn children(&self, window: XWindow) -> Vec<XWindow> {
        self.connection
            .query_tree(window)
            .unwrap()
            .reply()
            .unwrap()
            .children
    }

    fn size(&self, window: XWindow) -> (u16, u16) {
        let geometry = self
            .connection
            .get_geometry(window)
            .unwrap()
            .reply()
            .unwrap();
        (geometry.width, geometry.height)
    }

    fn viewable(&self, window: XWindow) -> bool {
        self.connection
            .get_window_attributes(window)
            .unwrap()
            .reply()
            .unwrap()
            .map_state
            == MapState::VIEWABLE
    }

    /// Writes what the window shows as a PPM, for a person to look at.
    fn shot(&self, window: XWindow, path: &std::path::Path) {
        let (width, height) = self.size(window);
        let image = self
            .connection
            .get_image(ImageFormat::Z_PIXMAP, window, 0, 0, width, height, !0)
            .unwrap()
            .reply()
            .unwrap();
        let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
        for pixel in image.data.chunks(4) {
            ppm.extend([pixel[2], pixel[1], pixel[0]]);
        }
        std::fs::write(path, ppm).unwrap();
    }

    /// How many different pixel values the window shows: one for a window
    /// nothing was drawn in.
    fn colours(&self, window: XWindow) -> usize {
        let (width, height) = self.size(window);
        let image = self
            .connection
            .get_image(ImageFormat::Z_PIXMAP, window, 0, 0, width, height, !0)
            .unwrap()
            .reply()
            .unwrap();
        let mut seen = std::collections::HashSet::new();
        for pixel in image.data.chunks(4) {
            seen.insert(pixel.to_vec());
        }
        seen.len()
    }
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// An X display, or `None` if there isn't one (and the tests aren't required
/// to have it).
fn display() -> Option<Display> {
    let display = Display::open();
    if display.is_none() {
        assert!(
            std::env::var_os("NOODLE_REQUIRE_WINDOW").is_none(),
            "NOODLE_REQUIRE_WINDOW is set but there is no X display"
        );
        eprintln!("no X display: skipped");
    }
    display
}

fn entry() -> PluginEntry {
    // SAFETY: the plugin is ours, and built by `plugin_path`.
    let entry = unsafe { PluginEntry::load(plugin_path()) }.expect("load the plugin");
    // Stay loaded, as the app's host does on Linux: a window thread may still
    // be winding down when the test process exits.
    std::mem::forget(entry.clone());
    entry
}

fn instance(entry: &PluginEntry, id: &str) -> PluginInstance<Host> {
    let info = HostInfo::new("Test host", "Noodle", "https://example.com", "1").unwrap();
    let id = std::ffi::CString::new(id).unwrap();
    PluginInstance::<Host>::new(|_| Shared, |_| Main, entry, &id, &info).expect("create instance")
}

fn embedded() -> GuiConfiguration<'static> {
    GuiConfiguration {
        api_type: GuiApiType::X11,
        is_floating: false,
    }
}

/// Opens `instance`'s editor in a new host window and shows it. Returns the
/// host window.
fn open(display: &Display, instance: &mut PluginInstance<Host>) -> XWindow {
    let handle = instance.plugin_handle();
    let gui = handle.get_extension::<PluginGui>().expect("a gui");
    gui.create(&handle, embedded()).expect("create");
    let size = gui.get_size(&handle).expect("a size");
    let parent = display.window(size.width as u16, size.height as u16);
    // SAFETY: the host window outlives the editor in these tests.
    unsafe { gui.set_parent(&handle, ClapWindow::from_x11_handle(parent.into())) }
        .expect("set_parent");
    gui.show(&handle).expect("show");
    parent
}

fn close(instance: &mut PluginInstance<Host>) {
    let handle = instance.plugin_handle();
    let gui = handle.get_extension::<PluginGui>().expect("a gui");
    gui.destroy(&handle);
}

#[test]
fn the_plugin_offers_an_embedded_editor_on_x11_and_nothing_else() {
    let entry = entry();
    let mut instance = instance(&entry, EFFECT_ID);
    let handle = instance.plugin_handle();
    let gui = handle.get_extension::<PluginGui>().expect("a gui");
    assert!(gui.is_api_supported(&handle, embedded()));
    assert!(!gui.is_api_supported(
        &handle,
        GuiConfiguration {
            api_type: GuiApiType::X11,
            is_floating: true,
        }
    ));
    assert!(!gui.is_api_supported(
        &handle,
        GuiConfiguration {
            api_type: GuiApiType::WIN32,
            is_floating: false,
        }
    ));
    assert!(gui.can_resize(&handle));
}

#[test]
fn the_editor_draws_in_the_hosts_window_resizes_hides_and_goes() {
    let Some(display) = display() else { return };
    let entry = entry();
    let mut instance = instance(&entry, EFFECT_ID);
    let parent = open(&display, &mut instance);

    // A child window appears in the host's, and something is drawn in it.
    until("a child window", || !display.children(parent).is_empty());
    let child = display.children(parent)[0];
    until("the child is shown", || display.viewable(child));
    until("something is drawn", || display.colours(child) > 8);
    // `NOODLE_WINDOW_SHOT=file.ppm` keeps what it drew.
    if let Some(path) = std::env::var_os("NOODLE_WINDOW_SHOT") {
        std::thread::sleep(Duration::from_secs(1));
        display.shot(child, path.as_ref());
    }

    // The host sets a size and the editor follows it.
    {
        let handle = instance.plugin_handle();
        let gui = handle.get_extension::<PluginGui>().unwrap();
        gui.set_size(
            &handle,
            GuiSize {
                width: 900,
                height: 600,
            },
        )
        .expect("set_size");
        let size = gui.get_size(&handle).unwrap();
        assert_eq!((size.width, size.height), (900, 600));
        // Under the smallest the editor allows, it asks for more.
        let adjusted = gui
            .adjust_size(
                &handle,
                GuiSize {
                    width: 10,
                    height: 10,
                },
            )
            .expect("adjust_size");
        assert!(adjusted.width >= 640 && adjusted.height >= 400);
    }
    until("the child's size follows", || {
        display.size(child) == (900, 600)
    });

    // Hidden, the window is not on screen; shown again it is.
    {
        let handle = instance.plugin_handle();
        let gui = handle.get_extension::<PluginGui>().unwrap();
        gui.hide(&handle).expect("hide");
    }
    until("hidden", || !display.viewable(child));
    {
        let handle = instance.plugin_handle();
        let gui = handle.get_extension::<PluginGui>().unwrap();
        gui.show(&handle).expect("show");
    }
    until("shown again", || display.viewable(child));

    // Destroying it takes the child window with it.
    close(&mut instance);
    until("the child is gone", || display.children(parent).is_empty());
}

#[test]
fn two_instances_each_have_a_window_and_their_own_project() {
    let Some(display) = display() else { return };
    let entry = entry();
    let mut effect = instance(&entry, EFFECT_ID);
    let mut synth = instance(&entry, INSTRUMENT_ID);
    let effect_parent = open(&display, &mut effect);
    let synth_parent = open(&display, &mut synth);
    until("both windows", || {
        !display.children(effect_parent).is_empty() && !display.children(synth_parent).is_empty()
    });
    let (effect_child, synth_child) = (
        display.children(effect_parent)[0],
        display.children(synth_parent)[0],
    );
    until("both draw", || {
        display.colours(effect_child) > 8 && display.colours(synth_child) > 8
    });

    // Each has its own project: the saved states differ.
    let save = |instance: &mut PluginInstance<Host>| -> Vec<u8> {
        let handle = instance.plugin_handle();
        let state = handle.get_extension::<PluginState>().unwrap();
        let mut bytes = Vec::new();
        state.save(&handle, &mut bytes).expect("save");
        bytes
    };
    assert_ne!(save(&mut effect), save(&mut synth));

    // Closing one leaves the other drawing.
    close(&mut effect);
    until("the first is gone", || {
        display.children(effect_parent).is_empty()
    });
    assert!(display.viewable(display.children(synth_parent)[0]));
    close(&mut synth);
    until("the second is gone", || {
        display.children(synth_parent).is_empty()
    });
}

#[test]
fn a_second_create_after_destroy_opens_a_fresh_editor() {
    let Some(display) = display() else { return };
    let entry = entry();
    let mut instance = instance(&entry, INSTRUMENT_ID);
    for _ in 0..2 {
        let parent = open(&display, &mut instance);
        until("a child window", || !display.children(parent).is_empty());
        close(&mut instance);
        until("the child is gone", || display.children(parent).is_empty());
    }
}
