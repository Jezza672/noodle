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

#[cfg(test)]
thread_local! {
    static ANSWER: std::cell::RefCell<Option<Vec<PathBuf>>> = const { std::cell::RefCell::new(None) };
    static ASKED: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Queues what the next picker returns. An empty list is a cancel.
#[cfg(test)]
pub fn answer(paths: Vec<PathBuf>) {
    ANSWER.with(|a| *a.borrow_mut() = Some(paths));
}

/// What the last unanswered picker was asking for, once.
#[cfg(test)]
pub fn take_asked() -> Option<String> {
    ASKED.with(|a| a.borrow_mut().take())
}

#[cfg(test)]
fn ask(what: &str) -> Option<Vec<PathBuf>> {
    match ANSWER.with(|a| a.borrow_mut().take()) {
        Some(paths) if paths.is_empty() => None,
        Some(paths) => Some(paths),
        None => {
            ASKED.with(|a| *a.borrow_mut() = Some(what.to_owned()));
            None
        }
    }
}

pub fn pick_folder() -> Option<PathBuf> {
    #[cfg(test)]
    return ask("choose a folder").and_then(|p| p.into_iter().next());
    #[cfg(not(test))]
    rfd::FileDialog::new().pick_folder()
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
