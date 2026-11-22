use std::process::ExitCode;

fn main() -> ExitCode {
    process_memory::configure_allocator();
    if let Some(result) = rufin_controller::discovery_worker_argument() {
        return result;
    }
    #[cfg(unix)]
    playback_gstreamer::force_http1();
    rufin_controller::run(std::env::args_os().skip(1))
}
