//! An editor window on Windows: a top-level window whose handle is the
//! plugin's parent. Made on the thread that pumps messages for it (the app's
//! UI thread), which is where it is used.

use std::sync::Once;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{COLOR_WINDOW, HBRUSH};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GWL_STYLE, GetClientRect, GetWindowLongPtrW, IDC_ARROW, IsIconic, IsWindowVisible, LoadCursorW,
    MSG, PM_REMOVE, PeekMessageW, RegisterClassW, SW_HIDE, SW_SHOW, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, TranslateMessage, WM_CLOSE, WNDCLASSW, WS_CLIPCHILDREN, WS_MAXIMIZEBOX,
    WS_OVERLAPPEDWINDOW, WS_THICKFRAME,
};

use super::{EditorWindow, ParentWindow};

const CLASS: &str = "NoodleEditorHost";

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The close button hides the window (the default destroys it, which would
/// pull it out from under the plugin).
unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_CLOSE {
        // SAFETY: called by the system with a live window.
        unsafe { ShowWindow(hwnd, SW_HIDE) };
        return 0;
    }
    // SAFETY: as above, passing the system's arguments on.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

fn register_class() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let name = wide(CLASS);
        // SAFETY: a window class made of valid pointers that outlive the call.
        unsafe {
            let class = WNDCLASSW {
                style: 0,
                lpfnWndProc: Some(window_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: GetModuleHandleW(std::ptr::null()),
                hIcon: std::ptr::null_mut(),
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                hbrBackground: (COLOR_WINDOW + 1) as HBRUSH,
                lpszMenuName: std::ptr::null(),
                lpszClassName: name.as_ptr(),
            };
            RegisterClassW(&class);
        }
    });
}

/// Dispatches the messages waiting for this thread's windows, for a program
/// with no message loop of its own (the plugin host process).
pub(super) fn pump_events() {
    // SAFETY: a message record on the stack, the thread's own queue; a burst
    // is bounded so a flood of messages can't starve the caller.
    unsafe {
        let mut message: MSG = std::mem::zeroed();
        for _ in 0..256 {
            if PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) == 0 {
                break;
            }
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

pub(super) struct WinWindow {
    hwnd: HWND,
    shown: bool,
}

pub(super) fn make(title: &str) -> Option<Box<dyn EditorWindow>> {
    register_class();
    let class = wide(CLASS);
    let title = wide(title);
    // SAFETY: valid, NUL-terminated strings; no parent, menu or extra data.
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            100,
            100,
            420,
            340,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        )
    };
    (!hwnd.is_null()).then(|| Box::new(WinWindow { hwnd, shown: false }) as Box<dyn EditorWindow>)
}

impl EditorWindow for WinWindow {
    fn parent(&self) -> ParentWindow {
        ParentWindow::Win32(self.hwnd)
    }

    fn set_size(&mut self, width: u32, height: u32, resizable: bool) {
        // Change only the resizing bits: the rest of the style word is state
        // (visible, minimized) that setting a whole new word would wipe.
        // SAFETY: our own window.
        let current = unsafe { GetWindowLongPtrW(self.hwnd, GWL_STYLE) } as u32;
        let resizing = WS_THICKFRAME | WS_MAXIMIZEBOX;
        let style = if resizable {
            current | resizing
        } else {
            current & !resizing
        };
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        };
        // SAFETY: our own window and a rectangle on the stack.
        unsafe {
            SetWindowLongPtrW(self.hwnd, GWL_STYLE, style as isize);
            AdjustWindowRectEx(&mut rect, style, 0, 0);
            SetWindowPos(
                self.hwnd,
                std::ptr::null_mut(),
                0,
                0,
                rect.right - rect.left,
                rect.bottom - rect.top,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
        }
    }

    fn size(&self) -> (u32, u32) {
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        // SAFETY: our own window and a rectangle on the stack.
        if unsafe { IsIconic(self.hwnd) } != 0 {
            // A minimized window has no client area to speak of.
            return (0, 0);
        }
        // SAFETY: as above.
        unsafe { GetClientRect(self.hwnd, &mut rect) };
        (rect.right.max(0) as u32, rect.bottom.max(0) as u32)
    }

    fn show(&mut self) {
        // SAFETY: our own window.
        unsafe {
            ShowWindow(self.hwnd, SW_SHOW);
            SetForegroundWindow(self.hwnd);
        }
        self.shown = true;
    }

    fn hide(&mut self) {
        // SAFETY: our own window.
        unsafe { ShowWindow(self.hwnd, SW_HIDE) };
        self.shown = false;
    }

    fn is_closed(&self) -> bool {
        // The close button hides the window; minimizing doesn't.
        // SAFETY: our own window.
        self.shown && unsafe { IsWindowVisible(self.hwnd) } == 0
    }
}

impl Drop for WinWindow {
    fn drop(&mut self) {
        // SAFETY: our own window, destroyed once, after the plugin's view.
        unsafe { DestroyWindow(self.hwnd) };
    }
}
