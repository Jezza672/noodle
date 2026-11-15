// The CLAP entry, made from the three functions the Rust static library
// exports (`noodle-plugin` built with `--features wrapper`). clap-wrapper
// compiles this once per format and puts it behind that format's entry.

#include <clap/clap.h>

extern "C"
{
  bool clap_init(const char *plugin_path);
  void clap_deinit();
  const void *clap_getfactory(const char *factory_id);

  CLAP_EXPORT const clap_plugin_entry clap_entry = {CLAP_VERSION, clap_init, clap_deinit,
                                                    clap_getfactory};
}
