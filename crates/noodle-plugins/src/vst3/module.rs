//! Loading a VST3 module: finding the binary in the bundle, running its entry
//! point and getting the plugin factory.

use std::collections::HashMap;
#[cfg(not(windows))]
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use vst3::Steinberg::*;
use vst3::{ComPtr, ComRef};

use crate::host::HostError;

use super::classes::cstring;

/// A loaded module. Its factory, and so every plugin made from it, must go
/// before the library's code is unloaded, so plugins hold an `Arc<Module>`.
pub(crate) struct Module {
    // Dropped (in `Drop`) before `library`.
    factory: Option<ComPtr<IPluginFactory>>,
    exit: Option<unsafe extern "C" fn() -> bool>,
    path: PathBuf,
    library: libloading::Library,
    #[cfg(target_os = "macos")]
    _bundle: core_foundation::bundle::CFBundle,
}

// SAFETY: the factory is only used to create plugins, which VST3 allows from
// any thread.
unsafe impl Send for Module {}
unsafe impl Sync for Module {}

/// What the factory says about one of its classes.
pub(crate) struct ClassInfo {
    pub cid: TUID,
    pub name: String,
    pub vendor: String,
    /// `Fx|Reverb` or `Instrument|Synth`.
    pub sub_categories: String,
}

/// The category of the classes that are plugins (as against their
/// controllers, which a factory also lists).
const AUDIO_CLASS: &str = "Audio Module Class";

impl Module {
    /// The module at `path`, shared with anyone else who has it loaded: the
    /// entry point runs once however many plugins come from it.
    pub fn load(path: &Path) -> Result<Arc<Module>, HostError> {
        static LOADED: Mutex<Option<HashMap<PathBuf, Weak<Module>>>> = Mutex::new(None);
        let mut loaded = LOADED.lock().unwrap_or_else(PoisonError::into_inner);
        let map = loaded.get_or_insert_with(HashMap::new);
        map.retain(|_, weak| weak.strong_count() > 0);
        if let Some(module) = map.get(path).and_then(Weak::upgrade) {
            return Ok(module);
        }
        let module = Arc::new(Self::open(path)?);
        map.insert(path.to_owned(), Arc::downgrade(&module));
        Ok(module)
    }

