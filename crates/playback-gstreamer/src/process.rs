use std::env;

/// Establish libsoup's process setting. Both executables call this first in `main`.
pub fn force_http1() {
    if env::var_os("SOUP_FORCE_HTTP1").is_some() {
        return;
    }
    // SAFETY: No other thread exists yet to read or change the environment.
    #[allow(unsafe_code)]
    unsafe {
        env::set_var("SOUP_FORCE_HTTP1", "1");
    }
}
