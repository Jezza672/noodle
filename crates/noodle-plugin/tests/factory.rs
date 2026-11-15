//! What `clap-wrapper` reads from the built plugin to make the Audio Units:
//! each plugin's type and subtype, one pair apiece, and nothing for a third.

mod common;

use clack_extensions::clap_wrapper::auv2::PluginAsAuv2Factory;
use clack_host::prelude::*;
use common::plugin_path;

#[test]
fn the_plugin_tells_the_wrapper_the_audio_unit_codes() {
    // SAFETY: the plugin is ours, and built by `plugin_path`.
    let entry = unsafe { PluginEntry::load(plugin_path()) }.expect("load the plugin");
    std::mem::forget(entry.clone());
    let factory = entry
        .get_factory::<PluginAsAuv2Factory>()
        .expect("the AUv2 info factory");
    let codes = |index| {
        factory
            .auv2_info(index)
            .map(|info| (info.au_type().to_vec(), info.au_subt().to_vec()))
    };
    assert_eq!(codes(0), Some((b"aufx\0".to_vec(), b"Nefx\0".to_vec())));
    assert_eq!(codes(1), Some((b"aumu\0".to_vec(), b"Nins\0".to_vec())));
    assert_eq!(codes(2), None);
}