    fn open(path: &Path) -> Result<Module, HostError> {
        let fail = |what: String| HostError(format!("can't load {}: {what}", path.display()));
        #[cfg(target_os = "macos")]
        let (bundle, binary) = {
            use core_foundation::bundle::CFBundle;
            use core_foundation::url::CFURL;
            let url = CFURL::from_path(path, true).ok_or_else(|| fail("bad path".into()))?;
            let bundle = CFBundle::new(url).ok_or_else(|| fail("not a bundle".into()))?;
            let binary = bundle
                .executable_url()
                .and_then(|url| url.to_path())
                .ok_or_else(|| fail("the bundle has no executable".into()))?;
            (bundle, binary)
        };
        #[cfg(not(target_os = "macos"))]
        let binary = binary_in(path).ok_or_else(|| fail("no binary in the bundle".into()))?;

        // SAFETY: loading a plugin runs its code. That's what the user asked
        // for by choosing it; scanning does it in a child process for that
        // reason.
        #[cfg(target_os = "linux")]
        let (library, handle) = {
            let raw = unsafe { libloading::os::unix::Library::new(&binary) }
                .map_err(|e| fail(e.to_string()))?
                .into_raw();
            // SAFETY: `raw` is the handle just taken out of a library.
            let library = unsafe { libloading::os::unix::Library::from_raw(raw) };
            (libloading::Library::from(library), raw)
        };
        #[cfg(not(target_os = "linux"))]
        let library =
            unsafe { libloading::Library::new(&binary) }.map_err(|e| fail(e.to_string()))?;

        // SAFETY: these are the VST3 module entry points, with these
        // signatures, on each platform.
        let exit = unsafe {
            #[cfg(target_os = "linux")]
            let entry = library
                .get::<unsafe extern "C" fn(*mut c_void) -> bool>(b"ModuleEntry\0")
                .map(|f| (*f)(handle));
            #[cfg(target_os = "macos")]
            let entry = {
                use core_foundation::base::TCFType;
                // The standard spells it `bundleEntry`; some plugins capitalise it.
                library
                    .get::<unsafe extern "C" fn(*mut c_void) -> bool>(b"bundleEntry\0")
                    .or_else(|_| {
                        library.get::<unsafe extern "C" fn(*mut c_void) -> bool>(b"BundleEntry\0")
                    })
                    .map(|f| (*f)(bundle.as_concrete_TypeRef() as *mut c_void))
            };
            #[cfg(windows)]
            let entry = library
                .get::<unsafe extern "C" fn() -> bool>(b"InitDll\0")
                .map(|f| (*f)());
            // A module without the entry point is tolerated; one that has it
            // and refuses isn't.
            if let Ok(false) = entry {
                return Err(fail("the module's entry point refused to start".into()));
            }
            #[cfg(target_os = "linux")]
            let exit = library.get::<unsafe extern "C" fn() -> bool>(b"ModuleExit\0");
            #[cfg(target_os = "macos")]
            let exit = library
                .get::<unsafe extern "C" fn() -> bool>(b"bundleExit\0")
                .or_else(|_| library.get::<unsafe extern "C" fn() -> bool>(b"BundleExit\0"));
            #[cfg(windows)]
            let exit = library.get::<unsafe extern "C" fn() -> bool>(b"ExitDll\0");
            exit.ok().map(|f| *f)
        };

        // SAFETY: `GetPluginFactory` returns a factory with one reference
        // owned by the caller.
        let factory = unsafe {
            library
                .get::<unsafe extern "C" fn() -> *mut IPluginFactory>(b"GetPluginFactory\0")
                .map_err(|_| fail("not a VST3 plugin: no GetPluginFactory".into()))
                .map(|f| (*f)())
        };
        let factory = factory.and_then(|ptr| {
            // SAFETY: as above.
            unsafe { ComPtr::from_raw(ptr) }.ok_or_else(|| fail("the module has no factory".into()))
        });
        let factory = match factory {
            Ok(factory) => factory,
            Err(error) => {
                if let Some(exit) = exit {
                    // SAFETY: the matching exit for the entry run above.
                    unsafe { exit() };
                }
                return Err(error);
            }
        };
        Ok(Module {
            factory: Some(factory),
            exit,
            path: path.to_owned(),
            library,
            #[cfg(target_os = "macos")]
            _bundle: bundle,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn factory(&self) -> ComRef<'_, IPluginFactory> {
        self.factory
            .as_ref()
            .expect("present until dropped")
            .as_com_ref()
    }

    /// The classes that are audio plugins.
    pub fn classes(&self) -> Vec<ClassInfo> {
        let factory = self.factory();
        let vendor = {
            let mut info: PFactoryInfo = unsafe { std::mem::zeroed() };
            // SAFETY: a plain call into the plugin's factory with a valid
            // out-pointer.
            match unsafe { factory.getFactoryInfo(&mut info) } {
                r if r == kResultOk => cstring(&info.vendor),
                _ => String::new(),
            }
        };
        let factory2 = factory.cast::<IPluginFactory2>();
        let count = unsafe { factory.countClasses() };
        let mut found = Vec::new();
        for index in 0..count {
            let (cid, category, name, vendor_of, sub_categories) = if let Some(f2) = &factory2 {
                let mut info: PClassInfo2 = unsafe { std::mem::zeroed() };
                if unsafe { f2.getClassInfo2(index, &mut info) } != kResultOk {
                    continue;
                }
                (
                    info.cid,
                    cstring(&info.category),
                    cstring(&info.name),
                    cstring(&info.vendor),
                    cstring(&info.subCategories),
                )
            } else {
                let mut info: PClassInfo = unsafe { std::mem::zeroed() };
                if unsafe { factory.getClassInfo(index, &mut info) } != kResultOk {
                    continue;
                }
                (
                    info.cid,
                    cstring(&info.category),
                    cstring(&info.name),
                    String::new(),
                    String::new(),
                )
            };
            if category != AUDIO_CLASS {
                continue;
            }
            found.push(ClassInfo {
                cid,
                name,
                vendor: if vendor_of.is_empty() {
                    vendor.clone()
                } else {
                    vendor_of
                },
                sub_categories,
            });
        }
        found
    }
}

impl Drop for Module {
    fn drop(&mut self) {
        // The factory first, then the module's exit, then the library.
        self.factory = None;
        if let Some(exit) = self.exit {
            // SAFETY: the matching exit for the entry run in `open`.
            unsafe { exit() };
        }
        let _ = &self.library;
    }
}

/// The binary inside a bundle (or the file itself, for a plain module).
#[cfg(not(target_os = "macos"))]
fn binary_in(path: &Path) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.to_owned());
    }
    let arch = if cfg!(target_os = "linux") {
        if cfg!(target_arch = "aarch64") {
            "aarch64-linux"
        } else {
            "x86_64-linux"
        }
    } else if cfg!(target_arch = "aarch64") {
        "arm64-win"
    } else {
        "x86_64-win"
    };
    let dir = path.join("Contents").join(arch);
    let extension = if cfg!(target_os = "linux") {
        "so"
    } else {
        "vst3"
    };
    let stem = path.file_stem()?;
    let named = dir.join(stem).with_extension(extension);
    if named.is_file() {
        return Some(named);
    }
    // A bundle whose binary has another name: the only file of that kind.
    std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|p| p.extension().is_some_and(|ext| ext == extension))
}
