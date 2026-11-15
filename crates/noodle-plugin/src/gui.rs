//! The plugin's window: the app's editor-only layout drawn by `egui` in a
//! child window of the host's, through `baseview` (`egui-baseview`).
//!
//! The host drives it with the CLAP `gui` extension: `create`, then
//! `set_parent` with its window, `show`, and `destroy` when it is done. The
//! editor lives from `create` to `destroy`; hiding it only hides the window,
//! so the app keeps its place (the selected node, the zoom, an open panel)
//! when the host closes and reopens the editor, which several hosts do.
//!
//! Embedding only: a floating window is refused, and the host asked for the
//! platform's own API (X11, Win32 or Cocoa). Wayland hosts embed through
//! XWayland, as with any plugin.
//!
//! Threads. On Linux `baseview` runs the window on a thread of its own; on
//! Windows and macOS it runs on the host's main thread. Either way the frame
//! runs with the instance's app locked (see the crate's docs), and the window
//! is made and dropped on the main thread.
//!
//! Sizes. CLAP counts physical pixels on X11 and Windows and logical points
//! on macOS, and so does this. The scale is the platform's, unless the host
//! gave one with `set_scale` where the platform doesn't say.

use std::sync::{Arc, Mutex, PoisonError};

use clack_extensions::gui::{
    AspectRatioStrategy, GuiApiType, GuiConfiguration, GuiResizeHints, GuiSize, PluginGuiImpl,
    Window,
};
use clack_plugin::prelude::PluginError;
use egui_baseview::baseview::dpi::{LogicalSize, PhysicalSize, Size};
use egui_baseview::baseview::{HandlerError, Window as BaseviewWindow};
use egui_baseview::{EguiWindow, EguiWindowSettings, Frame};
use noodle_app::app::App;
use noodle_engine::wake;

use crate::MainThread;

/// The editor's size when the host has no size to restore, in logical pixels.
pub const DEFAULT_SIZE: (u32, u32) = (1100, 720);
/// The smallest the editor is allowed to be, in logical pixels: below this
/// the panels no longer fit side by side.
pub const MIN_SIZE: (u32, u32) = (640, 400);

/// One open editor.
pub struct Editor {
    /// The window, once the host has given a parent to put it in. Dropping
    /// it closes it.
    window: Option<BaseviewWindow>,
    /// The size to make the window at, if the host chose one before it had a
    /// parent.
    wanted: Option<GuiSize>,
}

/// What `egui-baseview` runs: the instance's app, and the registration that
/// lets background work wake this window.
struct EditorApp {
    app: Arc<Mutex<App>>,
    /// Held for as long as the window lives.
    wake: Option<wake::Registration>,
}

impl egui_baseview::App for EditorApp {
    fn build(&mut self, ctx: egui::Context, _frame: &mut Frame) -> Result<(), HandlerError> {
        noodle_app::theme::apply(&ctx);
        // A finished job or scan wakes every open editor, this one among
        // them. A spare frame for the others costs next to nothing.
        self.wake = Some(wake::register(Arc::new(move || ctx.request_repaint())));
        Ok(())
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut Frame) {
        self.app
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .show(ui);
    }
}

/// The API this platform embeds through.
fn native_api() -> Option<GuiApiType<'static>> {
    GuiApiType::default_for_current_platform()
}

/// Whether sizes in the API are logical (macOS) rather than physical.
fn logical_sizes() -> bool {
    native_api().is_some_and(|api| api.uses_logical_size())
}

fn to_size(size: GuiSize) -> Size {
    if logical_sizes() {
        Size::Logical(LogicalSize::new(
            f64::from(size.width),
            f64::from(size.height),
        ))
    } else {
        Size::Physical(PhysicalSize::new(size.width, size.height))
    }
}

/// Keeps `size` inside what the editor can be, in the API's own units.
fn clamp(size: GuiSize, scale: f64) -> GuiSize {
    let unit = if logical_sizes() { 1.0 } else { scale };
    let at_least = |min: u32| (f64::from(min) * unit).ceil() as u32;
    GuiSize {
        width: size.width.max(at_least(MIN_SIZE.0)),
        height: size.height.max(at_least(MIN_SIZE.1)),
    }
}

impl MainThread<'_> {
    fn scale_or_one(&self) -> f64 {
        self.scale.get().unwrap_or(1.0)
    }
}

