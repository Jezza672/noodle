//! The native file pickers, behind one seam so a test (or the new-user
//! agent driver) can answer them.
//!
//! In the app these are `rfd`'s dialogs. In a test build nothing opens:
//! a picker returns the answer queued with [`answer`], or `None` and
//! records that one was asked for, which [`take_asked`] reads. The agent
//! driver tells the agent a picker opened and lets it choose a path, as a
//! person would in the dialog.

use std::path::PathBuf;

/// What kind of file a picker is for: a name and extensions.
pub type Filter<'a> = (&'a str, &'a [&'a str]);

/// A handler that answers every picker as it opens. It is given what the picker asks for and returns the paths, or
/// none to cancel. The agent driver uses it, so the action that opened the
/// picker carries on with the answer, as it does when a person picks in a
/// dialog.
#[cfg(test)]
pub type Handler = Box<dyn FnMut(&str) -> Vec<PathBuf>>;

#[cfg(test)]
thread_local! {
    static HANDLER: std::cell::RefCell<Option<Handler>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub fn set_handler(handler: Handler) {
    HANDLER.with(|h| *h.borrow_mut() = Some(handler));
}

#[cfg(test)]
fn ask(what: &str) -> Option<Vec<PathBuf>> {
    // Taken out while it runs, so a picker opened from the handler cannot
    // borrow it twice.
    if let Some(mut handler) = HANDLER.with(|h| h.borrow_mut().take()) {
        let paths = handler(what);
        HANDLER.with(|h| *h.borrow_mut() = Some(handler));
        return (!paths.is_empty()).then_some(paths);
    }
    // No handler: nothing is picked, as if the dialog were cancelled.
    let _ = what;
    None
}

pub fn pick_folder() -> Option<PathBuf> {
    pick_folder_in("", None)
}

/// A folder to pick, with a title and a place to start.
pub fn pick_folder_in(title: &str, dir: Option<&std::path::Path>) -> Option<PathBuf> {
    #[cfg(test)]
    {
        let _ = dir;
        ask(&format!("choose a folder {title}")).and_then(|p| p.into_iter().next())
    }
    #[cfg(not(test))]
    {
        let mut dialog = rfd::FileDialog::new().set_title(title);
        if let Some(dir) = dir {
            dialog = dialog.set_directory(dir);
        }
        dialog.pick_folder()
    }
}

pub fn pick_file(filter: Filter) -> Option<PathBuf> {
    #[cfg(test)]
    return ask(&format!("choose a {} file", filter.0)).and_then(|p| p.into_iter().next());
    #[cfg(not(test))]
    rfd::FileDialog::new()
        .add_filter(filter.0, filter.1)
        .pick_file()
}

pub fn pick_files(filter: Filter) -> Option<Vec<PathBuf>> {
    #[cfg(test)]
    return ask(&format!("choose one or more {} files", filter.0));
    #[cfg(not(test))]
    rfd::FileDialog::new()
        .add_filter(filter.0, filter.1)
        .pick_files()
}

/// A file to save to, offered `name`, starting in `dir` if there is one.
pub fn save_file(name: &str, filter: Filter, dir: Option<&std::path::Path>) -> Option<PathBuf> {
    #[cfg(test)]
    {
        let _ = (dir, filter);
        ask(&format!("choose where to save {name}")).and_then(|p| p.into_iter().next())
    }
    #[cfg(not(test))]
    {
        let mut dialog = rfd::FileDialog::new().set_file_name(name);
        if let Some(dir) = dir {
            dialog = dialog.set_directory(dir);
        }
        dialog.add_filter(filter.0, filter.1).save_file()
    }
}
