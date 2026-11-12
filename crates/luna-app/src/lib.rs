slint::include_modules!();

mod app;

pub use app::run;

#[cfg(target_os = "android")]
#[no_mangle]
pub fn android_main(app: slint::android::AndroidApp) {
    let data_dir = app.internal_data_path();
    if let Err(error) = slint::android::init(app) {
        eprintln!("failed to initialize the Slint Android backend: {error}");
        return;
    }
    if let Err(error) = app::run(data_dir) {
        eprintln!("luna app failed: {error}");
    }
}