impl PluginGuiImpl for MainThread<'_> {
    fn is_api_supported(&self, configuration: GuiConfiguration) -> bool {
        !configuration.is_floating && Some(configuration.api_type) == native_api()
    }

    fn get_preferred_api(&self) -> Option<GuiConfiguration<'_>> {
        native_api().map(|api_type| GuiConfiguration {
            api_type,
            is_floating: false,
        })
    }

    fn create(&self, configuration: GuiConfiguration) -> Result<(), PluginError> {
        if !self.is_api_supported(configuration) {
            return Err(PluginError::Message("only embedded windows are supported"));
        }
        // A second create without a destroy starts again.
        self.editor.borrow_mut().take();
        *self.editor.borrow_mut() = Some(Editor {
            window: None,
            wanted: None,
        });
        Ok(())
    }

    fn destroy(&self) {
        self.editor.borrow_mut().take();
    }

    fn set_scale(&self, scale: f64) -> Result<(), PluginError> {
        if !scale.is_finite() || scale <= 0.0 {
            return Err(PluginError::Message("a scale must be positive"));
        }
        self.scale.set(Some(scale));
        if let Some(Editor {
            window: Some(window),
            ..
        }) = self.editor.borrow().as_ref()
        {
            // Used where the platform has no scale of its own to give.
            let _ = window.suggest_fallback_scale_factor(scale);
        }
        Ok(())
    }

    fn get_size(&self) -> Option<GuiSize> {
        let editor = self.editor.borrow();
        let editor = editor.as_ref()?;
        if let Some(window) = &editor.window {
            let size = window.size();
            return Some(if logical_sizes() {
                GuiSize {
                    width: size.logical.width.round() as u32,
                    height: size.logical.height.round() as u32,
                }
            } else {
                GuiSize {
                    width: size.physical.width,
                    height: size.physical.height,
                }
            });
        }
        // Before there is a window the size is the host's, or the default at
        // the host's scale.
        Some(editor.wanted.unwrap_or_else(|| {
            let unit = if logical_sizes() {
                1.0
            } else {
                self.scale_or_one()
            };
            GuiSize {
                width: (f64::from(DEFAULT_SIZE.0) * unit).round() as u32,
                height: (f64::from(DEFAULT_SIZE.1) * unit).round() as u32,
            }
        }))
    }

    fn can_resize(&self) -> bool {
        true
    }

    fn get_resize_hints(&self) -> Option<GuiResizeHints> {
        Some(GuiResizeHints {
            can_resize_horizontally: true,
            can_resize_vertically: true,
            strategy: AspectRatioStrategy::Disregard,
        })
    }

    fn adjust_size(&self, size: GuiSize) -> Option<GuiSize> {
        Some(clamp(size, self.scale_or_one()))
    }

    fn set_size(&self, size: GuiSize) -> Result<(), PluginError> {
        let size = clamp(size, self.scale_or_one());
        let mut editor = self.editor.borrow_mut();
        let editor = editor
            .as_mut()
            .ok_or(PluginError::Message("there is no editor"))?;
        match &editor.window {
            Some(window) => window
                .resize(to_size(size))
                .map_err(|_| PluginError::Message("the window can't be resized")),
            None => {
                editor.wanted = Some(size);
                Ok(())
            }
        }
    }

    fn set_parent(&self, parent: Window) -> Result<(), PluginError> {
        let mut editor = self.editor.borrow_mut();
        let editor = editor
            .as_mut()
            .ok_or(PluginError::Message("there is no editor"))?;
        if editor.window.is_some() {
            return Err(PluginError::Message("the editor already has a parent"));
        }
        let size = editor.wanted.unwrap_or(GuiSize {
            width: DEFAULT_SIZE.0,
            height: DEFAULT_SIZE.1,
        });
        let size = if editor.wanted.is_some() {
            to_size(size)
        } else {
            // The default is in logical pixels whatever the API counts in.
            Size::Logical(LogicalSize::new(
                f64::from(DEFAULT_SIZE.0),
                f64::from(DEFAULT_SIZE.1),
            ))
        };
        let mut settings = EguiWindowSettings::new()
            .with_title("Noodle")
            .with_size(size)
            .with_min_size(Some(Size::Logical(LogicalSize::new(
                f64::from(MIN_SIZE.0),
                f64::from(MIN_SIZE.1),
            ))))
            .with_resizable(true)
            .with_parent(&parent);
        if let Some(scale) = self.scale.get() {
            settings = settings.with_fallback_scale_factor(scale);
        }
        let app = EditorApp {
            app: Arc::clone(&self.app),
            wake: None,
        };
        let window = EguiWindow::create(settings, app)
            .map_err(|_| PluginError::Message("couldn't open the editor's window"))?;
        editor.window = Some(window);
        Ok(())
    }

    fn set_transient(&self, _window: Window) -> Result<(), PluginError> {
        Err(PluginError::Message("floating windows are not supported"))
    }

    fn show(&self) -> Result<(), PluginError> {
        match self
            .editor
            .borrow()
            .as_ref()
            .and_then(|e| e.window.as_ref())
        {
            Some(window) => window
                .show()
                .map_err(|_| PluginError::Message("couldn't show the editor")),
            None => Err(PluginError::Message("the editor has no window yet")),
        }
    }

    fn hide(&self) -> Result<(), PluginError> {
        match self
            .editor
            .borrow()
            .as_ref()
            .and_then(|e| e.window.as_ref())
        {
            Some(window) => window
                .hide()
                .map_err(|_| PluginError::Message("couldn't hide the editor")),
            None => Ok(()),
        }
    }
}
