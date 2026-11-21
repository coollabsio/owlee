use std::process::ExitCode;

use owlee_platform::{TracingFormat, init_tracing};

// musl's malloc serializes threads on one lock and slows the static release binary.
#[cfg(target_env = "musl")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> ExitCode {
    let environment = std::env::var("OWLEE_ENV");
    let format = match environment.as_deref() {
        Ok("development") => TracingFormat::Compact,
        _ => TracingFormat::Json,
    };
    let _ = init_tracing(format);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    match runtime.block_on(owlee_server::run()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("owlee: {error}");
            ExitCode::FAILURE
        }
    }
}
